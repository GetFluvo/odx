use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::Arc;

use odx_core::{Invocation, ProcessRunner};

/// Errors from git operations.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// git could not be started.
    #[error("cannot run git: {0}")]
    Io(#[from] io::Error),
    /// git ran and failed.
    #[error("git {command} failed: {stderr}")]
    Failed {
        /// The git subcommand.
        command: String,
        /// git's error output, trimmed.
        stderr: String,
    },
    /// Another process holds the repository lock.
    #[error("{0} is locked by another odx process")]
    Locked(String),
    /// The operation was cancelled before it started.
    #[error("cancelled")]
    Cancelled,
}

/// The git operations odx needs from a backend.
pub trait GitBackend: std::fmt::Debug + Send + Sync {
    /// Create a bare repository at `dir` (and its parent directories).
    ///
    /// # Errors
    ///
    /// When the repository cannot be created.
    fn init_bare(&self, dir: &Path) -> Result<(), GitError>;

    /// Fetch `refspecs` from `url` into the repository at `dir`, atomically.
    ///
    /// # Errors
    ///
    /// When the remote cannot be reached or a ref does not exist.
    fn fetch(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError>;

    /// The commit `rev` points at, or `None` when it does not resolve.
    ///
    /// # Errors
    ///
    /// When git cannot be run.
    fn resolve(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError>;
}

/// The system `git` executable, run with a controlled environment: no
/// terminal prompts and C locale messages.
#[derive(Debug, Clone)]
pub struct SystemGit {
    runner: Arc<dyn ProcessRunner>,
    program: OsString,
}

impl SystemGit {
    /// Use `git` from `PATH` through `runner`.
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self::with_program(runner, "git")
    }

    /// Use a specific git executable.
    pub fn with_program(runner: Arc<dyn ProcessRunner>, program: impl Into<OsString>) -> Self {
        Self {
            runner,
            program: program.into(),
        }
    }

    fn git(&self, dir: Option<&Path>) -> Invocation {
        let invocation = Invocation::new(self.program.clone())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "never")
            .env("LC_ALL", "C");
        match dir {
            Some(dir) => invocation.arg("-C").arg(dir.as_os_str()),
            None => invocation,
        }
    }

    fn run(&self, command: &str, invocation: &Invocation) -> Result<odx_core::Output, GitError> {
        let output = self.runner.run(invocation)?;
        if output.success() {
            Ok(output)
        } else {
            Err(GitError::Failed {
                command: command.to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

impl GitBackend for SystemGit {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        let invocation = self
            .git(None)
            .args(["init", "--bare", "--quiet"])
            .arg(dir.as_os_str());
        self.run("init", &invocation).map(drop)
    }

    fn fetch(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError> {
        let invocation = self
            .git(Some(dir))
            .args([
                "fetch",
                "--no-tags",
                "--atomic",
                "--quiet",
                "--no-write-fetch-head",
                "--no-auto-gc",
                url,
            ])
            .args(refspecs);
        self.run("fetch", &invocation).map(drop)
    }

    fn resolve(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
        let invocation = self
            .git(Some(dir))
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("{rev}^{{commit}}"));
        let output = self.runner.run(&invocation)?;
        Ok(output
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }
}
