//! The `aggregate` action: read `repos.yaml`, fetch into the shared cache,
//! build every selected target directory, then run `shell_command_after`
//! (with `--allow-shell`) and push (with `--push`).

use std::cell::RefCell;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use odx_aggregate::{
    BuildOptions, PlanError, RepoAggregation, local_clones, mergiraf_program, run_pipeline,
    yaml_aggregations, yaml_fetch_plans,
};
use odx_config::compat::{RepoSpec, expand_env, parse_env_file, parse_repos};
use odx_config::schema::MergirafMode;
use odx_config::user::{self, load_user_config};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner, repo_key};
use odx_git::{
    Aggregated, Aggregation, Cache, Fetched, GitBackend, Limits, MergeError, Merger, Outcome,
    SystemGit,
};

use crate::dirmatch::match_dir;
use crate::{Cli, Context, LogLevel};

/// How long to wait for another process's repository lock.
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Fetches running at once per host.
const PER_HOST: usize = 6;

/// The shell `shell_command_after` entries run in, as git-aggregator's
/// `shell=True` does.
#[cfg(not(windows))]
const SHELL: [&str; 2] = ["sh", "-c"];
#[cfg(windows)]
const SHELL: [&str; 2] = ["cmd", "/C"];

/// What happened to one target directory.
struct Built {
    dir: String,
    result: Result<Aggregated, String>,
    pushed: Option<String>,
}

/// Diagnostics, filtered by `--log-level`.
struct Log {
    level: LogLevel,
    text: String,
}

impl Log {
    fn info(&mut self, message: &str) {
        if matches!(self.level, LogLevel::Debug | LogLevel::Info) {
            let _ = writeln!(self.text, "{message}");
        }
    }

    fn warning(&mut self, message: &str) {
        if !matches!(self.level, LogLevel::Error | LogLevel::Critical) {
            let _ = writeln!(self.text, "warning: {message}");
        }
    }

    fn error(&mut self, message: &str) {
        let _ = writeln!(self.text, "error: {message}");
    }
}

/// The `repos.yaml` text, with `$VAR` / `${VAR}` expanded when
/// `--expand-env` is given (the environment wins over `--env-file`).
fn load_text(cli: &Cli, ctx: &Context<'_>, log: &mut Log) -> Result<String, String> {
    let path = ctx.dir.join(&cli.config);
    let text = ctx
        .fs
        .read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !cli.expand_env {
        if cli.env_file.is_some() {
            log.warning("--env-file has no effect without --expand-env");
        }
        return Ok(text);
    }
    let from_file = cli.env_file.as_ref().map_or_else(Vec::new, |file| {
        let path = ctx.dir.join(file);
        ctx.fs.read(&path).map_or_else(
            |_| {
                log.warning(&format!("{}: not found; ignored", path.display()));
                Vec::new()
            },
            |bytes| parse_env_file(&String::from_utf8_lossy(&bytes)),
        )
    });
    let missing = RefCell::new(Vec::<String>::new());
    let lookup = |name: &str| {
        let value = ctx.env.var(name).or_else(|| {
            from_file
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        });
        if value.is_none() && !missing.borrow().iter().any(|known| known == name) {
            missing.borrow_mut().push(name.to_owned());
        }
        value
    };
    let expanded = expand_env(&text, &lookup);
    for name in missing.into_inner() {
        log.warning(&format!("${name} is not set; left as written"));
    }
    Ok(expanded)
}

/// Run the `aggregate` action.
///
/// # Errors
///
/// When writing to `out` or `err` fails.
pub(crate) fn aggregate(
    cli: &Cli,
    ctx: &Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    let mut log = Log {
        level: cli.log_level,
        text: String::new(),
    };
    let exit = match prepare_and_run(cli, ctx, &mut log) {
        Ok(built) => {
            if cli.json {
                writeln!(out, "{}", to_json(&built))?;
            }
            report(&built, &mut log)
        }
        Err((exit, message)) => {
            log.error(&message);
            exit
        }
    };
    err.write_all(log.text.as_bytes())?;
    Ok(exit)
}

/// Exit status and per-directory lines for the log.
fn report(built: &[Built], log: &mut Log) -> Exit {
    let mut exit = Exit::Success;
    for repo in built {
        match &repo.result {
            Ok(aggregated) => {
                log.info(&format!(
                    "{}: {} ({})",
                    repo.dir,
                    aggregated.describe(),
                    aggregated.head.get(..12).unwrap_or(&aggregated.head)
                ));
                if let Some(pushed) = &repo.pushed {
                    log.info(&format!("{}: pushed to {pushed}", repo.dir));
                }
            }
            Err(message) => {
                log.error(&format!("{}: {message}", repo.dir));
                exit = Exit::Failure;
            }
        }
    }
    exit
}

fn to_json(built: &[Built]) -> serde_json::Value {
    let repos: Vec<serde_json::Value> = built
        .iter()
        .map(|repo| match &repo.result {
            Ok(aggregated) => serde_json::json!({
                "dir": repo.dir,
                "head": aggregated.head,
                "tree": aggregated.tree,
                "merges": aggregated
                    .steps
                    .iter()
                    .map(|(label, outcome)| serde_json::json!({ "label": label, "status": outcome.status() }))
                    .collect::<Vec<_>>(),
                "pushed": repo.pushed,
            }),
            Err(message) => serde_json::json!({ "dir": repo.dir, "error": message }),
        })
        .collect();
    serde_json::json!({ "schema": 1, "repos": repos })
}

type Failed = (Exit, String);

fn prepare_and_run(cli: &Cli, ctx: &Context<'_>, log: &mut Log) -> Result<Vec<Built>, Failed> {
    let text = load_text(cli, ctx, log).map_err(|e| (Exit::Failure, e))?;
    let file = parse_repos(&text)
        .map_err(|e| (Exit::Failure, format!("{}: {e}", cli.config.display())))?;
    let (selected, skipped): (Vec<RepoSpec>, Vec<RepoSpec>) =
        file.repos.into_iter().partition(|repo| {
            cli.dirmatch
                .as_deref()
                .is_none_or(|pattern| match_dir(&ctx.dir, &repo.dir, pattern))
        });
    for repo in &skipped {
        log.info(&format!("Skip {}", repo.dir));
    }
    let user = load_user_config(ctx.fs, ctx.env).map_err(|e| (Exit::Failure, e.to_string()))?;
    let runner = Arc::new(OsRunner);
    let mergiraf = if cli.mergiraf {
        mergiraf_program(MergirafMode::Always, ctx.env, runner.as_ref()).map_err(|_| {
            (
                Exit::Usage,
                "--mergiraf is given, but mergiraf cannot be run (set ODX_MERGIRAF to its path)"
                    .to_owned(),
            )
        })?
    } else {
        None
    };
    let cache = Cache::new(
        user::cache_dir(&user, ctx.env).unwrap_or_else(|| ctx.dir.join(".odx").join("cache")),
    );
    let clones = local_clones(&user, ctx.env);
    let mut plans = yaml_fetch_plans(&selected);
    for plan in &mut plans {
        plan.plan.clone = clones.find(&plan.plan.url).cloned();
    }
    let limits = Limits {
        jobs: cli
            .jobs
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, NonZeroUsize::get)),
        per_host: PER_HOST,
        lock_timeout: LOCK_TIMEOUT,
    };
    let options = BuildOptions {
        mergiraf,
        keep_conflicts: false,
        force: cli.force,
    };
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::<OsRunner>::clone(&runner)));
    let merger = Arc::new(Merger::new(runner));
    let planned = RefCell::new(Vec::new());
    let build = |outcomes: &[Outcome]| {
        let fetched: Vec<(String, Vec<Fetched>)> = outcomes
            .iter()
            .filter_map(|o| o.result.as_ref().ok().map(|f| (o.url.clone(), f.clone())))
            .collect();
        let repos = yaml_aggregations(&selected, &ctx.dir, &cache, &fetched, &options);
        let runnable: Vec<Aggregation> = repos
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|r| r.aggregation.clone()))
            .collect();
        *planned.borrow_mut() = repos;
        runnable
    };
    let plans = plans.into_iter().map(|p| p.plan).collect();
    let (outcomes, results) = run_pipeline(&cache, &backend, plans, limits, Some(&merger), &build);
    let planned = planned.into_inner();
    Ok(finish(
        cli, &selected, &outcomes, &planned, results, &merger, log,
    ))
}

/// Pair every selected directory with its result, then run its shell
/// commands and push it.
fn finish(
    cli: &Cli,
    selected: &[RepoSpec],
    outcomes: &[Outcome],
    planned: &[Result<RepoAggregation, PlanError>],
    results: Vec<Result<Aggregated, MergeError>>,
    merger: &Merger,
    log: &mut Log,
) -> Vec<Built> {
    let entries = || selected.iter().zip(planned).enumerate();
    // `results` has one entry per planned aggregation, in order.
    let mut built: Vec<(usize, Built)> = entries()
        .filter_map(|(i, (repo, plan))| plan.as_ref().ok().map(|plan| (i, repo, plan)))
        .zip(results)
        .map(|((i, repo, plan), result)| {
            let result = result.map_err(|e| e.to_string()).and_then(|aggregated| {
                after(cli, repo, &plan.aggregation.target_dir, merger, log)
                    .map(|pushed| (aggregated, pushed))
            });
            let (result, pushed) = match result {
                Ok((aggregated, pushed)) => (Ok(aggregated), pushed),
                Err(message) => (Err(message), None),
            };
            (
                i,
                Built {
                    dir: repo.dir.clone(),
                    result,
                    pushed,
                },
            )
        })
        .collect();
    built.extend(entries().filter_map(|(i, (repo, plan))| {
        plan.as_ref().err().map(|e| {
            (
                i,
                Built {
                    dir: repo.dir.clone(),
                    result: Err(fetch_error(repo, outcomes, e)),
                    pushed: None,
                },
            )
        })
    }));
    built.sort_by_key(|(i, _)| *i);
    built.into_iter().map(|(_, built)| built).collect()
}

/// Why `repo` could not be built: the error fetching its base URL, else
/// the planning error.
fn fetch_error(repo: &RepoSpec, outcomes: &[Outcome], planning: &PlanError) -> String {
    let key = repo_key(&repo.base.url);
    outcomes
        .iter()
        .filter(|o| repo_key(&o.url) == key)
        .filter_map(|o| o.result.as_ref().err())
        .fold(planning.to_string(), |_, fetching| fetching.to_string())
}

/// `shell_command_after` (only with `--allow-shell`), then the push (with
/// `--push`); returns where the result was pushed.
fn after(
    cli: &Cli,
    repo: &RepoSpec,
    target: &Path,
    merger: &Merger,
    log: &mut Log,
) -> Result<Option<String>, String> {
    if !repo.shell_command_after.is_empty() {
        if cli.allow_shell {
            for command in &repo.shell_command_after {
                run_shell(command, target)?;
            }
        } else {
            log.warning(&format!(
                "{}: shell_command_after not run; pass --allow-shell to run it",
                repo.dir
            ));
        }
    }
    if !cli.push {
        return Ok(None);
    }
    let Some((remote, branch)) = repo.target.as_ref().and_then(|target| {
        target
            .remote
            .as_deref()
            .map(|remote| (remote, &target.branch))
    }) else {
        return Err("cannot push: no target remote configured".to_owned());
    };
    // Target remotes are validated when repos.yaml is read.
    let url = repo.remote_url(remote).unwrap_or(remote);
    merger
        .push(target, url, branch)
        .map_err(|e| format!("push to {remote} failed: {e}"))?;
    Ok(Some(format!("{remote} {branch}")))
}

fn run_shell(command: &str, dir: &Path) -> Result<(), String> {
    let [shell, flag] = SHELL;
    let invocation = Invocation::new(shell)
        .arg(flag)
        .arg(command)
        .current_dir(dir);
    match OsRunner.run(&invocation) {
        Ok(output) if output.success() => Ok(()),
        Ok(output) => Err(format!(
            "shell_command_after `{command}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!("shell_command_after `{command}` failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use odx_core::FileSystem;
    use odx_core::fake::{MapEnv, MemoryFs};

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_failures_are_reported() {
        let fs = MemoryFs::default();
        fs.write(
            Path::new("/work/repos.yaml"),
            b"./pos:\n  remotes:\n    oca: u\n  merges:\n    - oca 17.0\n",
        )
        .unwrap();
        let ctx = Context {
            fs: &fs,
            env: &MapEnv::default(),
            dir: "/work".into(),
        };
        let cli = Cli::try_parse_from(["g", "--json", "-d", "nothing"]).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            aggregate(&cli, &ctx, &mut out, &mut Vec::new()).unwrap(),
            Exit::Success
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"repos\":[],\"schema\":1}\n"
        );
        assert!(aggregate(&cli, &ctx, &mut Broken, &mut Vec::new()).is_err());
        assert!(aggregate(&cli, &ctx, &mut Vec::new(), &mut Broken).is_err());
        assert!(Broken.flush().is_ok());
    }

    #[test]
    fn shell_commands_that_cannot_start() {
        let error = run_shell("true", Path::new("/nonexistent/odx-test")).unwrap_err();
        assert!(
            error.starts_with("shell_command_after `true` failed: "),
            "{error}"
        );
    }
}
