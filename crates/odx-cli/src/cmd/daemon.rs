//! `odx daemon`: `odx sync --all` as a user-level scheduled job, or in a
//! loop in the foreground.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::PathBuf;

use odx_config::Span;
use odx_config::user::{self, SyncSettings, load_user_config};
use odx_core::{Env, Exit, Invocation};
use odx_sync::schedule::{
    Job, Scheduler, Step, files, install_steps, status_step, uninstall_steps,
};
use odx_sync::set_sync;

use super::sync::{edit_user_config, last_sync, run_sync, shared};
use super::{Report, human, run_checked, size};
use crate::Context;
use crate::cli::{DaemonCommand, GlobalArgs, SyncArgs};

fn failed(exit: Exit, message: impl std::fmt::Display) -> Report {
    Report::error(exit, message.to_string())
}

/// The scheduler: `ODX_SCHEDULER` (`systemd`, `launchd`, `schtasks`), else
/// the platform's.
fn scheduler(env: &dyn Env) -> Result<Scheduler, Report> {
    env.var("ODX_SCHEDULER").map_or_else(
        || Ok(Scheduler::native()),
        |name| {
            Scheduler::parse(&name).ok_or_else(|| {
                failed(
                    Exit::Usage,
                    format!("unknown scheduler {name:?}: expected systemd, launchd or schtasks"),
                )
            })
        },
    )
}

/// Where the scheduler reads the job's files.
fn job_dir(scheduler: Scheduler, env: &dyn Env) -> Option<PathBuf> {
    match scheduler {
        Scheduler::Systemd => env
            .var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                env.var("HOME")
                    .map(|home| PathBuf::from(home).join(".config"))
            })
            .map(|dir| dir.join("systemd").join("user")),
        Scheduler::Launchd => env
            .var("HOME")
            .map(|home| PathBuf::from(home).join("Library").join("LaunchAgents")),
        Scheduler::TaskScheduler => user::config_dir(env),
    }
}

/// The time between syncs: `given`, else `[sync] interval`, else 30 minutes.
fn interval(given: Option<&String>, settings: &SyncSettings) -> Result<Span, Report> {
    given.map_or_else(
        || Ok(settings.interval.unwrap_or(Span::minutes(30))),
        |text| text.parse::<Span>().map_err(|e| failed(Exit::Usage, e)),
    )
}

/// Run `step` with its program from `ODX_SYSTEMCTL`, `ODX_LAUNCHCTL` or
/// `ODX_SCHTASKS` when set.
fn run_step(step: &Step, env: &dyn Env) -> Result<(), String> {
    let program = env
        .var(&format!("ODX_{}", step.program.to_ascii_uppercase()))
        .unwrap_or_else(|| step.program.to_owned());
    run_checked(
        &Invocation::new(&program).args(&step.args),
        &format!("{} {}", step.program, step.args.join(" ")),
    )
}

/// The job as it would be installed now.
fn job(ctx: &Context<'_>) -> Result<(Scheduler, SyncSettings, Job, PathBuf), Report> {
    let settings = load_user_config(ctx.fs, ctx.env)
        .map_err(|e| failed(Exit::Usage, e))?
        .sync
        .unwrap_or_default();
    let scheduler = scheduler(ctx.env)?;
    let dir = job_dir(scheduler, ctx.env).ok_or_else(|| {
        failed(
            Exit::Usage,
            "no directory for the job files; set HOME (or ODX_CONFIG_DIR on Windows)",
        )
    })?;
    let job = Job {
        exe: std::env::current_exe()
            .unwrap_or_default()
            .display()
            .to_string(),
        interval: 0,
        max_duration: settings
            .max_duration
            .map_or(20 * 60, |span| span.as_duration().as_secs()),
        nice: settings.nice.unwrap_or(true),
        pause_on_battery: settings.pause_on_battery.unwrap_or(false),
    };
    Ok((scheduler, settings, job, dir))
}

/// The installed scheduler, when its job files exist.
pub(crate) fn installed(ctx: &Context<'_>) -> Option<Scheduler> {
    let (scheduler, _, job, dir) = job(ctx).ok()?;
    files(scheduler, &job, &dir)
        .iter()
        .all(|file| file.path.exists())
        .then_some(scheduler)
}

/// `odx daemon install`, `uninstall` and `status`.
pub(crate) fn run_daemon(
    command: &DaemonCommand,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    match command {
        DaemonCommand::Install { interval, dry_run } => install(interval.as_ref(), *dry_run, ctx),
        DaemonCommand::Uninstall => uninstall(ctx),
        DaemonCommand::Status | DaemonCommand::Run { .. } => status(global, ctx),
    }
    .unwrap_or_else(|report| report)
}

fn install(given: Option<&String>, dry_run: bool, ctx: &Context<'_>) -> Result<Report, Report> {
    let (scheduler, settings, mut job, dir) = job(ctx)?;
    let span = interval(given, &settings)?;
    let every = span.to_string();
    job.interval = span.as_duration().as_secs();
    let job_files = files(scheduler, &job, &dir);
    let steps = install_steps(scheduler, &job_files);
    let mut report = Report::default();
    if dry_run {
        for file in &job_files {
            let _ = writeln!(report.out, "# {}\n{}", file.path.display(), file.text);
        }
        for step in &steps {
            let _ = writeln!(report.out, "$ {} {}", step.program, step.args.join(" "));
        }
        return Ok(report);
    }
    for file in &job_files {
        file.path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file.path, file.bytes()))
            .map_err(|e| failed(Exit::Failure, format!("{}: {e}", file.path.display())))?;
    }
    for step in &steps {
        let result = run_step(step, ctx.env);
        if step.required {
            result.map_err(|e| failed(Exit::Failure, e))?;
        }
    }
    edit_user_config(ctx, |doc| set_sync(doc, true, Some(&every)))?;
    let _ = writeln!(
        report.out,
        "installed the {}: `odx sync --all` every {every}",
        scheduler.describe()
    );
    for file in &job_files {
        let _ = writeln!(report.out, "  {}", file.path.display());
    }
    Ok(report)
}

fn uninstall(ctx: &Context<'_>) -> Result<Report, Report> {
    let (scheduler, _, job, dir) = job(ctx)?;
    let job_files = files(scheduler, &job, &dir);
    if !job_files.iter().any(|file| file.path.exists()) {
        return Ok(Report {
            out: "background sync is not installed\n".to_owned(),
            ..Report::default()
        });
    }
    for step in uninstall_steps(scheduler, &job_files) {
        // The job may be stopped already; removing its files is what counts.
        let _ = run_step(&step, ctx.env);
    }
    for file in job_files.iter().filter(|file| file.path.exists()) {
        std::fs::remove_file(&file.path)
            .map_err(|e| failed(Exit::Failure, format!("{}: {e}", file.path.display())))?;
    }
    edit_user_config(ctx, |doc| set_sync(doc, false, None))?;
    Ok(Report {
        out: format!("removed the {}\n", scheduler.describe()),
        ..Report::default()
    })
}

fn status(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let (_, cache) = shared(ctx)?;
    let (scheduler, settings, _, _) = job(ctx)?;
    let every = settings.interval.unwrap_or(Span::minutes(30));
    let state = match installed(ctx) {
        None => "not installed",
        Some(_) if run_step(&status_step(scheduler), ctx.env).is_ok() => "active",
        Some(_) => "installed, not active",
    };
    let mut report = Report::default();
    if global.json {
        report.out = format!(
            "{}\n",
            serde_json::json!({
                "schema": 1,
                "scheduler": scheduler.describe(),
                "state": state,
                "interval": every.to_string(),
                "last_sync": last_sync(&cache),
                "cache": cache.display().to_string(),
                "cache_size": size(&cache),
            })
        );
    } else {
        let _ = writeln!(report.out, "job: {} ({state})", scheduler.describe());
        let _ = writeln!(report.out, "interval: {every}");
        let _ = writeln!(report.out, "last sync: {}", last_sync(&cache));
        let _ = writeln!(
            report.out,
            "cache: {} ({})",
            cache.display(),
            human(size(&cache))
        );
    }
    Ok(report)
}

/// `odx daemon run`: `odx sync --all`, then wait, forever.
// The loop never ends by design, so it cannot run under test; each round is
// `odx sync --all` and the interval comes from `interval`, both tested.
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) fn run_forever(
    given: Option<&String>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    let settings = load_user_config(ctx.fs, ctx.env)
        .ok()
        .and_then(|user| user.sync)
        .unwrap_or_default();
    let pause = match interval(given, &settings) {
        Ok(span) => span.as_duration(),
        Err(report) => return report.emit(out, err),
    };
    let all = SyncArgs {
        register: false,
        unregister: false,
        all: true,
        workspace: Vec::new(),
        max_duration: None,
    };
    loop {
        run_sync(&all, global, ctx).emit(out, err)?;
        out.flush()?;
        std::thread::sleep(pause);
    }
}
