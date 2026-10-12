//! odx: a Rust-native workspace manager for Odoo projects.
//!
//! This crate is the `odx` binary. [`run`] parses the command line and
//! dispatches to the library crates; `main` only wires it to the process.

pub mod cli;

use std::ffi::OsString;
use std::io::{self, Write};

use clap::{CommandFactory, Parser};
use odx_core::Exit;

use crate::cli::{Cli, Command};

/// Parse `args` (including the program name) and run the command.
///
/// Command output goes to `out`, diagnostics to `err`.
pub fn run<I, T>(args: I, out: &mut dyn Write, err: &mut dyn Write) -> Exit
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let result = match Cli::try_parse_from(args) {
        Ok(cli) => execute(&cli, out, err),
        Err(error) => git_aggregate_rs::report_clap_error(&error, out, err),
    };
    result.unwrap_or(Exit::Failure)
}

fn execute(cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<Exit> {
    match &cli.command {
        Command::Completions(args) => {
            clap_complete::generate(args.shell, &mut Cli::command(), "odx", out);
            Ok(Exit::Success)
        }
        Command::GitAggregate(args) => git_aggregate_rs::execute(args, "odx git-aggregate", err),
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
