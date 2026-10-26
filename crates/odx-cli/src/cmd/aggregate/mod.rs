//! `odx aggregate` and `odx lock`: fill the git cache, build the working
//! trees and keep `odx.lock` current. This module prepares and builds;
//! `lock` resolves and writes the lockfile, `vault` pushes to the vault and
//! `summary` reports a run.

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use odx_aggregate::{
    BuildOptions, PlanError, Planned, RepoAggregation, RepoPlan, aggregations, fetch_plans,
    local_clones, lock_changes, lock_entry, mergiraf_program, pin, plan_config, run_pipeline,
    strategy, update_lock,
};
use odx_config::lock::{LockedRepository, Lockfile};
use odx_config::user::{self, UserConfig, UserMirror, expand_home, load_user_config, mirror_for};
use odx_config::{ConfigError, OdxConfig, Project, Source, Span, discover};
use odx_core::system::OsRunner;
use odx_core::{Env, Exit};
use odx_git::{
    Aggregated, Aggregation, Cache, Clones, Fetched, GitBackend, Limits, MergeError, Merger,
    Outcome, SystemGit, url_hash,
};
use std::collections::BTreeMap;

use super::Report;
use super::edit::read_lock;
use super::progress::{Reporter, Verbosity, to_stderr};
use super::sniff::{fail_on_new, script_count, sniff_locked};
use crate::Context;
use crate::cli::{AggregateArgs, GlobalArgs};

mod lock;
mod summary;
mod vault;

use lock::Kept;
pub(crate) use lock::run_lock;
pub(crate) use summary::AggregateJson;
use summary::{Summary, relative};
pub(crate) use vault::vault_heads;
use vault::vault_lock;

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

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::MapEnv;
    use odx_git::StepOutcome;

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
