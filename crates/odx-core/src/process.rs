use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// A program invocation: what to run, with which arguments, and where.
///
/// ```
/// use odx_core::Invocation;
///
/// let git = Invocation::new("git").arg("status").current_dir("/tmp");
/// assert_eq!(git.program(), "git");
/// assert_eq!(git.args(), ["status"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    program: OsString,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
}

impl Invocation {
    /// Start an invocation of `program` with no arguments.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
        }
    }

    /// Append one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Run in `dir` instead of the current directory.
    #[must_use]
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// The program to run.
    pub fn program(&self) -> &OsString {
        &self.program
    }

    /// The arguments, in order.
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// The working directory, if one was set.
    pub fn working_dir(&self) -> Option<&PathBuf> {
        self.current_dir.as_ref()
    }
}

/// The result of a finished process.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// Exit status code; `None` when the process was terminated by a signal.
    pub status: Option<i32>,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
}

impl Output {
    /// Whether the process exited with status `0`.
    ///
    /// ```
    /// use odx_core::Output;
    ///
    /// assert!(Output { status: Some(0), ..Output::default() }.success());
    /// assert!(!Output { status: Some(1), ..Output::default() }.success());
    /// assert!(!Output { status: None, ..Output::default() }.success());
    /// ```
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Runs external programs such as `git`, `uv` and `mergiraf`.
pub trait ProcessRunner: std::fmt::Debug + Send + Sync {
    /// Run `invocation` to completion and capture its output.
    ///
    /// A non-zero exit status is not an error; inspect [`Output::success`].
    ///
    /// # Errors
    ///
    /// Returns an error when the program cannot be started.
    fn run(&self, invocation: &Invocation) -> io::Result<Output>;
}

#[cfg(test)]
mod tests {
    use super::Invocation;
    use std::path::PathBuf;

    #[test]
    fn invocation_builder_keeps_order_and_dir() {
        let inv = Invocation::new("uv")
            .arg("lock")
            .arg("--upgrade-package")
            .current_dir("/work");
        assert_eq!(inv.args(), ["lock", "--upgrade-package"]);
        assert_eq!(inv.working_dir(), Some(&PathBuf::from("/work")));
        assert_eq!(Invocation::new("git").working_dir(), None);
    }
}
