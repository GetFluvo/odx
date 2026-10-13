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

/// How each step is applied on top of the result so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Replay the step's commits on top (`git rebase`), keeping history
    /// linear; commits already applied are dropped.
    #[default]
    Rebase,
    /// Create a merge commit (`git merge --no-ff`).
    Merge,
}

impl Strategy {
    /// The git command that applies a step.
    pub const fn command(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::Merge => "merge",
        }
    }
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
    /// How steps are applied.
    pub strategy: Strategy,
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
    /// The step was applied: rebased, or merged with the merge strategy.
    Merged,
    /// The step could not be rebased and was merged with a merge commit.
    MergeCommit,
    /// The commit was already part of the result.
    UpToDate,
    /// The merge conflicted and was skipped (`on_conflict = "skip"`).
    Skipped {
        /// The conflicting files.
        files: Vec<String>,
    },
}

impl StepOutcome {
    /// The outcome of a merge made because the step could not be rebased.
    ///
    /// ```
    /// use odx_git::StepOutcome;
    ///
    /// assert_eq!(StepOutcome::Merged.instead_of_rebase(), StepOutcome::MergeCommit);
    /// assert_eq!(StepOutcome::UpToDate.instead_of_rebase(), StepOutcome::UpToDate);
    /// ```
    #[must_use]
    pub fn instead_of_rebase(self) -> Self {
        match self {
            Self::Merged => Self::MergeCommit,
            other => other,
        }
    }

    /// `merged`, `up-to-date` or `skipped`, as in machine-readable output.
    pub const fn status(&self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::MergeCommit => "merge-commit",
            Self::UpToDate => "up-to-date",
            Self::Skipped { .. } => "skipped",
        }
    }
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

impl Aggregated {
    /// `2 merged, 1 up to date, 1 skipped: #3 conflicts in a.xml`, or
    /// `base only`.
    ///
    /// ```
    /// use odx_git::{Aggregated, StepOutcome};
    ///
    /// let built = Aggregated {
    ///     head: "h".into(),
    ///     tree: "t".into(),
    ///     steps: vec![("#1".into(), StepOutcome::Merged), ("#2".into(), StepOutcome::UpToDate)],
    /// };
    /// assert_eq!(built.describe(), "1 merged, 1 up to date");
    /// ```
    pub fn describe(&self) -> String {
        let count =
            |want: fn(&StepOutcome) -> bool| self.steps.iter().filter(|(_, o)| want(o)).count();
        let merged = count(|o| *o == StepOutcome::Merged);
        let current = count(|o| *o == StepOutcome::UpToDate);
        let skipped: Vec<String> = self
            .steps
            .iter()
            .filter_map(|(label, outcome)| match outcome {
                StepOutcome::Skipped { files } => {
                    Some(format!("{label} conflicts in {}", files.join(", ")))
                }
                _ => None,
            })
            .collect();
        let fallbacks: Vec<&str> = self
            .steps
            .iter()
            .filter(|(_, outcome)| *outcome == StepOutcome::MergeCommit)
            .map(|(label, _)| label.as_str())
            .collect();
        let mut parts = Vec::new();
        if merged > 0 {
            parts.push(format!("{merged} merged"));
        }
        if !fallbacks.is_empty() {
            parts.push(format!(
                "{} merged with a merge commit (could not be rebased): {}",
                fallbacks.len(),
                fallbacks.join(", ")
            ));
        }
        if current > 0 {
            parts.push(format!("{current} up to date"));
        }
        if !skipped.is_empty() {
            parts.push(format!("{} skipped: {}", skipped.len(), skipped.join("; ")));
        }
        if parts.is_empty() {
            "base only".to_owned()
        } else {
            parts.join(", ")
        }
    }
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

    /// Whether `target` is a clean working tree of `cache` whose checkout
    /// has tree `tree`: then there is nothing to build.
    pub fn is_current(&self, target: &Path, cache: &Path, tree: &str) -> bool {
        self.is_worktree_of(target, cache)
            && self
                .stdout("status", &self.git(target).args(["status", "--porcelain"]))
                .is_ok_and(|status| status.is_empty())
            && self.tree(target, "HEAD").is_ok_and(|head| head == tree)
    }

    /// The top-level directories `sha` changes compared with its merge
    /// base with `base`: the addons a pull request touches.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub fn changed_dirs(
        &self,
        repo: &Path,
        base: &str,
        sha: &str,
    ) -> Result<Vec<String>, GitError> {
        let fork_point = self.stdout(
            "merge-base",
            &self.git(repo).args(["merge-base", base, sha]),
        )?;
        let paths = self.stdout(
            "diff",
            &self.git(repo).args([
                "diff",
                "--name-only",
                "--no-renames",
                fork_point.as_str(),
                sha,
            ]),
        )?;
        let mut dirs: Vec<String> = paths
            .lines()
            .filter_map(|path| path.split_once('/').map(|(dir, _)| dir.to_owned()))
            .collect();
        dirs.dedup();
        Ok(dirs)
    }

    /// The files that differ between the trees (or commits) `old` and
    /// `new`, without rename detection, under `path` when given: each with
    /// git's status letter (`A`, `M`, `D`, ...).
    ///
    /// # Errors
    ///
    /// When git fails, for instance when a tree is not in `repo`.
    pub fn changed_files(
        &self,
        repo: &Path,
        old: &str,
        new: &str,
        path: Option<&str>,
    ) -> Result<Vec<(char, String)>, GitError> {
        let invocation = self
            .git(repo)
            .args([
                "diff",
                "--name-status",
                "--no-renames",
                "-z",
                old,
                new,
                "--",
            ])
            .args(path);
        let listing = self.stdout("diff", &invocation)?;
        let mut fields = listing.split('\0').filter(|field| !field.is_empty());
        let mut changes = Vec::new();
        while let (Some(status), Some(file)) = (fields.next(), fields.next()) {
            changes.push((status.chars().next().unwrap_or('M'), file.to_owned()));
        }
        Ok(changes)
    }

    /// A zip archive of commit `rev` in `repo`, limited to `path` when
    /// given (entries keep their full path).
    ///
    /// # Errors
    ///
    /// When git fails, for instance when the commit is not in `repo`.
    pub fn archive(&self, repo: &Path, rev: &str, path: Option<&str>) -> Result<Vec<u8>, GitError> {
        let invocation = self
            .git(repo)
            // The committed bytes, without line-ending conversion.
            .args([
                "-c",
                "core.autocrlf=false",
                "archive",
                "--format=zip",
                rev,
                "--",
            ])
            .args(path);
        self.run("archive", &invocation).map(|out| out.stdout)
    }

    /// The content of `path` in the tree (or commit) `rev`, if it exists.
    pub fn show(&self, repo: &Path, rev: &str, path: &str) -> Option<String> {
        self.run(
            "show",
            &self.git(repo).arg("show").arg(format!("{rev}:{path}")),
        )
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The top-level directories of commit `rev`: the addons of a
    /// repository.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub fn dirs(&self, repo: &Path, rev: &str) -> Result<Vec<String>, GitError> {
        let listing = self.stdout(
            "ls-tree",
            &self.git(repo).args(["ls-tree", "-d", "--name-only", rev]),
        )?;
        Ok(listing.lines().map(str::to_owned).collect())
    }

    /// The object `spec` names in `repo`, such as `<commit>:<path>` for the
    /// tree of a directory.
    ///
    /// # Errors
    ///
    /// When git cannot resolve it.
    pub fn object(&self, repo: &Path, spec: &str) -> Result<String, GitError> {
        self.stdout(
            "rev-parse",
            &self
                .git(repo)
                .args(["rev-parse", "--verify", "--quiet", spec]),
        )
    }

    /// The tree of commit `rev` in `repo`.
    ///
    /// # Errors
    ///
    /// When git cannot resolve it.
    pub fn tree(&self, repo: &Path, rev: &str) -> Result<String, GitError> {
        self.stdout(
            "rev-parse",
            &self
                .git(repo)
                .arg("rev-parse")
                .arg(format!("{rev}^{{tree}}")),
        )
    }

    /// `git patch-id --stable` of the changes `sha` brings on top of `base`
    /// (from their merge base), so a rebase that keeps the change keeps the
    /// id. A commit already contained in `base` has no changes; its id is
    /// all zeros.
    ///
    /// # Errors
    ///
    /// When git fails.
    pub fn patch_id(&self, repo: &Path, base: &str, sha: &str) -> Result<String, GitError> {
        let fork_point = self.stdout(
            "merge-base",
            &self.git(repo).args(["merge-base", base, sha]),
        )?;
        let diff = self
            .run(
                "diff",
                &self
                    .git(repo)
                    .args(["diff", "--no-color", "--no-ext-diff", "--full-index"])
                    .args([fork_point.as_str(), sha]),
            )?
            .stdout;
        let id = self.stdout(
            "patch-id",
            &self.git(repo).args(["patch-id", "--stable"]).stdin(diff),
        )?;
        Ok(id
            .split_whitespace()
            .next()
            .map_or_else(|| "0".repeat(40), str::to_owned))
    }

    /// Push the working tree's commit to branch `branch` of `remote`
    /// (forced, as git-aggregator does).
    ///
    /// # Errors
    ///
    /// When git cannot push.
    pub fn push(&self, target: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
        let invocation = self
            .git(target)
            .args(["push", "--force", "--quiet", remote])
            .arg(format!("HEAD:refs/heads/{branch}"));
        self.run("push", &invocation).map(drop)
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
        let built = self.build(plan);
        // Rebased copies of earlier steps can make a later step conflict
        // where the original commits would not: the whole repository is
        // then built again with merge commits, as git-aggregator does.
        match (&built, plan.strategy, plan.keep_conflicts) {
            (Err(MergeError::Conflict { .. }), Strategy::Rebase, false) => {
                let merged = Aggregation {
                    strategy: Strategy::Merge,
                    ..plan.clone()
                };
                self.build(&merged).map(|built| Aggregated {
                    steps: built
                        .steps
                        .into_iter()
                        .map(|(label, outcome)| (label, outcome.instead_of_rebase()))
                        .collect(),
                    ..built
                })
            }
            _ => built,
        }
    }

    fn build(&self, plan: &Aggregation) -> Result<Aggregated, MergeError> {
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
    /// the base commit. A missing or empty directory becomes a new working
    /// tree.
    fn prepare(&self, plan: &Aggregation) -> Result<(), MergeError> {
        let cache = &plan.cache_repo;
        let target = &plan.target_dir;
        self.run("worktree", &self.git(cache).args(["worktree", "prune"]))?;
        let empty = fs::read_dir(target).is_ok_and(|mut entries| entries.next().is_none());
        if target.exists() && !empty {
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
            // A rebase kept for inspection would block the next one; the
            // result is irrelevant when none is in progress.
            let _ = self
                .runner
                .run(&self.git(target).args(["rebase", "--quit"]));
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

    /// Apply `step` with the plan's strategy. A step that cannot be rebased
    /// (its range replays merges that conflict when linearized) is merged
    /// with a merge commit instead; only when that conflicts too is the
    /// step a conflict.
    fn merge(
        &self,
        plan: &Aggregation,
        step: &Step,
        identity: &[(&str, &str)],
        attributes: Option<&Path>,
    ) -> Result<StepOutcome, MergeError> {
        let before = self.stdout(
            "rev-parse",
            &self.git(&plan.target_dir).args(["rev-parse", "HEAD"]),
        )?;
        if plan.strategy == Strategy::Rebase {
            match self.attempt(plan, step, identity, attributes, Strategy::Rebase, &before)? {
                Ok(outcome) => return Ok(outcome),
                Err(_) => self.abandon(plan, Strategy::Rebase, &before)?,
            }
            return match self.attempt(plan, step, identity, attributes, Strategy::Merge, &before)? {
                Ok(outcome) => Ok(outcome.instead_of_rebase()),
                Err(files) => self.conflict(plan, step, files, &before),
            };
        }
        match self.attempt(plan, step, identity, attributes, Strategy::Merge, &before)? {
            Ok(outcome) => Ok(outcome),
            Err(files) => self.conflict(plan, step, files, &before),
        }
    }

    /// Run `strategy` for `step`: the outcome, or the conflicting files with
    /// the operation still in progress.
    fn attempt(
        &self,
        plan: &Aggregation,
        step: &Step,
        identity: &[(&str, &str)],
        attributes: Option<&Path>,
        strategy: Strategy,
        before: &str,
    ) -> Result<Result<StepOutcome, Vec<String>>, MergeError> {
        let target = &plan.target_dir;
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
        let invocation = match strategy {
            Strategy::Merge => invocation
                .args(["merge", "--no-ff", "--no-edit", "--quiet", "-m"])
                .arg(format!("Merge {}", step.label))
                .arg(&step.sha),
            Strategy::Rebase => {
                let fork_point = self.stdout(
                    "merge-base",
                    &self.git(target).args(["merge-base", "HEAD"]).arg(&step.sha),
                )?;
                invocation
                    .args([
                        "-c",
                        "rebase.autoSquash=false",
                        "-c",
                        "rebase.updateRefs=false",
                    ])
                    .args(["rebase", "--quiet", "--no-autostash", "--onto"])
                    .arg(before)
                    .arg(fork_point)
                    .arg(&step.sha)
            }
        };
        let output = self.runner.run(&invocation).map_err(GitError::Io)?;
        if output.success() {
            let after = self.stdout("rev-parse", &self.git(target).args(["rev-parse", "HEAD"]))?;
            return Ok(Ok(if after == before {
                StepOutcome::UpToDate
            } else {
                StepOutcome::Merged
            }));
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
                command: strategy.command().to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }));
        }
        Ok(Err(files))
    }

    /// Abort `strategy` in progress and return to `before` (an aborted
    /// rebase returns to the step's commit, not to the result so far).
    fn abandon(
        &self,
        plan: &Aggregation,
        strategy: Strategy,
        before: &str,
    ) -> Result<(), GitError> {
        let target = &plan.target_dir;
        let command = strategy.command();
        self.run(command, &self.git(target).args([command, "--abort"]))?;
        self.run(
            "checkout",
            &self
                .git(target)
                .args(["checkout", "--detach", "--force", "--quiet"])
                .arg(before),
        )
        .map(drop)
    }

    /// A merge that conflicted: skipped, kept for inspection, or aborted.
    fn conflict(
        &self,
        plan: &Aggregation,
        step: &Step,
        files: Vec<String>,
        before: &str,
    ) -> Result<StepOutcome, MergeError> {
        let keep = plan.keep_conflicts && !step.skip_on_conflict;
        if !keep {
            self.abandon(plan, Strategy::Merge, before)?;
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
