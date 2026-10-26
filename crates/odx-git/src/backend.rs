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
    /// Offline, and what is asked for is not in the cache.
    #[error("{0} is not in the cache; fetch it once without --offline")]
    NotCached(String),
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

impl SystemGit {
    /// Apply `git update-ref --stdin` instructions to the repository at
    /// `dir`, atomically.
    ///
    /// # Errors
    ///
    /// When git refuses an update.
    pub fn update_refs(&self, dir: &Path, instructions: &str) -> Result<(), GitError> {
        let invocation = self
            .git(Some(dir))
            .args(["update-ref", "--stdin"])
            .stdin(instructions);
        self.run("update-ref", &invocation).map(drop)
    }

    /// Pack every object the repository at `dir` reaches, including those
    /// it borrows through `objects/info/alternates`, into its own store.
    ///
    /// # Errors
    ///
    /// When git cannot read or write an object.
    pub fn repack_all(&self, dir: &Path) -> Result<(), GitError> {
        let invocation = self.git(Some(dir)).args(["repack", "-a", "-d", "-q"]);
        self.run("repack", &invocation).map(drop)
    }

    /// Write a git bundle of `refs` of the repository at `dir` to `file`.
    ///
    /// # Errors
    ///
    /// When git cannot write it.
    pub fn bundle(&self, dir: &Path, file: &Path, refs: &[String]) -> Result<(), GitError> {
        let invocation = self
            .git(Some(dir))
            .args(["bundle", "create", "--quiet"])
            .arg(file.as_os_str())
            .args(refs);
        self.run("bundle", &invocation).map(drop)
    }

    /// Push `refspecs` from the repository at `dir` to `url`, atomically
    /// and never forced: a ref that exists with another value is an error.
    ///
    /// # Errors
    ///
    /// When the remote cannot be reached or refuses a ref.
    pub fn push(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError> {
        let invocation = self
            .git(Some(dir))
            .args(["push", "--atomic", "--quiet", "--no-verify", url])
            .args(refspecs);
        self.run("push", &invocation).map(drop)
    }
}

impl SystemGit {
    /// The commit `reference` (a branch or tag name, or a full ref) points
    /// at on `url`, without fetching anything; annotated tags are peeled.
    ///
    /// # Errors
    ///
    /// When the remote cannot be reached.
    pub fn ls_remote(&self, url: &str, reference: &str) -> Result<Option<String>, GitError> {
        let invocation = self
            .git(None)
            .args(["ls-remote", url, reference])
            .arg(format!("{reference}^{{}}"));
        let output = self.run("ls-remote", &invocation)?;
        let listing = String::from_utf8_lossy(&output.stdout);
        let refs: Vec<(&str, &str)> = listing
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .collect();
        let wanted = |name: &str| {
            [
                reference.to_owned(),
                format!("refs/heads/{reference}"),
                format!("refs/tags/{reference}"),
            ]
            .iter()
            .any(|candidate| name == candidate || name == format!("{candidate}^{{}}"))
        };
        let peeled = refs
            .iter()
            .find(|(_, name)| name.ends_with("^{}") && wanted(name));
        Ok(peeled
            .or_else(|| refs.iter().find(|(_, name)| wanted(name)))
            .map(|(sha, _)| (*sha).to_owned()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::system::OsRunner;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "odx")
            .env("GIT_AUTHOR_EMAIL", "odx@example.com")
            .env("GIT_COMMITTER_NAME", "odx")
            .env("GIT_COMMITTER_EMAIL", "odx@example.com")
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn remote_refs_without_fetching() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        git(repo, &["init", "--quiet", "--initial-branch=17.0"]);
        git(repo, &["commit", "--quiet", "--allow-empty", "-m", "a"]);
        let head = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["tag", "light"]);
        git(repo, &["tag", "-a", "annotated", "-m", "tag"]);
        let system = SystemGit::new(Arc::new(OsRunner));
        let url = repo.to_str().unwrap();
        for reference in [
            "17.0",
            "refs/heads/17.0",
            "light",
            "annotated",
            "refs/tags/annotated",
        ] {
            assert_eq!(
                system.ls_remote(url, reference).unwrap().as_deref(),
                Some(head.as_str()),
                "{reference}"
            );
        }
        assert_eq!(system.ls_remote(url, "missing").unwrap(), None);
        assert!(system.ls_remote("/nonexistent/odx", "17.0").is_err());
    }
}
