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
/// assert_eq!(git.arguments(), ["status"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    program: OsString,
    args: Vec<OsString>,
    current_dir: Option<PathBuf>,
    envs: Vec<(OsString, OsString)>,
}

impl Invocation {
    /// Start an invocation of `program` with no arguments.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            current_dir: None,
            envs: Vec::new(),
        }
    }

    /// Append one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several arguments.
    #[must_use]
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable for the process.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
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
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Extra environment variables, in order.
    pub fn envs(&self) -> &[(OsString, OsString)] {
        &self.envs
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
            .args(["--upgrade-package", "x"])
            .env("UV_OFFLINE", "1")
            .current_dir("/work");
        assert_eq!(inv.arguments(), ["lock", "--upgrade-package", "x"]);
        assert_eq!(inv.envs(), [("UV_OFFLINE".into(), "1".into())]);
        assert_eq!(inv.working_dir(), Some(&PathBuf::from("/work")));
        assert_eq!(Invocation::new("git").working_dir(), None);
    }
}
