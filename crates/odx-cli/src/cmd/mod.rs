//! Command implementations.
//!
//! Commands build a [`Report`] instead of writing as they go, so that all
//! output happens in [`Report::emit`] and every command is a pure function
//! of its arguments and context.

use std::fmt::Write as _;
use std::io::{self, Write};

use odx_config::Problem;
use odx_core::{Exit, redact};

pub(crate) mod aggregate;
pub(crate) mod autoupdate;
pub(crate) mod compat;
pub(crate) mod edit;
pub(crate) mod sniff;
pub(crate) mod stats;
pub(crate) mod vendor;
pub(crate) mod verify;

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
