use std::path::PathBuf;
use std::sync::Arc;

use std::ffi::OsString;

use odx_config::schema::MergirafMode;
use odx_config::user::{CloneMode, UserConfig, expand_home};
use odx_core::{Env, Invocation, ProcessRunner};
use odx_git::{
    Aggregated, Aggregation, Cache, CancellationToken, Clones, FetchPlan, GitBackend, GitError,
    Limits, MergeError, Merger, Outcome, Progress, aggregate_all, fetch_all,
};

/// The local clones configured in `[clones]` of the user configuration.
pub fn local_clones(user: &UserConfig, env: &dyn Env) -> Clones {
    user.clones.as_ref().map_or_else(Clones::default, |clones| {
        let dirs: Vec<PathBuf> = clones
            .dirs
            .iter()
            .map(|dir| expand_home(dir, env))
            .collect();
        Clones::scan(&dirs, clones.mode == Some(CloneMode::Update))
    })
}

/// The mergiraf executable to use: `ODX_MERGIRAF` or `mergiraf`, when the
/// configured mode wants it and it runs.
///
/// # Errors
///
/// A message when the mode is `always` and the executable cannot be run.
pub fn mergiraf_program(
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

/// Fetch every plan, then build the working trees `build` derives from the
/// outcomes (none without a `merger`), on a runtime of its own; pending work
/// is cancelled when Ctrl-C is pressed.
///
/// `progress` hears of each fetch and build as it starts and ends.
///
/// Excluded from coverage: building the runtime only fails when the OS
/// refuses threads, and a real SIGINT cannot be raised inside the test
/// process. Everything it calls is tested on its own.
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn run_pipeline(
    cache: &Cache,
    backend: &Arc<dyn GitBackend>,
    plans: Vec<FetchPlan>,
    limits: Limits,
    merger: Option<&Arc<Merger>>,
    build: &dyn Fn(&[Outcome]) -> Vec<Aggregation>,
    progress: &(dyn Fn(Progress<'_>) + Sync),
) -> (Vec<Outcome>, Vec<Result<Aggregated, MergeError>>) {
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
        let outcomes = fetch_all(cache, backend, plans, limits, &cancel, progress).await;
        let results = match merger {
            Some(merger) => {
                aggregate_all(merger, build(&outcomes), limits.jobs, &cancel, progress).await
            }
            None => Vec::new(),
        };
        (outcomes, results)
    })
}

/// Fetch every plan on a runtime of its own (see [`run_pipeline`]).
///
/// Excluded from coverage for the same reasons as [`run_pipeline`].
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn run_fetches(
    cache: &Cache,
    backend: &Arc<dyn GitBackend>,
    plans: Vec<FetchPlan>,
    limits: Limits,
    progress: &(dyn Fn(Progress<'_>) + Sync),
) -> Vec<Outcome> {
    run_pipeline(
        cache,
        backend,
        plans,
        limits,
        None,
        &|_| Vec::new(),
        progress,
    )
    .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::Output;
    use odx_core::fake::{MapEnv, ScriptedRunner};
    use std::path::Path;

    fn status(code: i32) -> Output {
        Output {
            status: Some(code),
            ..Output::default()
        }
    }

    #[test]
    fn mergiraf_modes() {
        let env = MapEnv::default().with("ODX_MERGIRAF", "/opt/mergiraf");
        let runner = ScriptedRunner::default();
        assert_eq!(
            mergiraf_program(MergirafMode::Never, &env, &runner),
            Ok(None)
        );
        runner.push(Ok(status(0)));
        assert_eq!(
            mergiraf_program(MergirafMode::Auto, &env, &runner),
            Ok(Some("/opt/mergiraf".into()))
        );
        assert_eq!(runner.invocations()[0].program(), "/opt/mergiraf");
        assert_eq!(
            mergiraf_program(MergirafMode::Auto, &MapEnv::default(), &runner),
            Ok(None)
        );
        assert_eq!(runner.invocations()[1].program(), "mergiraf");
        runner.push(Ok(status(0)));
        assert_eq!(
            mergiraf_program(MergirafMode::Always, &env, &runner),
            Ok(Some("/opt/mergiraf".into()))
        );
        runner.push(Ok(status(1)));
        assert_eq!(
            mergiraf_program(MergirafMode::Always, &env, &runner),
            Err("mergiraf = \"always\" is configured, but /opt/mergiraf cannot be run".to_owned())
        );
    }

    #[test]
    fn clones_from_the_user_configuration() {
        let user = odx_config::user::parse_user_config(
            "[clones]\ndirs = [\"~/none\"]\nmode = \"update\"\n",
            Path::new("config.toml"),
        )
        .unwrap();
        let env = MapEnv::default().with("HOME", "/home/dev");
        assert!(local_clones(&user, &env).is_empty());
        assert!(local_clones(&UserConfig::default(), &env).is_empty());
    }
}
