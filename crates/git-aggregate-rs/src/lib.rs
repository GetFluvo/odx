//! Drop-in replacement for git-aggregator's `gitaggregate`, written in Rust.
//!
//! The command line mirrors `gitaggregate`: the same `repos.yaml` format,
//! the same actions (`aggregate`, `show-closed-prs`, `show-all-prs`) and the
//! same flags, plus a few opt-in extensions (`--allow-shell`, `--lock`,
//! `--mergiraf`, `--json`). The `odx` binary exposes the same interface as
//! `odx git-aggregate`.
//!
//! In the workspace skeleton every action parses its arguments and then
//! reports that it is not implemented yet; the engine lands in roadmap
//! phase 3.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;

use clap::{ArgAction, Parser, ValueEnum};
use odx_core::Exit;

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

    /// Number of repositories to process in parallel.
    #[arg(short, long, value_name = "N", default_value_t = 1)]
    pub jobs: usize,

    /// Only process target directories matching this glob.
    #[arg(short, long, value_name = "GLOB")]
    pub dirmatch: Option<String>,

    /// Push each aggregated target branch to its remote.
    #[arg(short, long)]
    pub push: bool,

    /// Expand `$VAR` and `${VAR}` in the repositories file.
    #[arg(long)]
    pub expand_env: bool,

    /// Load environment variables from FILE before expanding.
    #[arg(short, long, value_name = "FILE")]
    pub env_file: Option<PathBuf>,

    /// Discard local changes in the target directories.
    #[arg(short, long)]
    pub force: bool,

    /// Logging verbosity.
    #[arg(long, value_enum, ignore_case = true, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,

    /// Disable colored output (also honors `NO_COLOR`).
    #[arg(long)]
    pub no_color: bool,

    /// Allow `shell_command_after` entries to run (off by default).
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_shell: bool,

    /// Write a reproducible lockfile with the resolved commits.
    #[arg(long, value_name = "FILE")]
    pub lock: Option<PathBuf>,

    /// Use mergiraf for syntax-aware conflict resolution.
    #[arg(long)]
    pub mergiraf: bool,

    /// Machine-readable output on stdout.
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
    /// Everything, including fetch and merge details.
    Debug,
    /// Progress per repository.
    Info,
    /// Warnings and errors.
    Warning,
    /// Errors only.
    Error,
}

/// Execute an already parsed command line.
///
/// # Errors
///
/// Returns an error when writing to `err` fails.
pub fn execute(cli: &Cli, prefix: &str, err: &mut dyn Write) -> io::Result<Exit> {
    writeln!(
        err,
        "{prefix} {}: not implemented yet (roadmap phase 3)",
        cli.action.name()
    )?;
    Ok(Exit::Usage)
}

/// Parse `args` (including the program name) and run the command.
///
/// Help and version output go to `out`, diagnostics to `err`.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let result = match Cli::try_parse_from(args) {
        Ok(cli) => execute(&cli, "git-aggregate-rs", err),
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

    fn run_args(args: &[&str]) -> (Exit, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let exit = run(args, &mut out, &mut err);
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
            "--expand-env",
            "-e",
            ".env",
            "-f",
            "--log-level",
            "DEBUG",
            "show-closed-prs",
        ])
        .unwrap();
        assert_eq!(cli.action, Action::ShowClosedPrs);
        assert_eq!(cli.jobs, 4);
        assert_eq!(cli.log_level, LogLevel::Debug);
        assert!(cli.push && cli.force && cli.expand_env);
    }

    #[test]
    fn every_action_is_not_implemented_yet() {
        for action in ["aggregate", "show-closed-prs", "show-all-prs"] {
            let (exit, out, err) = run_args(&["git-aggregate-rs", action]);
            assert_eq!(exit, Exit::Usage);
            assert_eq!(out, "");
            assert_eq!(
                err,
                format!("git-aggregate-rs {action}: not implemented yet (roadmap phase 3)\n")
            );
        }
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
        let exit = run(["git-aggregate-rs"], &mut Broken, &mut Broken);
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
