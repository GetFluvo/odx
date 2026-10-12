//! The `odx` binary.

use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let exit = odx_cli::run(
        std::env::args_os(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    );
    ExitCode::from(exit.code())
}
