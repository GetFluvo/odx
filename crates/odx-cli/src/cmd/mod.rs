//! Command implementations.
//!
//! Commands build a [`Report`] instead of writing as they go, so that all
//! output happens in [`Report::emit`] and every command is a pure function
//! of its arguments and context.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::Path;

use odx_config::Problem;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner, redact};

pub(crate) mod aggregate;
pub(crate) mod auth;
pub(crate) mod autoupdate;
pub(crate) mod build;
pub(crate) mod compat;
pub(crate) mod dev;
pub(crate) mod doctor;
pub(crate) mod edit;
pub(crate) mod hooks;
pub(crate) mod sniff;
pub(crate) mod stats;
pub(crate) mod sync;
pub(crate) mod vendor;
pub(crate) mod verify;

/// Run `invocation`; a failure carries its standard error.
pub(crate) fn run_checked(invocation: &Invocation, what: &str) -> Result<(), String> {
    let output = OsRunner.run(invocation);
    if output.as_ref().is_ok_and(odx_core::Output::success) {
        return Ok(());
    }
    let detail = output.map_or_else(
        |e| e.to_string(),
        |out| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    );
    Err(format!("{what} failed: {detail}"))
}

/// Bytes under `dir`.
pub(crate) fn size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => size(&entry.path()),
            _ => entry.metadata().map(|m| m.len()).unwrap_or_default(),
        })
        .sum()
}

#[expect(clippy::cast_precision_loss, reason = "a size for people to read")]
pub(crate) fn human(bytes: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units.get(unit).copied().unwrap_or("TB"))
    }
}

/// What a command produced.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Exit status.
    pub(crate) exit: Exit,
    /// Standard output.
    pub(crate) out: String,
    /// Warnings, printed to standard error before any error.
    pub(crate) warnings: Vec<Problem>,
    /// The error that ended the command, if any.
    pub(crate) error: Option<String>,
}

impl Report {
    /// A report that only carries an error.
    pub(crate) fn error(exit: Exit, message: String) -> Self {
        Self {
            exit,
            error: Some(message),
            ..Self::default()
        }
    }

    /// Write the report: warnings and the error to `err`, output to `out`.
    pub(crate) fn emit(self, out: &mut dyn Write, err: &mut dyn Write) -> io::Result<Exit> {
        let mut diagnostics = String::new();
        for warning in &self.warnings {
            let _ = writeln!(diagnostics, "warning: {warning}");
        }
        if let Some(error) = &self.error {
            let _ = writeln!(diagnostics, "error: {error}");
        }
        // Credentials in URLs never leave odx.
        err.write_all(redact(&diagnostics).as_bytes())?;
        out.write_all(redact(&self.out).as_bytes())?;
        Ok(self.exit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn emit_orders_warnings_before_errors() {
        let report = Report {
            exit: Exit::Failure,
            out: "partial\n".to_owned(),
            warnings: vec![Problem {
                key: "k".to_owned(),
                message: "m".to_owned(),
            }],
            error: Some("boom".to_owned()),
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(report.emit(&mut out, &mut err).unwrap(), Exit::Failure);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: k: m\nerror: boom\n"
        );
        assert_eq!(String::from_utf8(out).unwrap(), "partial\n");
    }

    #[test]
    fn emit_reports_write_failures() {
        let failed = Report::error(Exit::Failure, "boom".to_owned());
        assert!(failed.emit(&mut Vec::new(), &mut Broken).is_err());
        let output = Report {
            out: "x".to_owned(),
            ..Report::default()
        };
        assert!(output.emit(&mut Broken, &mut Vec::new()).is_err());
        assert!(Broken.flush().is_ok());
    }
}
