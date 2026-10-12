//! `odx aggregate --fetch-only`: fill the git cache.

use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use odx_aggregate::{RepoPlan, fetch_plans};
use odx_config::user::{CloneMode, UserConfig, expand_home, load_user_config};
use odx_config::{ConfigError, OdxConfig, Source, discover};
use odx_core::system::OsRunner;
use odx_core::{Env, Exit};
use odx_git::{
    Cache, CancellationToken, Clones, FetchPlan, GitBackend, GitError, Limits, Outcome, SystemGit,
    Via, fetch_all,
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

/// Run `fetch_all` on a runtime of its own, cancelling plans that have not
/// started when Ctrl-C is pressed.
///
/// Excluded from coverage: building the runtime only fails when the OS
/// refuses threads, and a real SIGINT cannot be raised inside the test
/// process. Everything it calls is tested on its own.
#[cfg_attr(coverage_nightly, coverage(off))]
fn fetch_with_ctrl_c(
    cache: &Cache,
    backend: &Arc<dyn GitBackend>,
    plans: Vec<FetchPlan>,
    limits: Limits,
) -> Vec<Outcome> {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            let message = e.to_string();
            return plans
                .into_iter()
                .map(|plan| Outcome {
                    url: plan.url,
                    result: Err(GitError::Io(std::io::Error::other(message.clone()))),
                })
                .collect();
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
        fetch_all(cache, backend, plans, limits, &cancel).await
    })
}

/// `odx aggregate --fetch-only [--repo NAME] [--prefer-mirror]`.
pub(crate) fn run_fetch_only(
    args: &AggregateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    plan_and_fetch(args, global, ctx).unwrap_or_else(|report| report)
}

fn plan_and_fetch(
    args: &AggregateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let project = match discover(ctx.fs, &ctx.dir, ctx.config) {
        Ok(Source::Pyproject(project)) => project,
        Ok(Source::ReposYaml { file, .. }) => {
            return Err(Report::error(
                Exit::Usage,
                format!(
                    "{}: fetching from repos.yaml arrives with the drop-in engine (roadmap phase 3)",
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
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::new(OsRunner)));
    let outcomes = fetch_with_ctrl_c(
        &cache,
        &backend,
        plans.iter().map(|p| p.plan.clone()).collect(),
        limits,
    );
    Ok(summarize(&plans, &outcomes, project.warnings))
}

/// One line per repository; failures end the command with exit code 1.
fn summarize(
    plans: &[RepoPlan],
    outcomes: &[Outcome],
    warnings: Vec<odx_config::Problem>,
) -> Report {
    let mut report = Report {
        warnings,
        ..Report::default()
    };
    let mut failures = Vec::new();
    for (plan, outcome) in plans.iter().zip(outcomes) {
        let names = plan.names.join(", ");
        match &outcome.result {
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
    if !failures.is_empty() {
        report.exit = Exit::Failure;
        report.error = Some(format!("fetching failed for\n  {}", failures.join("\n  ")));
    }
    report
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
