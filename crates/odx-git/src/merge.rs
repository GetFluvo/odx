use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use odx_core::{Invocation, Output, ProcessRunner};

use crate::GitError;

/// One merge to apply on top of the base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// How the merge is shown to people, e.g. `#123` or `branch 17.0-fix`.
    pub label: String,
    /// The commit to merge.
    pub sha: String,
    /// Skip the merge on a conflict instead of stopping.
    pub skip_on_conflict: bool,
}

/// Everything needed to build one repository's working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregation {
    /// The bare cache repository holding every commit.
    pub cache_repo: PathBuf,
    /// The working tree to create or reset.
    pub target_dir: PathBuf,
    /// The base commit.
    pub base: String,
    /// Merges, applied in order.
    pub steps: Vec<Step>,
    /// The mergiraf executable to use as merge driver, if any.
    pub mergiraf: Option<OsString>,
    /// Leave a conflicted merge in place instead of aborting it.
    pub keep_conflicts: bool,
    /// Discard local changes, or replace a directory that is not a working
    /// tree of the cache.
    pub force: bool,
}

/// What happened to one merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// A merge commit was created.
    Merged,
    /// The commit was already part of the result.
    UpToDate,
    /// The merge conflicted and was skipped (`on_conflict = "skip"`).
    Skipped {
        /// The conflicting files.
        files: Vec<String>,
    },
}

/// The result of an aggregation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregated {
    /// The resulting commit.
    pub head: String,
    /// Its tree.
    pub tree: String,
    /// One outcome per step, in order.
    pub steps: Vec<(String, StepOutcome)>,
}

/// Errors from aggregating.
#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    /// git failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The working tree has local changes and `force` is not set.
    #[error("{0} has local changes; commit or discard them, or use --force")]
    Dirty(PathBuf),
    /// The directory exists but is not a working tree of the cache.
    #[error("{0} exists and is not managed by odx; move it away, or use --force to replace it")]
    NotAWorktree(PathBuf),
    /// A merge conflicted.
    #[error("{label} conflicts in {files}{note}", files = files.join(", "), note = if *kept { " (left in place)" } else { "" })]
    Conflict {
        /// The step.
        label: String,
        /// The conflicting files.
        files: Vec<String>,
        /// Whether the conflicted state was kept for inspection.
        kept: bool,
    },
}

/// Builds working trees from the cache with the system `git`.
#[derive(Debug, Clone)]
pub struct Merger {
    runner: Arc<dyn ProcessRunner>,
    program: OsString,
    envs: Vec<(OsString, OsString)>,
}

impl Merger {
    /// Use `git` from `PATH`.
    pub fn new(runner: Arc<dyn ProcessRunner>) -> Self {
        Self {
            runner,
            program: "git".into(),
            envs: Vec::new(),
        }
    }

    /// Pass an extra environment variable to every git invocation, e.g.
    /// `GIT_CONFIG_GLOBAL` for hermetic runs.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    fn git(&self, dir: &Path) -> Invocation {
        let invocation = Invocation::new(self.program.clone())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C");
        self.envs
            .iter()
            .fold(invocation, |invocation, (key, value)| {
                invocation.env(key, value)
            })
            .arg("-C")
            .arg(dir.as_os_str())
    }

    fn run(&self, command: &str, invocation: &Invocation) -> Result<Output, GitError> {
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

    fn stdout(&self, command: &str, invocation: &Invocation) -> Result<String, GitError> {
        self.run(command, invocation)
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Create or reset the working tree, apply every step and return the
    /// result.
    ///
    /// # Errors
    ///
    /// [`MergeError::Dirty`] / [`MergeError::NotAWorktree`] when the target
    /// cannot be reset without `force`, [`MergeError::Conflict`] when a
    /// step conflicts and is not allowed to be skipped, and git failures.
    pub fn aggregate(&self, plan: &Aggregation) -> Result<Aggregated, MergeError> {
        self.prepare(plan)?;
        let identity = self.identity(&plan.target_dir)?;
        let attributes = match &plan.mergiraf {
            Some(_) => Some(mergiraf_attributes(&plan.cache_repo)?),
            None => None,
        };
        let mut steps = Vec::with_capacity(plan.steps.len());
        for step in &plan.steps {
            let outcome = self.merge(plan, step, &identity, attributes.as_deref())?;
            steps.push((step.label.clone(), outcome));
        }
        Ok(Aggregated {
            head: self.stdout(
                "rev-parse",
                &self.git(&plan.target_dir).args(["rev-parse", "HEAD"]),
            )?,
            tree: self.stdout(
                "rev-parse",
                &self
                    .git(&plan.target_dir)
                    .args(["rev-parse", "HEAD^{tree}"]),
            )?,
            steps,
        })
    }

    /// Make `target_dir` a clean, detached working tree of the cache at
    /// the base commit.
    fn prepare(&self, plan: &Aggregation) -> Result<(), MergeError> {
        let cache = &plan.cache_repo;
        let target = &plan.target_dir;
        self.run("worktree", &self.git(cache).args(["worktree", "prune"]))?;
        if target.exists() {
            if !self.is_worktree_of(target, cache) {
                if !plan.force {
                    return Err(MergeError::NotAWorktree(target.clone()));
                }
                fs::remove_dir_all(target).map_err(GitError::Io)?;
                return self.add_worktree(cache, target, &plan.base);
            }
            let status =
                self.stdout("status", &self.git(target).args(["status", "--porcelain"]))?;
            if !status.is_empty() && !plan.force {
                return Err(MergeError::Dirty(target.clone()));
            }
            self.run(
                "reset",
                &self.git(target).args(["reset", "--hard", "--quiet"]),
            )?;
            self.run(
                "checkout",
                &self
                    .git(target)
                    .args(["checkout", "--detach", "--force", "--quiet"])
                    .arg(&plan.base),
            )?;
            self.run("clean", &self.git(target).args(["clean", "-fdxq"]))?;
            return Ok(());
        }
        self.add_worktree(cache, target, &plan.base)
    }

    fn add_worktree(&self, cache: &Path, target: &Path, base: &str) -> Result<(), MergeError> {
        let invocation = self
            .git(cache)
            .args(["worktree", "add", "--detach", "--force", "--quiet"])
            .arg(target.as_os_str())
            .arg(base);
        self.run("worktree", &invocation)?;
        Ok(())
    }

    /// Whether `target` is a working tree whose repository is `cache`.
    fn is_worktree_of(&self, target: &Path, cache: &Path) -> bool {
        let common =
            self.git(target)
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        match (self.stdout("rev-parse", &common), fs::canonicalize(cache)) {
            (Ok(common), Ok(cache)) => fs::canonicalize(common).is_ok_and(|common| common == cache),
            _ => false,
        }
    }

    /// Environment that gives merge commits an identity when git has none.
    fn identity(&self, target: &Path) -> Result<Vec<(&'static str, &'static str)>, GitError> {
        let configured = self
            .runner
            .run(&self.git(target).args(["config", "user.email"]))?
            .success();
        Ok(if configured {
            Vec::new()
        } else {
            vec![
                ("GIT_AUTHOR_NAME", "odx"),
                ("GIT_AUTHOR_EMAIL", "odx@localhost"),
                ("GIT_COMMITTER_NAME", "odx"),
                ("GIT_COMMITTER_EMAIL", "odx@localhost"),
            ]
        })
    }

    fn merge(
        &self,
        plan: &Aggregation,
        step: &Step,
        identity: &[(&str, &str)],
        attributes: Option<&Path>,
    ) -> Result<StepOutcome, MergeError> {
        let target = &plan.target_dir;
        let before = self.stdout("rev-parse", &self.git(target).args(["rev-parse", "HEAD"]))?;
        let mut invocation = self.git(target);
        if let (Some(program), Some(attributes)) = (&plan.mergiraf, attributes) {
            let mut driver = program.clone();
            driver.push(" merge --git %O %A %B -s %S -x %X -y %Y -p %P -l %L");
            let mut attributes_file = OsString::from("core.attributesFile=");
            attributes_file.push(attributes.as_os_str());
            let mut driver_config = OsString::from("merge.mergiraf.driver=");
            driver_config.push(driver);
            invocation = invocation
                .args(["-c", "merge.mergiraf.name=mergiraf", "-c"])
                .arg(driver_config)
                .arg("-c")
                .arg(attributes_file);
        }
        for (key, value) in identity {
            invocation = invocation.env(*key, *value);
        }
        let invocation = invocation
            .args(["merge", "--no-ff", "--no-edit", "--quiet", "-m"])
            .arg(format!("Merge {}", step.label))
            .arg(&step.sha);
        let output = self.runner.run(&invocation).map_err(GitError::Io)?;
        if output.success() {
            let after = self.stdout("rev-parse", &self.git(target).args(["rev-parse", "HEAD"]))?;
            return Ok(if after == before {
                StepOutcome::UpToDate
            } else {
                StepOutcome::Merged
            });
        }
        let files: Vec<String> = self
            .stdout(
                "diff",
                &self
                    .git(target)
                    .args(["diff", "--name-only", "--diff-filter=U"]),
            )?
            .lines()
            .map(str::to_owned)
            .collect();
        if files.is_empty() {
            return Err(MergeError::Git(GitError::Failed {
                command: "merge".to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }));
        }
        let keep = plan.keep_conflicts && !step.skip_on_conflict;
        if !keep {
            self.run("merge", &self.git(target).args(["merge", "--abort"]))?;
        }
        if step.skip_on_conflict {
            Ok(StepOutcome::Skipped { files })
        } else {
            Err(MergeError::Conflict {
                label: step.label.clone(),
                files,
                kept: keep,
            })
        }
    }
}

/// A git attributes file that routes every path through mergiraf.
fn mergiraf_attributes(cache: &Path) -> Result<PathBuf, GitError> {
    let file = cache.join("odx-mergiraf.attributes");
    fs::write(&file, "* merge=mergiraf\n")?;
    Ok(file)
}
#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
