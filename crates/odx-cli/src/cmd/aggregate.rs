//! `odx aggregate` and `odx lock`: fill the git cache, build the working
//! trees and keep `odx.lock` current.

use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use odx_aggregate::{
    BuildOptions, Change, ChangeKind, PlanError, Planned, RepoAggregation, RepoPlan, aggregations,
    fetch_plans, local_clones, lock_changes, lock_entry, mergiraf_program, pin, plan_config,
    run_pipeline, strategy, update_lock,
};
use odx_config::lock::{LockedRepository, Lockfile};
use odx_config::schema::RefStyle;
use odx_config::user::{self, UserConfig, expand_home, load_user_config};
use odx_config::{ConfigError, OdxConfig, Project, Source, discover};
use odx_core::system::OsRunner;
use odx_core::{Env, Exit};
use odx_forge::{ForgeClient, ForgeKind, RepoUrl, ReqwestHttp, Visibility};
use odx_git::{
    Aggregated, Aggregation, Cache, Clones, Fetched, GitBackend, Limits, MergeError, Merger,
    Outcome, StepOutcome, SystemGit, VaultRefs, Via, url_hash, vault,
};

use super::Report;
use super::edit::read_lock;
use crate::Context;
use crate::cli::{AggregateArgs, GlobalArgs, LockArgs};

/// How long to wait for another odx process's repository lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

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
struct Prepared {
    project: Project,
    lock_path: PathBuf,
    lock: Option<Lockfile>,
    cache: Cache,
    clones: Clones,
    limits: Limits,
}

impl Prepared {
    fn locked(&self) -> &[LockedRepository] {
        self.lock.as_ref().map_or(&[], |lock| &lock.repositories)
    }
}

fn prepare(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Prepared, Report> {
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
    Ok(Prepared {
        clones: local_clones(&user, ctx.env),
        project: *project,
        lock_path,
        lock,
        cache,
        limits,
    })
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
    }
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::new(OsRunner)));
    let aggregated = std::cell::RefCell::new(Vec::new());
    let plan_builds = |outcomes: &[Outcome]| {
        let fetched: Vec<(String, Vec<Fetched>)> = outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().ok().map(|f| (o.url.clone(), f.clone())))
            .collect();
        let repos = aggregations(planned, &prepared.cache, &fetched, options);
        let runnable: Vec<Aggregation> = repos
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|r| r.aggregation.clone()))
            .collect();
        *aggregated.borrow_mut() = repos;
        runnable
    };
    let (outcomes, results) = run_pipeline(
        &prepared.cache,
        &backend,
        plans.iter().map(|p| p.plan.clone()).collect(),
        prepared.limits,
        merger,
        &plan_builds,
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

fn write_lock(ctx: &Context<'_>, prepared: &Prepared, lock: &Lockfile) -> Result<(), Report> {
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
    let prepared = prepare(global, ctx)?;
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
    if !args.fetch_only && !args.locked {
        let (entries, failures) = lock_entries(&planned, &built, &merger);
        summary.failures.extend(failures);
        let lock = next_lock(&prepared, entries, args.repo.as_deref());
        let previous = prepared.lock.clone().unwrap_or_default();
        summary.changes = lock_changes(&previous, &lock);
        if previous != lock {
            write_lock(ctx, &prepared, &lock)?;
        }
    }
    Ok(summary.report(global.json, args.fetch_only, prepared.project.warnings))
}

/// `odx lock [--repo NAME] [--check | --update]`.
pub(crate) fn run_lock(args: &LockArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    lock(args, global, ctx).unwrap_or_else(|report| report)
}

fn lock(args: &LockArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    if args.auto_update {
        return Err(Report::error(
            Exit::Usage,
            "odx lock --auto-update: not implemented yet (roadmap phase 5)".to_owned(),
        ));
    }
    let prepared = prepare(global, ctx)?;
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
    for repo in &mut planned {
        let name: String = repo
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        repo.target_dir = prepared
            .cache
            .root()
            .join("locks")
            .join(format!("{}-{name}", url_hash(&repo.url)));
    }
    let (merger, options) = merger_and_options(&prepared.project.config, ctx, false, true)?;
    let built = build(&prepared, &planned, &options, Some(&merger), true);
    let mut summary = Summary::new(&built, &planned, &prepared.project.root, true);
    let (entries, failures) = lock_entries(&planned, &built, &merger);
    summary.failures.extend(failures);
    let lock = next_lock(&prepared, entries, args.repo.as_deref());
    let previous = prepared.lock.clone().unwrap_or_default();
    summary.changes = lock_changes(&previous, &lock);
    let outdated = previous != lock;
    if args.check {
        if outdated {
            summary.failures.push(format!(
                "{} is not up to date; run `odx lock`",
                prepared.lock_path.display()
            ));
        }
    } else {
        let mut lock = lock;
        let mut warnings = prepared.project.warnings.clone();
        if !args.no_push {
            vault_lock(ctx, &prepared, &mut lock, &mut summary, &mut warnings)?;
        }
        if previous != lock {
            write_lock(ctx, &prepared, &lock)?;
        }
        return Ok(summary.report(global.json, false, warnings));
    }
    Ok(summary.report(global.json, false, prepared.project.warnings))
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
    if !lock.repositories.iter().any(unvaulted) {
        return Ok(());
    }
    let refs = vault_refs(ctx, mirror, warnings)?;
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
    Ok(())
}

/// The refs vaulted commits get, after checking the vault's visibility on
/// its forge: a public vault is refused unless `allow_public = true`, and
/// GitLab vaults use tags (GitLab protects tags, not custom refs).
fn vault_refs(
    ctx: &Context<'_>,
    mirror: &odx_config::schema::Mirror,
    warnings: &mut Vec<odx_config::Problem>,
) -> Result<VaultRefs, Report> {
    let refuse = |message: String| {
        Report::error(
            Exit::Failure,
            format!("{message}; run `odx lock --no-push` to lock without vaulting"),
        )
    };
    let gitlab = match RepoUrl::parse(&mirror.url) {
        Some(repo) => {
            let (kind, visibility) = ReqwestHttp::new()
                .and_then(|http| {
                    let sleep = std::thread::sleep;
                    let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
                    let kind = client.kind(&repo)?;
                    client
                        .visibility(&repo, kind)
                        .map(|visibility| (kind, visibility))
                })
                .map_err(|e| refuse(e.to_string()))?;
            if visibility == Visibility::Public && mirror.allow_public != Some(true) {
                return Err(refuse(format!(
                    "{} is public on {}; odx vaults commits only in private repositories (allow_public = true overrides)",
                    mirror.url,
                    kind.name()
                )));
            }
            kind == ForgeKind::Gitlab
        }
        None => false,
    };
    Ok(match (gitlab, mirror.ref_style) {
        (true, Some(RefStyle::Refs)) => {
            warnings.push(odx_config::Problem {
                key: "tool.odx.mirror.ref_style".to_owned(),
                message:
                    "GitLab protects tags, not custom refs; vaulting under refs/tags/odx instead"
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
        for change in &self.changes {
            let _ = writeln!(report.out, "{}", describe_change(change));
        }
        for (name, commits) in &self.vaulted {
            let _ = writeln!(report.out, "vaulted {name}: {commits} commits");
        }
        failures.extend(self.failures.iter().cloned());
        if !failures.is_empty() {
            report.error = Some(format!(
                "aggregation failed for\n  {}",
                failures.join("\n  ")
            ));
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
        let failures = &self.failures;
        value.insert("errors".to_owned(), serde_json::json!(failures));
        serde_json::Value::Object(value)
    }
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
