//! `odx aggregate` and `odx lock`: fill the git cache, build the working
//! trees and keep `odx.lock` current.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use odx_aggregate::{
    BuildOptions, Change, ChangeKind, HeadDecision, PlanError, Planned, PlannedSource,
    RepoAggregation, RepoPlan, aggregations, classify, date, decide, fetch_plans, local_clones,
    lock_changes, lock_entry, locked_entry, mergiraf_program, package_entries, package_fetch_plans,
    pin, plan_config, rewrite_packages, run_fetches, run_pipeline, strategy, update_lock,
};
use odx_config::deps::{normalize_name, read_uv_git_sources};
use odx_config::edit::PyprojectEdit;
use odx_config::lock::{
    CiState as LockCi, LockedRepository, Lockfile, MergeStatus, OdooPin, RedClassification,
};
use odx_config::packages::{PackageSpec, package_specs};
use odx_config::schema::OnHeadChange;
use odx_config::schema::RefStyle;
use odx_config::user::{self, UserConfig, UserMirror, expand_home, load_user_config, mirror_for};
use odx_config::{ConfigError, OdxConfig, Project, Source, Span, discover};
use odx_core::system::OsRunner;
use odx_core::{Env, Exit, Invocation, ProcessRunner, repo_key};
use odx_forge::{
    CachedHttp, CiState, ForgeClient, ForgeError, ForgeKind, RepoUrl, ReqwestHttp, Visibility,
};
use odx_git::{
    Aggregated, Aggregation, Cache, Clones, Fetched, GitBackend, Limits, MergeError, Merger,
    Outcome, StepOutcome, SystemGit, VaultRefs, Via, bundle, url_hash, vault,
};
use std::collections::BTreeMap;
use toml_edit::DocumentMut;

use super::Report;
use super::edit::{read_lock, save};
use super::progress::{Reporter, Verbosity, to_stderr};
use super::sniff::{
    Sniffed, describe as describe_sniffed, fail_on_new, script_count, sniff_locked,
    to_json as sniffed_json,
};
use crate::Context;
use crate::cli::{AggregateArgs, GlobalArgs, LockArgs};

/// How long to wait for another odx process's repository lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// How old fetched refs may be by default (`[tool.odx.cache] ttl.refs`).
const REFS_TTL: Duration = Duration::from_mins(15);

/// The cache directory: `cache_dir` from the project configuration
/// (relative to the project root, `~/` expanded), else the shared cache
/// directory (`ODX_CACHE_DIR`, the user configuration, the platform), else
/// `.odx/cache` in the project.
pub(crate) fn cache_dir(
    config: &OdxConfig,
    user: &UserConfig,
    root: &Path,
    env: &dyn Env,
) -> PathBuf {
    config.cache_dir.as_ref().map_or_else(
        || user::cache_dir(user, env).unwrap_or_else(|| root.join(".odx").join("cache")),
        |dir| root.join(expand_home(dir, env)),
    )
}

/// The project, its lockfile and where and how to fetch.
pub(crate) struct Prepared {
    pub(crate) project: Project,
    pub(crate) lock_path: PathBuf,
    pub(crate) lock: Option<Lockfile>,
    pub(crate) cache: Cache,
    clones: Clones,
    limits: Limits,
    /// How old fetched refs may be before they are fetched again; `None`
    /// fetches every time.
    pub(crate) max_age: Option<Duration>,
    /// Never use the network for git (`--offline`).
    offline: bool,
    /// How much to say while fetching and building.
    verbosity: Verbosity,
    /// `[mirrors]` of the user configuration.
    mirrors: BTreeMap<String, UserMirror>,
    /// Register the workspace for `odx sync --all` (`[cache]
    /// register_workspaces`, and not registered yet).
    pub(crate) register: bool,
}

impl Prepared {
    /// The local clones of `[clones]` in the user configuration.
    pub(crate) const fn clones(&self) -> &Clones {
        &self.clones
    }

    pub(crate) fn locked(&self) -> &[LockedRepository] {
        self.lock.as_ref().map_or(&[], |lock| &lock.repositories)
    }

    /// The lockfile, empty when there is none, borrowed when there is.
    pub(crate) fn lock_or_default(&self) -> Cow<'_, Lockfile> {
        self.lock
            .as_ref()
            .map_or_else(|| Cow::Owned(Lockfile::default()), Cow::Borrowed)
    }
}

pub(crate) fn prepare(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Prepared, Report> {
    let project = match discover(ctx.fs, &ctx.dir, ctx.config) {
        Ok(Source::Pyproject(project)) => project,
        Ok(Source::ReposYaml { file, .. }) => {
            return Err(Report::error(
                Exit::Usage,
                format!(
                    "{}: use git-aggregate-rs (or odx git-aggregate) for repos.yaml",
                    file.display()
                ),
            ));
        }
        Err(e @ ConfigError::Io { .. }) => return Err(Report::error(Exit::Failure, e.to_string())),
        Err(e) => return Err(Report::error(Exit::Usage, e.to_string())),
    };
    let user =
        load_user_config(ctx.fs, ctx.env).map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    let lock_path = project.root.join(project.config.lockfile());
    let lock = read_lock(ctx, &lock_path)?;
    let cache = Cache::new(cache_dir(&project.config, &user, &project.root, ctx.env));
    let limits = Limits {
        jobs: global
            .jobs
            .or_else(|| project.config.jobs.map(usize::from))
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, NonZeroUsize::get)),
        per_host: project
            .config
            .http
            .as_ref()
            .and_then(|http| http.per_host_concurrency)
            .map_or(6, usize::from),
        lock_timeout: LOCK_TIMEOUT,
    };
    let max_age = if global.fresh {
        None
    } else if global.no_refresh || global.offline {
        Some(Duration::MAX)
    } else {
        Some(
            project
                .config
                .cache
                .as_ref()
                .and_then(|cache| cache.ttl.as_ref())
                .and_then(|ttl| ttl.refs)
                .map_or(REFS_TTL, Span::as_duration),
        )
    };
    let register = user
        .cache
        .as_ref()
        .and_then(|cache| cache.register_workspaces)
        .unwrap_or(false)
        && !user
            .workspaces
            .contains_key(&project.root.display().to_string());
    Ok(Prepared {
        max_age,
        offline: global.offline,
        verbosity: Verbosity::of(global),
        mirrors: user.mirrors.clone(),
        register,
        clones: local_clones(&user, ctx.env),
        project: *project,
        lock_path,
        lock,
        cache,
        limits,
    })
}

/// The `uv` executable: `ODX_UV`, else `uv` from `PATH`.
pub(crate) fn uv_program(env: &dyn Env) -> String {
    env.var("ODX_UV").unwrap_or_else(|| "uv".to_owned())
}

/// Whether odx runs in CI (`CI` set and not `false`), where locked commits
/// come from the vault first.
fn in_ci(env: &dyn Env) -> bool {
    env.var("CI")
        .is_some_and(|value| !value.is_empty() && value != "false" && value != "0")
}

/// What a fetch-and-build run produced.
struct Built {
    plans: Vec<RepoPlan>,
    outcomes: Vec<Outcome>,
    aggregations: Vec<Result<RepoAggregation, PlanError>>,
    results: Vec<Result<Aggregated, MergeError>>,
}

impl Built {
    /// The repositories that were built, with what was built and the result.
    fn pairs<'a>(
        &'a self,
        planned: &'a [Planned],
    ) -> impl Iterator<
        Item = (
            &'a Planned,
            &'a Aggregation,
            &'a Result<Aggregated, MergeError>,
        ),
    > {
        planned
            .iter()
            .zip(&self.aggregations)
            .filter_map(|(planned, built)| built.as_ref().ok().map(|b| (planned, &b.aggregation)))
            .zip(&self.results)
            .map(|((planned, aggregation), result)| (planned, aggregation, result))
    }
}

/// Fetch everything `planned` needs, then build it (unless `merger` is
/// `None`).
fn build(
    prepared: &Prepared,
    planned: &[Planned],
    options: &BuildOptions,
    merger: Option<&Arc<Merger>>,
    prefer_mirror: bool,
    bundles: Option<&Path>,
) -> Built {
    let vault = prepared
        .project
        .config
        .mirror
        .as_ref()
        .map(|mirror| mirror.url.as_str());
    let mut plans = fetch_plans(planned, vault.filter(|_| prefer_mirror), vault);
    for plan in &mut plans {
        plan.plan.clone = prepared.clones.find(&plan.plan.url).cloned();
        plan.plan.max_age = prepared.max_age;
        plan.plan.offline = prepared.offline;
        if let Some(mirror) = mirror_for(&prepared.mirrors, &plan.plan.url) {
            plan.plan.fallback.get_or_insert_with(|| mirror.url.clone());
            if mirror.prefer.unwrap_or(false) {
                plan.plan.mirror.get_or_insert_with(|| mirror.url.clone());
            }
        }
        // A bundle of the locked commits comes before any network.
        let bundle = bundles.map(|dir| dir.join(format!("{}.bundle", url_hash(&plan.plan.url))));
        if let Some(bundle) = bundle.filter(|bundle| bundle.is_file()) {
            plan.plan.mirror = Some(bundle.display().to_string());
        }
    }
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::new(OsRunner)));
    let aggregated = std::cell::RefCell::new(Vec::new());
    let plan_builds = |outcomes: &[Outcome]| {
        let fetched: Vec<(&str, &[Fetched])> = outcomes
            .iter()
            .filter_map(|o| {
                o.result
                    .as_ref()
                    .ok()
                    .map(|f| (o.url.as_str(), f.as_slice()))
            })
            .collect();
        let repos = aggregations(planned, &prepared.cache, &fetched, options);
        let runnable: Vec<Aggregation> = repos
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|r| r.aggregation.clone()))
            .collect();
        *aggregated.borrow_mut() = repos;
        runnable
    };
    let reporter = Reporter::new(prepared.verbosity, planned, &to_stderr);
    let (outcomes, results) = run_pipeline(
        &prepared.cache,
        &backend,
        plans.iter().map(|p| p.plan.clone()).collect(),
        prepared.limits,
        merger,
        &plan_builds,
        &|event| reporter.hear(event),
    );
    Built {
        plans,
        outcomes,
        aggregations: aggregated.into_inner(),
        results,
    }
}

/// Lock entries for what was built from unpinned sources; failures to
/// compute one are returned as messages.
fn lock_entries(
    planned: &[Planned],
    built: &Built,
    merger: &Merger,
) -> (Vec<LockedRepository>, Vec<String>) {
    let (entries, failures): (Vec<_>, Vec<_>) = built
        .pairs(planned)
        .filter(|(repo, _, result)| !repo.pinned && result.is_ok())
        .filter_map(|(repo, aggregation, result)| {
            result
                .as_ref()
                .ok()
                .map(|result| lock_entry(repo, aggregation, result, merger))
        })
        .partition(Result::is_ok);
    (
        entries.into_iter().filter_map(Result::ok).collect(),
        failures
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<PlanError>>()
            .iter()
            .map(ToString::to_string)
            .collect(),
    )
}

/// The lockfile with `entries`; without `--repo`, entries of repositories
/// no longer configured are dropped.
fn next_lock(prepared: &Prepared, entries: Vec<LockedRepository>, only: Option<&str>) -> Lockfile {
    let mut lock = prepared.lock.clone().unwrap_or_default();
    let names: Vec<String> = prepared
        .project
        .config
        .repositories
        .iter()
        .map(|repo| repo.name.clone().unwrap_or_else(|| repo.url.clone()))
        .collect();
    update_lock(
        &mut lock,
        entries,
        only.is_none().then_some(names.as_slice()),
    );
    lock
}

pub(crate) fn write_lock(
    ctx: &Context<'_>,
    prepared: &Prepared,
    lock: &Lockfile,
) -> Result<(), Report> {
    ctx.fs
        .write(&prepared.lock_path, lock.render().as_bytes())
        .map_err(|e| {
            Report::error(
                Exit::Failure,
                format!("{}: {e}", prepared.lock_path.display()),
            )
        })
}

/// The merger and the build options from the project configuration.
fn merger_and_options(
    config: &OdxConfig,
    ctx: &Context<'_>,
    keep_conflicts: bool,
    force: bool,
) -> Result<(Arc<Merger>, BuildOptions), Report> {
    let runner = Arc::new(OsRunner);
    let mergiraf = mergiraf_program(
        config.mergiraf.unwrap_or_default(),
        ctx.env,
        runner.as_ref(),
    )
    .map_err(|e| Report::error(Exit::Usage, e))?;
    Ok((
        Arc::new(Merger::new(runner)),
        BuildOptions {
            strategy: strategy(config.merge_strategy.unwrap_or_default()),
            mergiraf,
            keep_conflicts,
            force,
        },
    ))
}

/// `odx aggregate [--fetch-only] [--repo NAME] [--prefer-mirror] [--keep-conflicts] [--force] [--locked | --update]`.
pub(crate) fn run_aggregate(
    args: &AggregateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    aggregate(args, global, ctx).unwrap_or_else(|report| report)
}

fn aggregate(
    args: &AggregateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let mut prepared = prepare(global, ctx)?;
    if args.update {
        // Updating means taking what upstream has now.
        prepared.max_age = None;
    }
    super::sync::auto_register(&prepared, ctx);
    let mut planned = plan_config(
        &prepared.project.config,
        &prepared.project.root,
        args.repo.as_deref(),
    )
    .map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    if !args.update {
        for repo in &mut planned {
            pin(repo, prepared.locked());
        }
    }
    if args.locked {
        let stale: Vec<&str> = planned
            .iter()
            .filter(|repo| !repo.pinned)
            .map(|repo| repo.name.as_str())
            .collect();
        if !stale.is_empty() {
            return Err(Report::error(
                Exit::Failure,
                format!(
                    "{} does not lock {} as configured; run `odx lock`",
                    prepared.lock_path.display(),
                    stale.join(", ")
                ),
            ));
        }
    }
    let (merger, options) = merger_and_options(
        &prepared.project.config,
        ctx,
        args.keep_conflicts,
        args.force,
    )?;
    // Working trees that already are what odx.lock describes need nothing.
    let (current, planned): (Vec<Planned>, Vec<Planned>) = planned.into_iter().partition(|repo| {
        !args.fetch_only
            && repo.pinned
            && prepared.locked().iter().any(|entry| {
                entry.name == repo.name
                    && merger.is_current(
                        &repo.target_dir,
                        &prepared.cache.repo_dir(&repo.url),
                        &entry.result_tree,
                    )
            })
    });
    let built = build(
        &prepared,
        &planned,
        &options,
        (!args.fetch_only).then_some(&merger),
        args.prefer_mirror || in_ci(ctx.env),
        args.bundle_dir
            .as_ref()
            .map(|dir| ctx.dir.join(dir))
            .as_deref(),
    );
    let mut summary = Summary::new(&built, &planned, &prepared.project.root, false);
    summary.current = current
        .iter()
        .map(|repo| {
            (
                repo.name.clone(),
                relative(&repo.target_dir, &prepared.project.root),
            )
        })
        .collect();
    let mut locked = Cow::Borrowed(prepared.locked());
    if !args.fetch_only && !args.locked {
        let (entries, failures) = lock_entries(&planned, &built, &merger);
        summary.failures.extend(failures);
        locked = Cow::Owned(entries.clone());
        let lock = next_lock(&prepared, entries, args.repo.as_deref());
        let previous = prepared.lock_or_default();
        summary.changes = lock_changes(&previous, &lock);
        if *previous != lock {
            write_lock(ctx, &prepared, &lock)?;
        }
    }
    if !args.fetch_only {
        sniff_built(&prepared, &merger, &locked, &mut summary);
    }
    Ok(summary.report(global.json, args.fetch_only, prepared.project.warnings))
}

/// The migration scripts the merges of every repository built in this run
/// bring on top of its base; with `[tool.odx.sniff] fail_on_new`, any
/// script fails the run.
fn sniff_built(
    prepared: &Prepared,
    merger: &Merger,
    locked: &[LockedRepository],
    summary: &mut Summary,
) {
    let built: Vec<&LockedRepository> = locked
        .iter()
        .filter(|entry| {
            summary
                .builds
                .iter()
                .any(|(name, _, result)| *name == entry.name && result.is_ok())
        })
        .collect();
    let mut problems = Vec::new();
    summary.sniffed = sniff_locked(merger, &prepared.cache, &built, &mut problems);
    summary.notes.append(&mut problems);
    let scripts = script_count(&summary.sniffed);
    if scripts > 0 && fail_on_new(&prepared.project.config) {
        summary
            .failures
            .push(format!("{scripts} migration scripts (fail_on_new)"));
    }
}

/// `odx lock [--repo NAME] [--check | --update]`.
pub(crate) fn run_lock(args: &LockArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    lock(args, global, ctx).unwrap_or_else(|report| report)
}

fn lock(args: &LockArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    if args.auto_update {
        return super::autoupdate::auto_update(global, ctx);
    }
    if args.migrate {
        return migrate(ctx);
    }
    let mut prepared = prepare(global, ctx)?;
    // A lock records what upstream has now, never what was cached earlier.
    prepared.max_age = None;
    super::sync::auto_register(&prepared, ctx);
    let mut planned = plan_config(
        &prepared.project.config,
        &prepared.project.root,
        args.repo.as_deref(),
    )
    .map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    if !args.update {
        for repo in &mut planned {
            pin(repo, prepared.locked());
        }
    }
    // Pinned repositories keep their entries; the others are built in
    // scratch working trees inside the cache, never in the project.
    let mut planned: Vec<Planned> = planned.into_iter().filter(|repo| !repo.pinned).collect();
    in_scratch_dirs(&prepared.cache, &mut planned);
    let (merger, options) = merger_and_options(&prepared.project.config, ctx, false, true)?;
    let mut warnings = prepared.project.warnings.clone();
    let kept = if args.update && prepared.lock.is_some() {
        // Resolve the heads first: the guard rail decides which to adopt.
        let resolved = build(&prepared, &planned, &options, None, true, None);
        let fetched: Vec<(&str, &[Fetched])> = resolved
            .outcomes
            .iter()
            .filter_map(|o| {
                o.result
                    .as_ref()
                    .ok()
                    .map(|f| (o.url.as_str(), f.as_slice()))
            })
            .collect();
        guard(
            ctx,
            &prepared,
            &mut planned,
            &fetched,
            &args.follow,
            &merger,
            &mut warnings,
        )
    } else {
        Vec::new()
    };
    let built = build(&prepared, &planned, &options, Some(&merger), true, None);
    let mut summary = Summary::new(&built, &planned, &prepared.project.root, true);
    let (mut entries, failures) = lock_entries(&planned, &built, &merger);
    record_kept(&mut entries, &kept, prepared.locked());
    summary.kept = kept;
    summary.failures.extend(failures);
    let mut lock = next_lock(&prepared, entries, args.repo.as_deref());
    // Packages and the Odoo core belong to the whole project, not to a
    // repository: `--repo` leaves them as they are.
    let packages = if args.repo.is_none() {
        Some(lock_packages(
            &prepared,
            args.update,
            &mut lock,
            &mut summary,
            &mut warnings,
        )?)
    } else {
        None
    };
    let previous = prepared.lock_or_default();
    summary.changes = lock_changes(&previous, &lock);
    let outdated = *previous != lock;
    if args.check {
        if outdated {
            summary.failures.push(format!(
                "{} is not up to date; run `odx lock`",
                prepared.lock_path.display()
            ));
        }
    } else {
        if !args.no_push {
            vault_lock(ctx, &prepared, &mut lock, &mut summary, &mut warnings)?;
            if let Some((specs, doc)) = packages {
                point_packages_at_the_vault(
                    ctx,
                    &prepared,
                    args.no_uv,
                    &specs,
                    doc,
                    &lock,
                    &mut summary,
                )?;
            }
        }
        if let Some(dir) = &args.bundle {
            bundle_lock(&prepared, &lock, &ctx.dir.join(dir), &mut summary);
        }
        if *previous != lock {
            write_lock(ctx, &prepared, &lock)?;
        }
        warnings.extend(super::glue::hint(&prepared));
    }
    Ok(summary.report(global.json, false, warnings))
}

/// `odx lock --migrate`: rewrite `odx.lock` in the current format.
fn migrate(ctx: &Context<'_>) -> Result<Report, Report> {
    let project = match discover(ctx.fs, &ctx.dir, ctx.config) {
        Ok(Source::Pyproject(project)) => project,
        Ok(Source::ReposYaml { file, .. }) => {
            return Err(Report::error(
                Exit::Usage,
                format!("{}: repos.yaml projects have no odx.lock", file.display()),
            ));
        }
        Err(e) => return Err(Report::error(Exit::Usage, e.to_string())),
    };
    let path = project.root.join(project.config.lockfile());
    let text = ctx
        .fs
        .read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", path.display())))?;
    let lock = Lockfile::migrate(&text)
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", path.display())))?;
    let rendered = lock.render();
    if rendered == text {
        return Ok(Report {
            out: format!("{} is already in the current format\n", path.display()),
            ..Report::default()
        });
    }
    ctx.fs
        .write(&path, rendered.as_bytes())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", path.display())))?;
    Ok(Report {
        out: format!("rewrote {} in the current format\n", path.display()),
        ..Report::default()
    })
}

/// `odx lock --bundle`: one git bundle per repository URL with every
/// locked commit of it, named after the URL's hash (no project or client
/// name).
fn bundle_lock(prepared: &Prepared, lock: &Lockfile, dir: &Path, summary: &mut Summary) {
    let mut by_url: Vec<(String, Vec<String>)> = Vec::new();
    let commits = lock
        .repositories
        .iter()
        .flat_map(|entry| {
            std::iter::once(&entry.base_sha)
                .chain(entry.merges.iter().map(|m| &m.sha))
                .map(move |sha| (&entry.url, sha))
        })
        .chain(
            lock.packages
                .values()
                .map(|package| (&package.url, &package.sha)),
        );
    for (url, sha) in commits {
        let key = repo_key(url);
        match by_url.iter_mut().find(|(known, _)| repo_key(known) == key) {
            Some((_, shas)) if !shas.contains(sha) => shas.push(sha.clone()),
            Some(_) => {}
            None => by_url.push((url.clone(), vec![sha.clone()])),
        }
    }
    let git = SystemGit::new(Arc::new(OsRunner));
    for (url, shas) in by_url {
        let file = dir.join(format!("{}.bundle", url_hash(&url)));
        match bundle(&git, &prepared.cache.repo_dir(&url), &shas, &file) {
            Ok(()) => summary
                .bundled
                .push((url, shas.len(), file.display().to_string())),
            Err(e) => summary
                .failures
                .push(format!("{url}: bundling failed: {e}")),
        }
    }
}

/// Build in scratch working trees inside the cache, never in the project.
fn in_scratch_dirs(cache: &Cache, planned: &mut [Planned]) {
    for repo in planned.iter_mut() {
        let name: String = repo
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        repo.target_dir = cache
            .root()
            .join("locks")
            .join(format!("{}-{name}", url_hash(&repo.url)));
    }
}

/// Resolve the project's git packages (pinned by `odx.lock` unless
/// `update`) and its Odoo core source into `lock`; returns the packages and
/// the parsed `pyproject.toml` for rewriting.
fn lock_packages(
    prepared: &Prepared,
    update: bool,
    lock: &mut Lockfile,
    summary: &mut Summary,
    warnings: &mut Vec<odx_config::Problem>,
) -> Result<(Vec<PackageSpec>, PyprojectEdit), Report> {
    // The text was read and validated when the project was loaded.
    let edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let doc = edit.document();
    let previous = prepared.lock_or_default();
    let none = BTreeMap::new();
    let pinned = if update { &none } else { &previous.packages };
    let (specs, problems) = package_specs(doc, &prepared.project.config, &previous.packages);
    warnings.extend(problems);
    let vault = prepared
        .project
        .config
        .mirror
        .as_ref()
        .map(|m| m.url.as_str());
    let mut plans = package_fetch_plans(&specs, pinned, vault);
    for plan in &mut plans {
        plan.clone = prepared.clones.find(&plan.url).cloned();
        plan.offline = prepared.offline;
    }
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::new(OsRunner)));
    let reporter = Reporter::new(prepared.verbosity, &[], &to_stderr);
    let outcomes = run_fetches(
        &prepared.cache,
        &backend,
        plans,
        prepared.limits,
        &|event| {
            reporter.hear(event);
        },
    );
    let fetched: Vec<(&str, &[Fetched])> = outcomes
        .iter()
        .filter_map(|o| {
            o.result
                .as_ref()
                .ok()
                .map(|f| (o.url.as_str(), f.as_slice()))
        })
        .collect();
    summary.failures.extend(outcomes.iter().filter_map(|o| {
        o.result
            .as_ref()
            .err()
            .map(|e| format!("packages from {}: {e}", o.url))
    }));
    let (entries, missing) = package_entries(&specs, &fetched, pinned);
    summary
        .failures
        .extend(missing.iter().map(|dist| format!("{dist}: not resolved")));
    lock.packages = entries;
    lock.odoo = odoo_pin(doc, &previous, update, prepared.offline)
        .map_err(|e| Report::error(Exit::Failure, e))?;
    Ok((specs, edit))
}

/// The Odoo core revision from `[tool.uv.sources] odoo` (git), asked of
/// the remote without downloading anything; the locked one while the
/// source is unchanged and not `update`.
fn odoo_pin(
    doc: &DocumentMut,
    previous: &Lockfile,
    update: bool,
    offline: bool,
) -> Result<Option<OdooPin>, String> {
    let Some(source) = read_uv_git_sources(doc)
        .into_iter()
        .find(|source| normalize_name(&source.name) == "odoo")
    else {
        return Ok(None);
    };
    let kept = previous
        .odoo
        .clone()
        .filter(|pin| !update && repo_key(&pin.url) == repo_key(&source.git));
    if kept.is_some() {
        return Ok(kept);
    }
    let reference = source
        .rev
        .clone()
        .or_else(|| source.tag.clone())
        .or_else(|| source.branch.clone())
        .unwrap_or_else(|| "HEAD".to_owned());
    let full =
        matches!(reference.len(), 40 | 64) && reference.bytes().all(|b| b.is_ascii_hexdigit());
    let rev = if full {
        reference
    } else if offline {
        return Err(format!(
            "odoo: {reference} of {} is asked of the remote; run without --offline",
            source.git
        ));
    } else {
        SystemGit::new(Arc::new(OsRunner))
            .ls_remote(&source.git, &reference)
            .map_err(|e| format!("odoo: {e}"))?
            .ok_or_else(|| format!("odoo: {} has no {reference}", source.git))?
    };
    Ok(Some(OdooPin {
        url: source.git,
        rev,
    }))
}

/// Rewrite vaulted packages to their vault tags in `pyproject.toml`, then
/// update `uv.lock` (`ODX_UV` names the executable) unless `no_uv`.
fn point_packages_at_the_vault(
    ctx: &Context<'_>,
    prepared: &Prepared,
    no_uv: bool,
    specs: &[PackageSpec],
    mut edit: PyprojectEdit,
    lock: &Lockfile,
    summary: &mut Summary,
) -> Result<(), Report> {
    let Some(mirror) = prepared.project.config.mirror.as_ref() else {
        return Ok(());
    };
    let before = edit.to_string();
    let file = &prepared.project.file;
    let failures = rewrite_packages(edit.document_mut(), specs, &lock.packages, &mirror.url);
    summary.failures.extend(
        failures
            .iter()
            .map(|dist| format!("{dist}: cannot point it at the vault in {}", file.display())),
    );
    if edit.to_string() == before {
        return Ok(());
    }
    save(ctx, file, &edit)?;
    summary.rewritten = true;
    if !no_uv {
        let uv = uv_program(ctx.env);
        let invocation = Invocation::new(&uv)
            .arg("lock")
            .current_dir(&prepared.project.root);
        let output = OsRunner.run(&invocation);
        let ok = output.as_ref().is_ok_and(odx_core::Output::success);
        if !ok {
            let detail = output.map_or_else(
                |e| e.to_string(),
                |out| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            );
            summary.failures.push(format!("{uv} lock failed: {detail}"));
        }
    }
    Ok(())
}

/// A pull request the guard rail kept at its locked commit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    repository: String,
    step: usize,
    label: String,
    locked: String,
    head: String,
    ci: Option<LockCi>,
    classification: Option<RedClassification>,
}

fn sha_of(commits: &[Fetched], wanted: &PlannedSource) -> Option<String> {
    commits
        .iter()
        .find(|f| f.remote == wanted.remote && f.source == wanted.source)
        .map(|f| f.sha.clone())
}

/// The force-push guard rail: for every pull request whose head moved since
/// it was locked, apply its `on_head_change` policy to the CI state of the
/// new head; a kept pull request goes back to its locked commit. Every
/// other source is set to the commit it resolved to, so building needs no
/// network.
fn guard(
    ctx: &Context<'_>,
    prepared: &Prepared,
    planned: &mut [Planned],
    fetched: &[(&str, &[Fetched])],
    follow: &[String],
    merger: &Merger,
    warnings: &mut Vec<odx_config::Problem>,
) -> Vec<Kept> {
    let mut kept_prs = Vec::new();
    // Repositories whose fetch failed are left as planned; building reports
    // them.
    let resolved = planned.iter_mut().filter_map(|repo| {
        let key = repo_key(&repo.url);
        fetched
            .iter()
            .find(|(url, _)| repo_key(url) == key)
            .and_then(|(_, commits)| sha_of(commits, &repo.base).map(|base| (repo, *commits, base)))
    });
    for (repo, commits, base) in resolved {
        let entry = locked_entry(repo, prepared.locked());
        let cache = prepared.cache.repo_dir(&repo.url);
        let locked_merges = entry.map_or(&[][..], |e| e.merges.as_slice());
        repo.base.source = odx_git::Source::Commit(base.clone());
        let steps = repo
            .steps
            .iter_mut()
            .enumerate()
            .filter_map(|(i, step)| sha_of(commits, &step.from).map(|head| (i, step, head)));
        for (i, step, head) in steps {
            step.from.source = odx_git::Source::Commit(head.clone());
            let Some((pr, locked)) = step
                .lock
                .pr
                .zip(locked_merges.get(i))
                .filter(|(_, locked)| locked.sha != head)
            else {
                continue;
            };
            let follows = follow.iter().any(|f| *f == format!("{}#{pr}", repo.name));
            let asks_ci = !follows && step.on_head_change == OnHeadChange::FollowIfGreen;
            let report =
                asks_ci.then(|| head_ci(ctx, prepared.cache.root(), &repo.url, &head, warnings));
            let ci = report.as_ref().map(|(ci, _, _)| *ci);
            if decide(step.on_head_change, ci.unwrap_or(LockCi::Unknown), follows)
                == HeadDecision::Adopt
            {
                continue;
            }
            let classification = report
                .filter(|(ci, _, statuses_only)| *ci == LockCi::Red && !*statuses_only)
                .map(|(_, failing, _)| {
                    let (base_ci, base_failing, _) = head_ci(
                        ctx,
                        prepared.cache.root(),
                        &repo.url,
                        &base,
                        &mut Vec::new(),
                    );
                    classify(
                        &failing,
                        (base_ci == LockCi::Red).then_some(base_failing.as_slice()),
                        &merger
                            .changed_dirs(&cache, &base, &head)
                            .unwrap_or_default(),
                        &merger.dirs(&cache, &base).unwrap_or_default(),
                    )
                })
                .or_else(|| (ci == Some(LockCi::Red)).then_some(RedClassification::Red));
            step.from.source = odx_git::Source::Commit(locked.sha.clone());
            kept_prs.push(Kept {
                repository: repo.name.clone(),
                step: i,
                label: step.label.clone(),
                locked: locked.sha.clone(),
                head,
                ci,
                classification,
            });
        }
    }
    kept_prs
}

/// The CI state of `sha` in the repository at `url`, its failing checks,
/// and whether the forge only has commit statuses (no check names worth
/// classifying). A URL that is no forge has no CI; an API that cannot be
/// read counts as pending, so the locked commit is kept.
fn head_ci(
    ctx: &Context<'_>,
    cache: &Path,
    url: &str,
    sha: &str,
    warnings: &mut Vec<odx_config::Problem>,
) -> (LockCi, Vec<String>, bool) {
    let Some(repo) = RepoUrl::parse(url) else {
        return (LockCi::Unknown, Vec::new(), false);
    };
    let answer = ReqwestHttp::new().and_then(|http| {
        let http = CachedHttp::new(http, cache.join("http"));
        let sleep = std::thread::sleep;
        let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
        let kind = client.kind(&repo)?;
        client.ci(&repo, kind, sha).map(|report| (report, kind))
    });
    match answer {
        Ok((report, kind)) => (
            match report.state {
                CiState::Green => LockCi::Green,
                CiState::Red => LockCi::Red,
                CiState::Pending => LockCi::Pending,
                CiState::Unknown => LockCi::Unknown,
            },
            report.failing,
            kind == ForgeKind::Gitea,
        ),
        Err(e) => {
            warnings.push(odx_config::Problem {
                key: url.to_owned(),
                message: format!(
                    "cannot read the CI state of {sha}: {e}; keeping the locked commit"
                ),
            });
            (LockCi::Pending, Vec::new(), false)
        }
    }
}

/// Mark the kept pull requests in the new lock entries: status `pinned`,
/// the head that was not adopted, its CI state, and since when (kept from
/// an earlier pin).
fn record_kept(entries: &mut [LockedRepository], kept: &[Kept], locked: &[LockedRepository]) {
    let today = date(std::time::SystemTime::now());
    for keep in kept {
        let since = locked
            .iter()
            .filter(|entry| entry.name == keep.repository)
            .filter_map(|entry| entry.merges.get(keep.step))
            .find_map(|merge| merge.pinned_since.clone())
            .unwrap_or_else(|| today.clone());
        for merge in entries
            .iter_mut()
            .filter(|entry| entry.name == keep.repository)
            .filter_map(|entry| entry.merges.get_mut(keep.step))
        {
            merge.status = MergeStatus::Pinned;
            merge.upstream_head = Some(keep.head.clone());
            merge.upstream_head_ci = keep.ci;
            merge.upstream_head_classification = keep.classification;
            merge.pinned_since = Some(since.clone());
        }
    }
}

/// Push every locked commit that is not vaulted yet to `[tool.odx.mirror]`
/// and record its ref, after checking that the vault is not public.
fn vault_lock(
    ctx: &Context<'_>,
    prepared: &Prepared,
    lock: &mut Lockfile,
    summary: &mut Summary,
    warnings: &mut Vec<odx_config::Problem>,
) -> Result<(), Report> {
    let Some(mirror) = prepared
        .project
        .config
        .mirror
        .as_ref()
        .filter(|mirror| mirror.push_on_lock != Some(false))
    else {
        return Ok(());
    };
    let unvaulted = |entry: &LockedRepository| entry.merges.iter().any(|m| m.mirror_ref.is_none());
    if !lock.repositories.iter().any(unvaulted)
        && lock
            .packages
            .values()
            .all(|package| package.mirror_ref.is_some())
    {
        return Ok(());
    }
    let refs = vault_refs(
        ctx,
        prepared.cache.root(),
        mirror,
        warnings,
        &mut summary.notes,
    )?;
    let git = SystemGit::new(Arc::new(OsRunner));
    for entry in lock
        .repositories
        .iter_mut()
        .filter(|entry| unvaulted(entry))
    {
        let shas: Vec<String> = std::iter::once(entry.base_sha.clone())
            .chain(entry.merges.iter().map(|m| m.sha.clone()))
            .collect();
        match vault(
            &git,
            &prepared.cache.repo_dir(&entry.url),
            &mirror.url,
            &shas,
            &refs,
        ) {
            Ok(names) => {
                for (merge, name) in entry.merges.iter_mut().zip(names.into_iter().skip(1)) {
                    merge.mirror_ref = Some(name);
                }
                summary.vaulted.push((entry.name.clone(), shas.len()));
            }
            Err(e) => summary
                .failures
                .push(format!("{}: vaulting failed: {e}", entry.name)),
        }
    }
    // uv fetches tags and branches only: packages are always vaulted as tags.
    for (dist, package) in lock
        .packages
        .iter_mut()
        .filter(|(_, package)| package.mirror_ref.is_none())
    {
        let shas = [package.sha.clone()];
        match vault(
            &git,
            &prepared.cache.repo_dir(&package.url),
            &mirror.url,
            &shas,
            &VaultRefs::Tags,
        ) {
            Ok(names) => {
                package.mirror_ref = names.into_iter().next();
                summary.vaulted.push((dist.clone(), 1));
            }
            Err(e) => summary
                .failures
                .push(format!("{dist}: vaulting failed: {e}")),
        }
    }
    Ok(())
}

/// Push the newer upstream heads `prepared`'s odx.lock records for the
/// pull requests it keeps pinned to `mirror`, so that it serves them too
/// (`[tool.odx.bot].vault_heads`): how many were vaulted, of how many.
pub(crate) fn vault_heads(
    ctx: &Context<'_>,
    prepared: &Prepared,
    mirror: &odx_config::schema::Mirror,
) -> (usize, usize) {
    let heads: Vec<(&LockedRepository, Vec<String>)> = prepared
        .locked()
        .iter()
        .map(|entry| {
            let shas = entry
                .merges
                .iter()
                .filter_map(|merge| merge.upstream_head.clone())
                .collect();
            (entry, shas)
        })
        .filter(|(_, shas): &(_, Vec<String>)| !shas.is_empty())
        .collect();
    let of = heads.iter().map(|(_, shas)| shas.len()).sum();
    // A vault that is refused vaults nothing, as one that fails.
    let refs = vault_refs(
        ctx,
        prepared.cache.root(),
        mirror,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    let git = SystemGit::new(Arc::new(OsRunner));
    let done = refs.map_or(0, |refs| {
        heads
            .iter()
            .filter(|(entry, shas)| {
                vault(
                    &git,
                    &prepared.cache.repo_dir(&entry.url),
                    &mirror.url,
                    shas,
                    &refs,
                )
                .is_ok()
            })
            .map(|(_, shas)| shas.len())
            .sum()
    });
    (done, of)
}

/// The refs vaulted commits get, after checking the vault's visibility on
/// its forge: a public vault is refused unless `allow_public = true`, and
/// GitLab vaults use tags (GitLab protects tags, not custom refs).
fn vault_refs(
    ctx: &Context<'_>,
    cache: &Path,
    mirror: &odx_config::schema::Mirror,
    warnings: &mut Vec<odx_config::Problem>,
    notes: &mut Vec<String>,
) -> Result<VaultRefs, Report> {
    let refuse = |message: String| {
        Report::error(
            Exit::Failure,
            format!("{message}; run `odx lock --no-push` to lock without vaulting"),
        )
    };
    let auto_create = mirror.auto_create == Some(true);
    let protected_tags = match RepoUrl::parse(&mirror.url) {
        Some(repo) => {
            let (kind, visibility, created) = ReqwestHttp::new()
                .and_then(|http| {
                    let http = CachedHttp::new(http, cache.join("http"));
                    let sleep = std::thread::sleep;
                    let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
                    let kind = client.kind(&repo)?;
                    match client.visibility(&repo, kind) {
                        Err(ForgeError::NotVisible(_)) if auto_create => {
                            client.create_private(&repo, kind)?;
                            client.protect_vault_tags(&repo, kind)?;
                            Ok((kind, Visibility::Private, true))
                        }
                        other => other.map(|visibility| (kind, visibility, false)),
                    }
                })
                .map_err(|e| refuse(e.to_string()))?;
            if created {
                notes.push(format!(
                    "created the private vault {} on {} with protected odx/* tags",
                    mirror.url,
                    kind.name()
                ));
            }
            if visibility == Visibility::Public && mirror.allow_public != Some(true) {
                return Err(refuse(format!(
                    "{} is public on {}; odx vaults commits only in private repositories (allow_public = true overrides)",
                    mirror.url,
                    kind.name()
                )));
            }
            // GitLab protects tags, not custom refs; an auto-created vault
            // protects tags everywhere.
            kind == ForgeKind::Gitlab || auto_create
        }
        None => false,
    };
    Ok(match (protected_tags, mirror.ref_style) {
        (true, Some(RefStyle::Refs)) => {
            warnings.push(odx_config::Problem {
                key: "tool.odx.mirror.ref_style".to_owned(),
                message:
                    "the vault protects tags, not custom refs; vaulting under refs/tags/odx instead"
                        .to_owned(),
            });
            VaultRefs::Tags
        }
        (true, _) | (false, Some(RefStyle::Tags)) => VaultRefs::Tags,
        (false, _) => VaultRefs::Prefix(
            mirror
                .ref_prefix
                .clone()
                .unwrap_or_else(|| "refs/odx".to_owned()),
        ),
    })
}

fn relative(dir: &Path, root: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// The outcome of a run: per repository URL (fetch), per repository
/// (build or already current), and the changes to `odx.lock`.
struct Summary {
    fetches: Vec<(String, Result<Vec<Fetched>, String>)>,
    builds: Vec<(String, String, Result<Aggregated, String>)>,
    current: Vec<(String, String)>,
    changes: Vec<Change>,
    vaulted: Vec<(String, usize)>,
    kept: Vec<Kept>,
    bundled: Vec<(String, usize, String)>,
    notes: Vec<String>,
    sniffed: Vec<Sniffed>,
    rewritten: bool,
    failures: Vec<String>,
    /// Built in scratch working trees (`odx lock`): directories are not
    /// shown.
    scratch: bool,
}

impl Summary {
    fn new(built: &Built, planned: &[Planned], root: &Path, scratch: bool) -> Self {
        let fetches = built
            .plans
            .iter()
            .zip(&built.outcomes)
            .map(|(plan, outcome)| {
                let result = outcome
                    .result
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(ToString::to_string);
                (plan.names.join(", "), result)
            })
            .collect();
        // Fetch failures are reported with the fetches; a loaded
        // configuration has only valid merges, so what was planned is all
        // that is built, one result each, in order.
        let builds = built
            .pairs(planned)
            .map(|(repo, aggregation, result)| {
                let result = match result {
                    Ok(aggregated) => Ok(aggregated.clone()),
                    Err(e) => Err(e.to_string()),
                };
                (
                    repo.name.clone(),
                    relative(&aggregation.target_dir, root),
                    result,
                )
            })
            .collect();
        Self {
            fetches,
            builds,
            current: Vec::new(),
            changes: Vec::new(),
            vaulted: Vec::new(),
            kept: Vec::new(),
            bundled: Vec::new(),
            notes: Vec::new(),
            sniffed: Vec::new(),
            rewritten: false,
            failures: Vec::new(),
            scratch,
        }
    }

    fn failed(&self) -> bool {
        !self.failures.is_empty()
            || self.fetches.iter().any(|(_, r)| r.is_err())
            || self.builds.iter().any(|(_, _, r)| r.is_err())
    }

    fn report(&self, json: bool, fetch_only: bool, warnings: Vec<odx_config::Problem>) -> Report {
        let mut report = if json {
            Report {
                out: format!("{}\n", self.to_json(fetch_only)),
                ..Report::default()
            }
        } else {
            Report {
                warnings,
                ..self.to_text(fetch_only)
            }
        };
        if self.failed() {
            report.exit = Exit::Failure;
        }
        report
    }

    fn to_text(&self, fetch_only: bool) -> Report {
        let mut report = Report::default();
        let mut failures = Vec::new();
        for (names, result) in &self.fetches {
            match result {
                Ok(fetched) => {
                    let count = |via| fetched.iter().filter(|f| f.via == via).count();
                    let _ = write!(report.out, "fetched {names}: {} refs", fetched.len());
                    for (via, label) in [
                        (Via::Mirror, "from the mirror"),
                        (Via::Clone, "through the local clone"),
                        (Via::Cache, "already cached"),
                    ] {
                        let n = count(via);
                        if n > 0 {
                            let _ = write!(report.out, " ({n} {label})");
                        }
                    }
                    report.out.push('\n');
                }
                Err(e) => failures.push(format!("{names}: {e}")),
            }
        }
        if !fetch_only {
            for (name, dir) in &self.current {
                let _ = writeln!(report.out, "up to date {name} in {dir} (locked)");
            }
            for (name, dir, result) in &self.builds {
                match result {
                    Ok(built) if self.scratch => {
                        let _ = writeln!(
                            report.out,
                            "resolved {name}: {} ({})",
                            built.describe(),
                            short(&built.head)
                        );
                    }
                    Ok(built) => {
                        let _ = writeln!(
                            report.out,
                            "built {name} in {dir}: {} ({})",
                            built.describe(),
                            short(&built.head)
                        );
                    }
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
        report.out.push_str(&describe_sniffed(&self.sniffed));
        for change in &self.changes {
            let _ = writeln!(report.out, "{}", describe_change(change));
        }
        if self.rewritten {
            report
                .out
                .push_str("pointed packages at the vault in pyproject.toml\n");
        }
        for (url, commits, file) in &self.bundled {
            let _ = writeln!(report.out, "bundled {commits} commits of {url} into {file}");
        }
        for keep in &self.kept {
            let _ = writeln!(report.out, "{}", describe_kept(keep));
        }
        for note in &self.notes {
            let _ = writeln!(report.out, "{note}");
        }
        for (name, commits) in &self.vaulted {
            let _ = writeln!(report.out, "vaulted {name}: {commits} commits");
        }
        failures.extend(self.failures.iter().cloned());
        if !failures.is_empty() {
            let what = if self.scratch {
                "locking"
            } else {
                "aggregation"
            };
            report.error = Some(format!("{what} failed for\n  {}", failures.join("\n  ")));
        }
        report
    }

    fn to_json(&self, fetch_only: bool) -> serde_json::Value {
        let fetches: Vec<serde_json::Value> = self
            .fetches
            .iter()
            .map(|(names, result)| match result {
                Ok(fetched) => serde_json::json!({
                    "repositories": names,
                    "refs": fetched.len(),
                    "from_mirror": fetched.iter().filter(|f| f.via == Via::Mirror).count(),
                    "through_clone": fetched.iter().filter(|f| f.via == Via::Clone).count(),
                    "cached": fetched.iter().filter(|f| f.via == Via::Cache).count(),
                }),
                Err(e) => serde_json::json!({ "repositories": names, "error": e }),
            })
            .collect();
        let mut value = serde_json::Map::new();
        value.insert("schema".to_owned(), serde_json::json!(1));
        value.insert("fetched".to_owned(), serde_json::Value::Array(fetches));
        if !fetch_only {
            let builds: Vec<serde_json::Value> = self
                .builds
                .iter()
                .map(|(name, dir, result)| match result {
                    Ok(built) => serde_json::json!({
                        "name": name,
                        "target_dir": (!self.scratch).then_some(dir),
                        "head": built.head,
                        "tree": built.tree,
                        "merges": built.steps.iter().map(|(label, outcome)| match outcome {
                            StepOutcome::Skipped { files } => serde_json::json!({ "label": label, "status": "skipped", "files": files }),
                            other => serde_json::json!({ "label": label, "status": other.status() }),
                        }).collect::<Vec<_>>(),
                    }),
                    Err(e) => serde_json::json!({ "name": name, "target_dir": (!self.scratch).then_some(dir), "error": e }),
                })
                .collect();
            value.insert("built".to_owned(), serde_json::Value::Array(builds));
            let current: Vec<serde_json::Value> = self
                .current
                .iter()
                .map(|(name, dir)| serde_json::json!({ "name": name, "target_dir": dir }))
                .collect();
            value.insert("current".to_owned(), serde_json::Value::Array(current));
        }
        let changes: Vec<serde_json::Value> = self
            .changes
            .iter()
            .map(|change| {
                serde_json::json!({
                    "repository": change.repository,
                    "item": change.item,
                    "change": change.kind.as_str(),
                    "old": change.old,
                    "new": change.new,
                })
            })
            .collect();
        value.insert("lock".to_owned(), serde_json::Value::Array(changes));
        let vaulted: Vec<serde_json::Value> = self
            .vaulted
            .iter()
            .map(|(name, commits)| serde_json::json!({ "repository": name, "commits": commits }))
            .collect();
        value.insert("vaulted".to_owned(), serde_json::Value::Array(vaulted));
        let kept: Vec<serde_json::Value> = self
            .kept
            .iter()
            .map(|keep| {
                serde_json::json!({
                    "repository": keep.repository,
                    "item": keep.label,
                    "locked": keep.locked,
                    "upstream_head": keep.head,
                    "ci": keep.ci.map(LockCi::as_str),
                    "classification": keep.classification.map(RedClassification::as_str),
                })
            })
            .collect();
        value.insert("kept".to_owned(), serde_json::Value::Array(kept));
        value.insert("rewritten".to_owned(), serde_json::json!(self.rewritten));
        value.insert("notes".to_owned(), serde_json::json!(self.notes));
        value.insert("migrations".to_owned(), sniffed_json(&self.sniffed));
        let bundled: Vec<serde_json::Value> = self
            .bundled
            .iter()
            .map(|(url, commits, file)| serde_json::json!({ "url": url, "commits": commits, "file": file }))
            .collect();
        value.insert("bundled".to_owned(), serde_json::Value::Array(bundled));
        let failures = &self.failures;
        value.insert("errors".to_owned(), serde_json::json!(failures));
        serde_json::Value::Object(value)
    }
}

/// `kept pos #1 at 1a2b3c4d5e6f: upstream head 6f5e4d3c2b1a is red (base also failing)`.
fn describe_kept(keep: &Kept) -> String {
    let state = match (keep.ci, keep.classification) {
        (Some(LockCi::Red), Some(RedClassification::BaseAlsoFailing)) => {
            "is red, likely unrelated (base also failing)"
        }
        (Some(LockCi::Red), Some(RedClassification::OtherAddon)) => {
            "is red, likely unrelated (other addon)"
        }
        (Some(LockCi::Red), _) => "is red",
        (Some(_), _) => "is pending",
        (None, _) => "is newer (on_head_change = \"pin\")",
    };
    format!(
        "kept {} {} at {}: upstream head {} {state}",
        keep.repository,
        keep.label,
        short(&keep.locked),
        short(&keep.head)
    )
}

/// `locked pos #1 at 1a2b3c4d5e6f`, `updated pos #1: a -> b (rebased (same patch))`.
fn describe_change(change: &Change) -> String {
    let what = if change.item.is_empty() {
        change.repository.clone()
    } else {
        format!("{} {}", change.repository, change.item)
    };
    let old = change.old.as_deref().map(short).unwrap_or_default();
    let new = change.new.as_deref().map(short).unwrap_or_default();
    match change.kind {
        ChangeKind::Added => format!("locked {what} at {new}"),
        ChangeKind::Removed => format!("unlocked {what} (was {old})"),
        ChangeKind::Rebased | ChangeKind::Changed => {
            format!("updated {what}: {old} -> {new} ({})", change.kind.as_str())
        }
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::MapEnv;

    fn cache_dir_of(config: &OdxConfig, root: &Path, env: &dyn Env) -> PathBuf {
        cache_dir(config, &UserConfig::default(), root, env)
    }

    #[test]
    fn lock_entries_report_git_failures() {
        let config = config(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"u\"\nmerges = [{ pr = 1 }]\n",
        );
        let planned = plan_config(&config, Path::new("/p"), None).unwrap();
        let aggregation = Aggregation {
            cache_repo: PathBuf::from("/nonexistent/odx/cache.git"),
            target_dir: PathBuf::from("/p/vendor/pos"),
            base: "0".repeat(40),
            steps: vec![odx_git::Step {
                label: "#1".into(),
                sha: "1".repeat(40),
                skip_on_conflict: false,
            }],
            strategy: odx_git::Strategy::Rebase,
            mergiraf: None,
            keep_conflicts: false,
            force: false,
        };
        let built = Built {
            plans: Vec::new(),
            outcomes: Vec::new(),
            aggregations: vec![Ok(RepoAggregation {
                name: "pos".into(),
                aggregation,
            })],
            results: vec![Ok(Aggregated {
                head: "2".repeat(40),
                tree: "3".repeat(40),
                steps: vec![("#1".into(), StepOutcome::Merged)],
            })],
        };
        let (entries, failures) = lock_entries(&planned, &built, &Merger::new(Arc::new(OsRunner)));
        assert_eq!(entries, []);
        assert!(failures[0].starts_with("pos: "), "{failures:?}");
    }

    #[test]
    fn ci_detection() {
        assert!(!in_ci(&MapEnv::default()));
        for (value, ci) in [
            ("true", true),
            ("1", true),
            ("", false),
            ("false", false),
            ("0", false),
        ] {
            assert_eq!(in_ci(&MapEnv::default().with("CI", value)), ci, "{value:?}");
        }
    }

    #[test]
    fn user_cache_dir() {
        let user =
            user::parse_user_config("[cache]\ndir = \"~/c\"\n", Path::new("config.toml")).unwrap();
        let env = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            cache_dir(&config(""), &user, Path::new("/p"), &env),
            Path::new("/home/dev/c")
        );
    }

    fn config(toml: &str) -> OdxConfig {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{toml}");
        odx_config::parse_pyproject(&text, Path::new("/p/pyproject.toml"))
            .unwrap()
            .unwrap()
            .config
    }

    #[test]
    fn configured_cache_dir() {
        let root = Path::new("/p");
        let home = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            cache_dir_of(&config("cache_dir = \"~/c\""), root, &home),
            Path::new("/home/dev/c")
        );
        let windows_home = MapEnv::default().with("USERPROFILE", "/Users/dev");
        assert_eq!(
            cache_dir_of(&config("cache_dir = \"~/c\""), root, &windows_home),
            Path::new("/Users/dev").join("c")
        );
        assert_eq!(
            cache_dir_of(&config("cache_dir = \"~/c\""), root, &MapEnv::default()),
            root.join("~/c")
        );
        assert_eq!(
            cache_dir_of(&config("cache_dir = \"cache\""), root, &home),
            root.join("cache")
        );
        let env = home.with("ODX_CACHE_DIR", "/tmp/odx");
        assert_eq!(cache_dir_of(&config(""), root, &env), Path::new("/tmp/odx"));
        assert_eq!(
            cache_dir_of(&config(""), root, &MapEnv::default()),
            root.join(".odx").join("cache")
        );
    }
}
