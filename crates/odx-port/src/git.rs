//! The git queries the analysis needs, on a (bare) repository.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use odx_core::{Invocation, ProcessRunner};

/// A commit as the analysis sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Full id.
    pub sha: String,
    /// First line of the message.
    pub subject: String,
    /// Author name.
    pub author: String,
    /// Author date (ISO 8601).
    pub date: String,
}

/// A repository and the runner its git commands go through.
#[derive(Debug, Clone)]
pub struct Repo<'a> {
    runner: &'a dyn ProcessRunner,
    dir: PathBuf,
}

impl<'a> Repo<'a> {
    /// The repository at `dir`.
    pub fn new(runner: &'a dyn ProcessRunner, dir: impl Into<PathBuf>) -> Self {
        Self {
            runner,
            dir: dir.into(),
        }
    }

    fn git(&self) -> Invocation {
        Invocation::new("git")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(self.dir.as_os_str())
    }

    fn output(&self, invocation: &Invocation) -> Result<Vec<u8>, String> {
        let output = self.runner.run(invocation).map_err(|e| e.to_string())?;
        if output.success() {
            Ok(output.stdout)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    fn text(&self, args: &[&str]) -> Result<String, String> {
        self.output(&self.git().args(args))
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    }

    /// Non-merge commits of `range` that touch `dir`, newest first.
    ///
    /// # Errors
    ///
    /// When git cannot read the range.
    pub fn commits(&self, range: &str, dir: &str) -> Result<Vec<Commit>, String> {
        let log = self.text(&[
            "log",
            "--no-merges",
            "--format=%H%x1f%s%x1f%an%x1f%aI",
            range,
            "--",
            dir,
        ])?;
        Ok(log
            .lines()
            .map(|line| {
                let mut fields = line.splitn(4, '\u{1f}').map(str::to_owned);
                Commit {
                    sha: fields.next().unwrap_or_default(),
                    subject: fields.next().unwrap_or_default(),
                    author: fields.next().unwrap_or_default(),
                    date: fields.next().unwrap_or_default(),
                }
            })
            .collect())
    }

    /// `git patch-id --stable` of every non-merge commit of `range` that
    /// touches `dir`, with paths relative to `dir` so a renamed or moved
    /// addon keeps its ids: commit → patch-id.
    ///
    /// # Errors
    ///
    /// When git cannot read the range.
    pub fn patch_ids(&self, range: &str, dir: &str) -> Result<BTreeMap<String, String>, String> {
        let relative = format!("--relative={dir}/");
        let diff = self.output(&self.git().args([
            "log",
            "--no-merges",
            "-p",
            "--format=commit %H",
            &relative,
            range,
            "--",
            dir,
        ]))?;
        // patch-id reads any input; it has nothing to fail on.
        let ids = self
            .output(&self.git().args(["patch-id", "--stable"]).stdin(diff))
            .unwrap_or_default();
        Ok(String::from_utf8_lossy(&ids)
            .lines()
            .filter_map(|line| line.split_once(' '))
            .map(|(id, sha)| (sha.to_owned(), id.to_owned()))
            .collect())
    }

    /// The files commit `sha` changes.
    ///
    /// # Errors
    ///
    /// When git cannot read the commit.
    pub fn files(&self, sha: &str) -> Result<Vec<String>, String> {
        let names = self.text(&[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
            sha,
        ])?;
        Ok(names.lines().map(str::to_owned).collect())
    }

    /// The commits on the first-parent line of `branch`: those pushed or
    /// merged onto it directly.
    ///
    /// # Errors
    ///
    /// When git cannot read the branch.
    pub fn first_parents(&self, branch: &str) -> Result<BTreeSet<String>, String> {
        let list = self.text(&["rev-list", "--first-parent", branch])?;
        Ok(list.lines().map(str::to_owned).collect())
    }

    /// The message of the merge commit that brought `sha` onto `branch`
    /// (`first_parents` of it): the oldest commit of the first-parent line
    /// that descends from `sha`. `None` for a commit on the line itself.
    #[must_use]
    pub fn merged_by(
        &self,
        sha: &str,
        branch: &str,
        first_parents: &BTreeSet<String>,
    ) -> Option<String> {
        if first_parents.contains(sha) {
            return None;
        }
        let range = format!("{sha}..{branch}");
        let descendants = self
            .text(&["rev-list", "--ancestry-path", "--reverse", &range])
            .ok()?;
        let merge = descendants
            .lines()
            .find(|commit| first_parents.contains(*commit))?;
        self.text(&["log", "-1", "--format=%s%n%b", merge])
            .ok()
            .map(|message| message.trim().to_owned())
    }

    /// The content of `path` at `rev`, if it exists.
    #[must_use]
    pub fn show(&self, rev: &str, path: &str) -> Option<String> {
        self.text(&["show", &format!("{rev}:{path}")]).ok()
    }
}
