//! Drop-in replacement for git-aggregator's `gitaggregate`, written in Rust.
//!
//! The command line mirrors `gitaggregate`: the same `repos.yaml` format,
//! the same actions (`aggregate`, `show-closed-prs`, `show-all-prs`) and the
//! same flags, plus a few opt-in extensions (`--allow-shell`, `--mergiraf`,
//! `--json`). The `odx` binary exposes the same interface as
//! `odx git-aggregate`.
//!
//! Every remote is fetched once into a shared cache of bare repositories
//! (the odx cache), concurrently; each target directory becomes a detached
//! working tree of that cache with the merges applied. `--push` pushes the
//! result to the target remote's branch.

mod aggregate;
mod dirmatch;

pub use dirmatch::fnmatch;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};
use odx_core::{Env, Exit, FileSystem};

/// Command-line interface of `git-aggregate-rs`.
#[derive(Debug, Parser)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
#[command(
    name = "git-aggregate-rs",
    version,
    about = "Aggregate git branches and pull requests from repos.yaml (gitaggregate drop-in)"
)]
pub struct Cli {
    /// Action to run.
    #[arg(value_enum, default_value_t = Action::Aggregate)]
    pub action: Action,

    /// Path to the repositories file.
    #[arg(short, long, value_name = "FILE", default_value = "repos.yaml")]
    pub config: PathBuf,

    /// Push the result to the target remote and branch.
    #[arg(short, long)]
    pub push: bool,

    /// Only process target directories matching this fnmatch pattern.
    #[arg(short, long, value_name = "PATTERN")]
    pub dirmatch: Option<String>,

    /// Logging verbosity.
    #[arg(long, value_enum, ignore_case = true, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,

    /// Expand `$VAR` and `${VAR}` in the repositories file.
    #[arg(short, long)]
    pub expand_env: bool,

    /// Variables for --expand-env (the environment takes precedence).
    #[arg(long, value_name = "FILE")]
    pub env_file: Option<PathBuf>,

    /// Discard local changes, and replace directories not built by odx.
    #[arg(short, long)]
    pub force: bool,

    /// Repositories fetched and built at once [default: number of CPUs].
    #[arg(short, long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Disable colored output.
    #[arg(long)]
    pub no_color: bool,

    /// Run `shell_command_after` entries (skipped with a warning otherwise).
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_shell: bool,

    /// Resolve conflicts with mergiraf (`ODX_MERGIRAF` names the executable).
    #[arg(long)]
    pub mergiraf: bool,

    /// Machine-readable summary on stdout.
    #[arg(long)]
    pub json: bool,
}

/// The `gitaggregate` actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Action {
    /// Fetch and merge the configured branches and pull requests.
    Aggregate,
    /// List pull requests in the configuration that are closed or merged.
    ShowClosedPrs,
    /// List every pull request in the configuration with its state.
    ShowAllPrs,
}

impl Action {
    /// The action name as typed on the command line.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Aggregate => "aggregate",
            Self::ShowClosedPrs => "show-closed-prs",
            Self::ShowAllPrs => "show-all-prs",
        }
    }
}

/// Logging levels accepted by `--log-level`, case-insensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    /// Everything.
    Debug,
    /// Progress per repository.
    Info,
    /// Warnings and errors.
    Warning,
    /// Errors only.
    Error,
    /// Errors only (git-aggregator's highest level).
    Critical,
}

/// What a run needs from its environment, so tests can supply fakes.
#[derive(Debug)]
pub struct Context<'a> {
    /// File system access, for the repositories, env and user files.
    pub fs: &'a dyn FileSystem,
    /// Environment variables.
    pub env: &'a dyn Env,
    /// The directory target directories are relative to.
    pub dir: PathBuf,
}

/// Execute an already parsed command line; `prefix` names the command in
/// messages.
///
/// # Errors
///
/// Returns an error when writing to `out` or `err` fails.
pub fn execute(
    cli: &Cli,
    prefix: &str,
    ctx: &Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    match cli.action {
        Action::Aggregate => aggregate::aggregate(cli, ctx, out, err),
        action => {
            writeln!(
                err,
                "{prefix} {}: not implemented yet (roadmap phase 7)",
                action.name()
            )?;
            Ok(Exit::Usage)
        }
    }
}

/// Parse `args` (including the program name) and run the command in the
/// current directory, with the real file system and environment.
///
/// Help and version output go to `out`, diagnostics to `err`.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let ctx = Context {
        fs: &odx_core::system::OsFs,
        env: &odx_core::system::OsEnv,
        dir: std::env::current_dir().unwrap_or_default(),
    };
    run_with(args, &ctx, out, err)
}

/// Like [`run`], with an explicit context.
pub fn run_with<I, T>(args: I, ctx: &Context<'_>, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let result = match Cli::try_parse_from(args) {
        Ok(cli) => execute(&cli, "git-aggregate-rs", ctx, out, err),
        Err(error) => report_clap_error(&error, out, err),
    };
    result.unwrap_or(Exit::Failure)
}

/// Print a clap parse outcome: help and version to `out` with exit `0`,
/// usage errors to `err` with exit `2`.
///
/// # Errors
///
/// Returns an error when writing fails.
pub fn report_clap_error(
    error: &clap::Error,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    let rendered = error.render().to_string();
    if error.use_stderr() {
        err.write_all(rendered.as_bytes())?;
        Ok(Exit::Usage)
    } else {
        out.write_all(rendered.as_bytes())?;
        Ok(Exit::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::{MapEnv, MemoryFs};

    fn run_args(args: &[&str]) -> (Exit, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let ctx = Context {
            fs: &MemoryFs::default(),
            env: &MapEnv::default(),
            dir: PathBuf::from("/work"),
        };
        let exit = run_with(args, &ctx, &mut out, &mut err);
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn default_action_is_aggregate() {
        let cli = Cli::try_parse_from(["git-aggregate-rs"]).unwrap();
        assert_eq!(cli.action, Action::Aggregate);
        assert_eq!(cli.config, PathBuf::from("repos.yaml"));
        assert_eq!(cli.jobs, None);
        assert_eq!(Action::Aggregate.name(), "aggregate");
    }

    #[test]
    fn gitaggregate_flags_parse() {
        let cli = Cli::try_parse_from([
            "git-aggregate-rs",
            "-c",
            "custom.yaml",
            "-j",
            "4",
            "-d",
            "*/sale-*",
            "-p",
            "-e",
            "--env-file",
            ".env",
            "-f",
            "--no-color",
            "--log-level",
            "CRITICAL",
            "show-closed-prs",
        ])
        .unwrap();
        assert_eq!(cli.action, Action::ShowClosedPrs);
        assert_eq!(cli.jobs, Some(4));
        assert_eq!(cli.log_level, LogLevel::Critical);
        assert_eq!(cli.env_file, Some(PathBuf::from(".env")));
        assert!(cli.push && cli.force && cli.expand_env && cli.no_color);
    }

    #[test]
    fn pull_request_actions_are_not_implemented_yet() {
        for action in ["show-closed-prs", "show-all-prs"] {
            let (exit, out, err) = run_args(&["git-aggregate-rs", action]);
            assert_eq!(exit, Exit::Usage);
            assert_eq!(out, "");
            assert_eq!(
                err,
                format!("git-aggregate-rs {action}: not implemented yet (roadmap phase 7)\n")
            );
        }
    }

    #[test]
    fn missing_configuration_fails() {
        let (exit, out, err) = run_args(&["git-aggregate-rs"]);
        assert_eq!(exit, Exit::Failure);
        assert_eq!(out, "");
        assert!(err.starts_with("error: cannot read "), "{err}");
    }

    #[test]
    fn help_goes_to_stdout_with_success() {
        let (exit, out, err) = run_args(&["git-aggregate-rs", "--help"]);
        assert_eq!(exit, Exit::Success);
        assert!(out.contains("gitaggregate drop-in"));
        assert_eq!(err, "");
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        let (exit, out, err) = run_args(&["git-aggregate-rs", "--bogus"]);
        assert_eq!(exit, Exit::Usage);
        assert_eq!(out, "");
        assert!(err.contains("--bogus"));
    }

    #[test]
    fn write_failure_maps_to_failure() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(Broken.flush().is_ok());
        let exit = run(
            ["git-aggregate-rs", "show-all-prs"],
            &mut Broken,
            &mut Broken,
        );
        assert_eq!(exit, Exit::Failure);
        let exit = run(["git-aggregate-rs", "--help"], &mut Broken, &mut Broken);
        assert_eq!(exit, Exit::Failure);
        let exit = run(
            ["git-aggregate-rs", "--bogus"],
            &mut Vec::new(),
            &mut Broken,
        );
        assert_eq!(exit, Exit::Failure);
    }
}
