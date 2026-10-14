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

/// The merge commit of a change, as [`Repo::merged_by`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merge {
    /// Its message.
    pub message: String,
    /// The subject of the merged branch's tip, when it has a second parent.
    pub tip: Option<String>,
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

    /// The merge commit that brought `sha` onto `branch` (`first_parents`
    /// of it): the oldest commit of the first-parent line that descends
    /// from `sha`, with its message and the subject of the merged branch's
    /// tip (the pull request's last commit). `None` for a commit on the
    /// line itself.
    #[must_use]
    pub fn merged_by(
        &self,
        sha: &str,
        branch: &str,
        first_parents: &BTreeSet<String>,
    ) -> Option<Merge> {
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
        let message = self.text(&["log", "-1", "--format=%s%n%b", merge]).ok()?;
        let tip = self
            .text(&["log", "-1", "--format=%s", &format!("{merge}^2")])
            .ok()
            .map(|subject| subject.trim().to_owned());
        Some(Merge {
            message: message.trim().to_owned(),
            tip,
        })
    }

    /// An id of `files` as they are at `rev`: the git object id of their
    /// `ls-tree` listing, so it changes exactly when one of them does.
    ///
    /// # Errors
    ///
    /// When git cannot read `rev`.
    pub fn tree_of(&self, rev: &str, files: &[String]) -> Result<String, String> {
        let listing = self.output(&self.git().args(["ls-tree", "-r", rev, "--"]).args(files))?;
        let id = self
            .output(&self.git().args(["hash-object", "--stdin"]).stdin(listing))
            .unwrap_or_default();
        Ok(String::from_utf8_lossy(&id).trim().to_owned())
    }

    /// Whether `commits` (oldest first) cherry-pick cleanly onto `target`,
    /// replayed with `git merge-tree` without touching any worktree; `None`
    /// when git cannot tell (a root commit, an unknown revision).
    #[must_use]
    pub fn applies(&self, target: &str, commits: &[String]) -> Option<bool> {
        let mut onto = self.text(&["rev-parse", target]).ok()?.trim().to_owned();
        for commit in commits {
            let parent = format!("{commit}^");
            let merged = self
                .runner
                .run(&self.git().args([
                    "merge-tree",
                    "--write-tree",
                    "--merge-base",
                    &parent,
                    &onto,
                    commit,
                ]))
                .ok()?;
            match merged.status {
                Some(0) => {}
                Some(1) => return Some(false),
                _ => return None,
            }
            let stdout = String::from_utf8_lossy(&merged.stdout);
            let tree = stdout.lines().next().unwrap_or_default().to_owned();
            let next = self
                .text(&[
                    "-c",
                    "user.name=odx",
                    "-c",
                    "user.email=odx@localhost",
                    "commit-tree",
                    &tree,
                    "-p",
                    &onto,
                    "-m",
                    "odx: applies",
                ])
                .ok()?;
            next.trim().clone_into(&mut onto);
        }
        Some(true)
    }

    /// The content of `path` at `rev`, if it exists.
    #[must_use]
    pub fn show(&self, rev: &str, path: &str) -> Option<String> {
        self.text(&["show", &format!("{rev}:{path}")]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::system::OsRunner;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=Odx", "-c", "user.email=odx@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn merges_and_their_tips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet", "--initial-branch=main"]);
        commit(dir, "a", "a\n");
        git(dir, &["checkout", "--quiet", "-b", "side"]);
        let change = commit(dir, "b", "b\n");
        git(dir, &["checkout", "--quiet", "main"]);
        git(
            dir,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                "side",
                "-m",
                "Merge PR #1 into main",
            ],
        );
        let repo = Repo::new(&OsRunner, dir);
        let line = repo.first_parents("main").unwrap();
        let merge = repo.merged_by(&change, "main", &line).unwrap();
        assert_eq!(merge.message, "Merge PR #1 into main");
        assert_eq!(merge.tip.as_deref(), Some("b"));
        // A failing log leaves no merge, a failing tip no tip.
        let faulty = crate::testing::Faulty::new(1, false);
        assert_eq!(
            Repo::new(&faulty, dir).merged_by(&change, "main", &line),
            None
        );
        let faulty = crate::testing::Faulty::new(2, false);
        let merge = Repo::new(&faulty, dir)
            .merged_by(&change, "main", &line)
            .unwrap();
        assert_eq!(merge.tip, None);
    }

    #[test]
    fn commits_replayed_on_a_target() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet", "--initial-branch=old"]);
        let root = commit(dir, "a", "a\n");
        let clean = commit(dir, "b", "b\n");
        let other = commit(dir, "a", "a, old\n");
        git(dir, &["checkout", "--quiet", "-b", "new", &root]);
        commit(dir, "a", "a, new\n");
        let repo = Repo::new(&OsRunner, dir);
        assert_eq!(
            repo.applies("new", std::slice::from_ref(&clean)),
            Some(true)
        );
        assert_eq!(repo.applies("new", &[clean.clone(), other]), Some(false));
        assert_eq!(repo.applies("new", &[root]), None);
        assert_eq!(repo.applies("nope", std::slice::from_ref(&clean)), None);
        // Whatever command fails, the answer is "cannot tell".
        for fail_at in 0..3 {
            for spawn in [false, true] {
                let faulty = crate::testing::Faulty::new(fail_at, spawn);
                let repo = Repo::new(&faulty, dir);
                assert_ne!(
                    repo.applies("new", std::slice::from_ref(&clean)),
                    Some(true)
                );
            }
        }
    }
}
