//! odx: a Rust-native workspace manager for Odoo projects.
//!
//! This crate is the `odx` binary. [`run`] parses the command line and
//! dispatches to the library crates; `main` only wires it to the process.

pub mod cli;
mod cmd;
pub mod system;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser};
use odx_core::{Exit, FileSystem};

use crate::cli::{Cli, Command};

/// What a command needs from its environment, so tests can supply fakes.
#[derive(Debug)]
pub struct Context<'a> {
    /// File system access.
    pub fs: &'a dyn FileSystem,
    /// The directory odx runs in (after `-C`).
    pub dir: PathBuf,
    /// The configuration file given with `--config`, if any.
    pub config: Option<&'a Path>,
}

/// Parse `args` (including the program name) and run the command against
/// the real file system and working directory.
///
/// Command output goes to `out`, diagnostics to `err`.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cwd = std::env::current_dir().unwrap_or_default();
    run_in(args, &system::OsFs, &cwd, out, err)
}

/// Like [`run`], with an explicit file system and working directory.
pub fn run_in<I, T>(
    args: I,
    fs: &dyn FileSystem,
    cwd: &Path,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let result = match Cli::try_parse_from(args) {
        Ok(cli) => {
            let ctx = Context {
                fs,
                dir: cli
                    .global
                    .project_dir
                    .as_ref()
                    .map_or_else(|| cwd.to_path_buf(), |dir| cwd.join(dir)),
                config: cli.global.config.as_deref(),
            };
            execute(&cli, &ctx, out, err)
        }
        Err(error) => git_aggregate_rs::report_clap_error(&error, out, err),
    };
    result.unwrap_or(Exit::Failure)
}

fn execute(
    cli: &Cli,
    ctx: &Context<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    match &cli.command {
        Command::Completions(args) => {
            clap_complete::generate(args.shell, &mut Cli::command(), "odx", out);
            Ok(Exit::Success)
        }
        Command::GitAggregate(args) => git_aggregate_rs::execute(args, "odx git-aggregate", err),
        Command::Import(args) => cmd::compat::run_import(args, ctx).emit(out, err),
        Command::Export(args) => cmd::compat::run_export(args, ctx).emit(out, err),
        Command::Add(args) => cmd::edit::run_add(args, ctx).emit(out, err),
        Command::Drop(args) => cmd::edit::run_drop(args, ctx).emit(out, err),
        command => {
            writeln!(
                err,
                "odx {}: not implemented yet (roadmap phase {})",
                command.name(),
                command.phase()
            )?;
            Ok(Exit::Usage)
        }
    }
}
