//! `odx aggregate`: fill the git cache and build the working trees.

use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use std::ffi::OsString;

use odx_aggregate::{BuildOptions, RepoAggregation, RepoPlan, aggregations, fetch_plans};
use odx_config::schema::MergirafMode;
use odx_config::user::{CloneMode, UserConfig, expand_home, load_user_config};
use odx_config::{ConfigError, OdxConfig, Source, discover};
use odx_core::system::OsRunner;
use odx_core::{Env, Exit, Invocation, ProcessRunner};
use odx_git::{
    Aggregated, Aggregation, Cache, CancellationToken, Clones, FetchPlan, Fetched, GitBackend,
    GitError, Limits, MergeError, Merger, Outcome, StepOutcome, SystemGit, Via, aggregate_all,
    fetch_all,
};

use super::Report;
use crate::Context;
use crate::cli::{AggregateArgs, GlobalArgs};

/// How long to wait for another odx process's repository lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// The cache directory: `cache_dir` from the project configuration
/// (relative to the project root, `~/` expanded), else `ODX_CACHE_DIR`,
/// else `[cache].dir` from the user configuration, else the platform cache
/// directory.
pub(crate) fn cache_dir(
    config: &OdxConfig,
    user: &UserConfig,
    root: &Path,
    env: &dyn Env,
) -> PathBuf {
    if let Some(dir) = &config.cache_dir {
        return root.join(expand_home(dir, env));
    }
    if let Some(dir) = env.var("ODX_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dir) = user.cache.as_ref().and_then(|cache| cache.dir.as_ref()) {
        return expand_home(dir, env);
    }
    let home = env.var("HOME").or_else(|| env.var("USERPROFILE"));
    platform_cache_dir(env, home).unwrap_or_else(|| root.join(".odx").join("cache"))
}

/// The local clones configured in `[clones]` of the user configuration.
pub(crate) fn local_clones(user: &UserConfig, env: &dyn Env) -> Clones {
    user.clones.as_ref().map_or_else(Clones::default, |clones| {
        let dirs: Vec<PathBuf> = clones
            .dirs
            .iter()
            .map(|dir| expand_home(dir, env))
            .collect();
        Clones::scan(&dirs, clones.mode == Some(CloneMode::Update))
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_dir(env: &dyn Env, home: Option<String>) -> Option<PathBuf> {
    env.var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|home| Path::new(&home).join(".cache")))
        .map(|dir| dir.join("odx"))
}

#[cfg(target_os = "macos")]
fn platform_cache_dir(_: &dyn Env, home: Option<String>) -> Option<PathBuf> {
    home.map(|home| Path::new(&home).join("Library").join("Caches").join("odx"))
}

#[cfg(windows)]
fn platform_cache_dir(env: &dyn Env, _: Option<String>) -> Option<PathBuf> {
    env.var("LOCALAPPDATA")
        .map(|dir| Path::new(&dir).join("odx").join("cache"))
}

/// What `--fetch-only` and a full aggregation share.
struct Prepared {
    project: odx_config::Project,
    plans: Vec<RepoPlan>,
    cache: Cache,
    limits: Limits,
}

fn prepare(
    args: &AggregateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Prepared, Report> {
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
    let mut plans = fetch_plans(&project.config, args.repo.as_deref(), args.prefer_mirror)
        .map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    let clones = local_clones(&user, ctx.env);
    for plan in &mut plans {
        plan.plan.clone = clones.find(&plan.plan.url).cloned();
    }
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
        project: *project,
        plans,
        cache,
        limits,
    })
}

/// The mergiraf executable to use: `ODX_MERGIRAF` or `mergiraf`, when the
/// configured mode wants it and it runs.
///
/// # Errors
///
/// A message when the mode is `always` and the executable cannot be run.
pub(crate) fn mergiraf_program(
    mode: MergirafMode,
    env: &dyn Env,
    runner: &dyn ProcessRunner,
) -> Result<Option<OsString>, String> {
    let program = env
        .var("ODX_MERGIRAF")
        .unwrap_or_else(|| "mergiraf".to_owned());
    let runs = || {
        runner
            .run(&Invocation::new(&program).arg("--version"))
            .is_ok_and(|out| out.success())
    };
    match mode {
        MergirafMode::Never => Ok(None),
        MergirafMode::Auto => Ok(runs().then(|| OsString::from(&program))),
        MergirafMode::Always if runs() => Ok(Some(OsString::from(&program))),
        MergirafMode::Always => Err(format!(
            "mergiraf = \"always\" is configured, but {program} cannot be run"
        )),
    }
}

/// Fetch, then build working trees from what was fetched, on a runtime of
/// its own; pending work is cancelled when Ctrl-C is pressed.
///
/// Excluded from coverage: building the runtime only fails when the OS
/// refuses threads, and a real SIGINT cannot be raised inside the test
/// process. Everything it calls is tested on its own.
#[cfg_attr(coverage_nightly, coverage(off))]
fn run_pipeline(
    prepared: &Prepared,
    backend: &Arc<dyn GitBackend>,
    merger: Option<&Arc<Merger>>,
    build: &dyn Fn(&[Outcome]) -> Vec<Aggregation>,
) -> (Vec<Outcome>, Vec<Result<Aggregated, MergeError>>) {
    let plans: Vec<FetchPlan> = prepared.plans.iter().map(|p| p.plan.clone()).collect();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let message = e.to_string();
            let outcomes = plans
                .into_iter()
                .map(|plan| Outcome {
                    url: plan.url,
                    result: Err(GitError::Io(std::io::Error::other(message.clone()))),
                })
                .collect();
            return (outcomes, Vec::new());
        }
    };
    runtime.block_on(async {
        let cancel = CancellationToken::new();
        let on_signal = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                on_signal.cancel();
            }
        });
        let outcomes = fetch_all(&prepared.cache, backend, plans, prepared.limits, &cancel).await;
        let results = match merger {
            Some(merger) => {
                aggregate_all(merger, build(&outcomes), prepared.limits.jobs, &cancel).await
            }
            None => Vec::new(),
        };
        (outcomes, results)
    })
}

/// `odx aggregate [--fetch-only] [--repo NAME] [--prefer-mirror] [--keep-conflicts] [--force]`.
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
    let prepared = prepare(args, global, ctx)?;
    let runner = Arc::new(OsRunner);
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::<OsRunner>::clone(&runner)));
    let merger = if args.fetch_only {
        None
    } else {
        let mode = prepared.project.config.mergiraf.unwrap_or_default();
        let mergiraf = mergiraf_program(mode, ctx.env, runner.as_ref())
            .map_err(|e| Report::error(Exit::Usage, e))?;
        Some((Arc::new(Merger::new(runner)), mergiraf))
    };
    let options = BuildOptions {
        mergiraf: merger.as_ref().and_then(|(_, m)| m.clone()),
        keep_conflicts: args.keep_conflicts,
        force: args.force,
    };
    let planned = std::cell::RefCell::new(Vec::new());
    let plan_builds = |outcomes: &[Outcome]| {
        let fetched: Vec<(String, Vec<Fetched>)> = outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().ok().map(|f| (o.url.clone(), f.clone())))
            .collect();
        let repos = aggregations(
            &prepared.project.config,
            &prepared.project.root,
            args.repo.as_deref(),
            &prepared.cache,
            &fetched,
            &options,
        );
        let runnable: Vec<Aggregation> = repos
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|r| r.aggregation.clone()))
            .collect();
        *planned.borrow_mut() = repos;
        runnable
    };
    let (outcomes, results) = run_pipeline(
        &prepared,
        &backend,
        merger.as_ref().map(|(m, _)| m),
        &plan_builds,
    );
    let planned = planned.into_inner();
    let summary = Summary::new(
        &prepared.plans,
        &outcomes,
        &planned,
        &results,
        &prepared.project.root,
    );
    let mut report = if global.json {
        Report {
            out: format!("{}\n", summary.to_json(args.fetch_only)),
            ..Report::default()
        }
    } else {
        summary.to_text(args.fetch_only)
    };
    if !global.json {
        report.warnings = prepared.project.warnings;
    }
    if summary.failed() {
        report.exit = Exit::Failure;
    }
    Ok(report)
}

/// The outcome of a run, per repository URL (fetch) and per repository
/// (build).
struct Summary {
    fetches: Vec<(String, Result<Vec<Fetched>, String>)>,
    builds: Vec<(String, String, Result<Aggregated, String>)>,
}

impl Summary {
    fn new(
        plans: &[RepoPlan],
        outcomes: &[Outcome],
        planned: &[Result<RepoAggregation, odx_aggregate::PlanError>],
        built: &[Result<Aggregated, MergeError>],
        root: &Path,
    ) -> Self {
        let fetches = plans
            .iter()
            .zip(outcomes)
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
        // configuration has only valid merges, so `Ok` entries are all that
        // is built, one result each, in order.
        let builds = planned
            .iter()
            .filter_map(|repo| repo.as_ref().ok())
            .zip(built)
            .map(|(repo, result)| {
                let dir = repo
                    .aggregation
                    .target_dir
                    .strip_prefix(root)
                    .unwrap_or(&repo.aggregation.target_dir)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                let result = match result {
                    Ok(aggregated) => Ok(aggregated.clone()),
                    Err(e) => Err(e.to_string()),
                };
                (repo.name.clone(), dir, result)
            })
            .collect();
        Self { fetches, builds }
    }

    fn failed(&self) -> bool {
        self.fetches.iter().any(|(_, r)| r.is_err())
            || self.builds.iter().any(|(_, _, r)| r.is_err())
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
            for (name, dir, result) in &self.builds {
                match result {
                    Ok(built) => {
                        let _ = writeln!(
                            report.out,
                            "built {name} in {dir}: {} ({})",
                            describe(built),
                            short(&built.head)
                        );
                    }
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
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
                        "target_dir": dir,
                        "head": built.head,
                        "tree": built.tree,
                        "merges": built.steps.iter().map(|(label, outcome)| match outcome {
                            StepOutcome::Merged => serde_json::json!({ "label": label, "status": "merged" }),
                            StepOutcome::UpToDate => serde_json::json!({ "label": label, "status": "up-to-date" }),
                            StepOutcome::Skipped { files } => serde_json::json!({ "label": label, "status": "skipped", "files": files }),
                        }).collect::<Vec<_>>(),
                    }),
                    Err(e) => serde_json::json!({ "name": name, "target_dir": dir, "error": e }),
                })
                .collect();
            value.insert("built".to_owned(), serde_json::Value::Array(builds));
        }
        serde_json::Value::Object(value)
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// `2 merged, 1 up to date, 1 skipped (conflicts in a.xml)`, or `base only`.
fn describe(built: &Aggregated) -> String {
    let count =
        |want: fn(&StepOutcome) -> bool| built.steps.iter().filter(|(_, o)| want(o)).count();
    let merged = count(|o| *o == StepOutcome::Merged);
    let current = count(|o| *o == StepOutcome::UpToDate);
    let skipped: Vec<String> = built
        .steps
        .iter()
        .filter_map(|(label, outcome)| match outcome {
            StepOutcome::Skipped { files } => {
                Some(format!("{label} conflicts in {}", files.join(", ")))
            }
            _ => None,
        })
        .collect();
    let mut parts = Vec::new();
    if merged > 0 {
        parts.push(format!("{merged} merged"));
    }
    if current > 0 {
        parts.push(format!("{current} up to date"));
    }
    if !skipped.is_empty() {
        parts.push(format!("{} skipped: {}", skipped.len(), skipped.join("; ")));
    }
    if parts.is_empty() {
        "base only".to_owned()
    } else {
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::MapEnv;

    fn cache_dir_of(config: &OdxConfig, root: &Path, env: &dyn Env) -> PathBuf {
        cache_dir(config, &UserConfig::default(), root, env)
    }

    #[test]
    fn user_cache_dir_and_clones() {
        let user = odx_config::user::parse_user_config(
            "[cache]\ndir = \"~/c\"\n[clones]\ndirs = [\"~/none\"]\nmode = \"update\"\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let env = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            cache_dir(&config(""), &user, Path::new("/p"), &env),
            Path::new("/home/dev/c")
        );
        assert!(local_clones(&user, &env).is_empty());
        assert!(local_clones(&UserConfig::default(), &env).is_empty());
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

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_cache_dir() {
        let root = Path::new("/p");
        let xdg = MapEnv::default().with("XDG_CACHE_HOME", "/x");
        assert_eq!(cache_dir_of(&config(""), root, &xdg), Path::new("/x/odx"));
        let home = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            cache_dir_of(&config(""), root, &home),
            Path::new("/home/dev/.cache/odx")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cache_dir() {
        let home = MapEnv::default().with("HOME", "/Users/dev");
        assert_eq!(
            cache_dir_of(&config(""), Path::new("/p"), &home),
            Path::new("/Users/dev/Library/Caches/odx")
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cache_dir() {
        let env = MapEnv::default().with("LOCALAPPDATA", "C:/Users/dev/AppData/Local");
        assert_eq!(
            cache_dir_of(&config(""), Path::new("C:/p"), &env),
            Path::new("C:/Users/dev/AppData/Local")
                .join("odx")
                .join("cache")
        );
    }
}
