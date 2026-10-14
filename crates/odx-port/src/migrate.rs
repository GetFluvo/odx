//! An addon's migration to another series the OCA way, on a branch.
//!
//! The branch gets the addon's history from the source series (`git
//! format-patch | git am`, authors and dates kept), the repository's
//! pre-commit fixes in their own commit, then odoo-lint's upgrade fixes,
//! the version bump and the removal of old migration scripts as the
//! `[MIG]` commit.
//!
//! A conflict while replaying the history stops at git's own `am` state;
//! the migration is kept in `<git dir>/odx-migrate.json` so it can be
//! continued or aborted.

use std::path::PathBuf;

use odx_core::{Invocation, ProcessRunner};
use serde::{Deserialize, Serialize};

use crate::squash::Rules;

/// The file, in the repository's git directory, that keeps a migration.
const STATE_FILE: &str = "odx-migrate.json";

/// What to migrate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The addon.
    pub addon: String,
    /// The source series.
    pub from: String,
    /// The target series.
    pub to: String,
    /// The remote both series are read from.
    pub remote: String,
    /// Leave the repository's pre-commit fixes out.
    #[serde(default)]
    pub no_pre_commit: bool,
    /// Keep the administrative commits of the history as they are.
    #[serde(default)]
    pub no_squash: bool,
    /// What makes a commit administrative.
    #[serde(default)]
    pub squash: Rules,
}

impl Plan {
    /// The migration branch: `<to>-mig-<addon>`.
    #[must_use]
    pub fn branch(&self) -> String {
        format!("{}-mig-{}", self.to, self.addon)
    }

    fn target(&self) -> String {
        format!("refs/remotes/{}/{}", self.remote, self.to)
    }

    fn source(&self) -> String {
        format!("refs/remotes/{}/{}", self.remote, self.from)
    }
}

/// The step a migration stands at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    /// Replaying the addon's history.
    History,
    /// The pre-commit and odoo-lint fixes.
    Fixes,
}

/// A migration in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// What is migrated.
    pub plan: Plan,
    /// Where it stands.
    pub step: Step,
    /// The branch (or commit) checked out before, restored by an abort.
    pub previous: String,
    /// The commit the migration branch started from.
    pub base: String,
}

/// The programs the fixes run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    /// odoo-lint's `odl`.
    pub odl: String,
    /// `pre-commit`; `None` leaves the pre-commit fixes out.
    pub pre_commit: Option<String>,
}

/// A finding odoo-lint leaves to fix by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The file, relative to the repository.
    #[serde(rename = "file_path")]
    pub file: String,
    /// The line.
    pub line: u64,
    /// The rule's code, e.g. `U1908`.
    pub code: String,
    /// What to do.
    pub message: String,
}

/// A finished migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// The migration branch.
    pub branch: String,
    /// The commits of history replayed from the source series, after the
    /// squash.
    pub history: usize,
    /// The administrative commits squashed into others.
    pub squashed: usize,
    /// The subjects of the commits the fixes made.
    pub commits: Vec<String>,
    /// What odoo-lint leaves to fix by hand.
    pub findings: Vec<Finding>,
    /// Steps that could not run, and why.
    pub warnings: Vec<String>,
}

/// An addon migrated in place by [`Worktree::migrate_in_place`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InPlace {
    /// The addon's directory, as given.
    pub addon: String,
    /// Whether the migration changed anything, committed as `[MIG]`.
    pub committed: bool,
    /// What odoo-lint leaves to fix by hand.
    pub findings: Vec<Finding>,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The branch is ready for review.
    Done(Summary),
    /// Replaying the history stopped on a conflict; the message says how to
    /// go on.
    Stopped(String),
}

/// The repository a migration runs in, with the runner its commands go
/// through.
#[derive(Debug, Clone)]
pub struct Worktree<'a> {
    runner: &'a dyn ProcessRunner,
    dir: PathBuf,
    /// `-c user.name=... -c user.email=...` for the commits, when set.
    identity: Vec<String>,
}

impl<'a> Worktree<'a> {
    /// The repository checked out at `dir`.
    pub fn new(runner: &'a dyn ProcessRunner, dir: impl Into<PathBuf>) -> Self {
        Self {
            runner,
            dir: dir.into(),
            identity: Vec::new(),
        }
    }

    /// Commit as `name` and `email` instead of git's configured identity.
    #[must_use]
    pub fn with_identity(mut self, name: &str, email: &str) -> Self {
        self.identity = vec![
            "-c".to_owned(),
            format!("user.name={name}"),
            "-c".to_owned(),
            format!("user.email={email}"),
        ];
        self
    }

    fn bytes(&self, invocation: &Invocation) -> Result<Vec<u8>, String> {
        let output = self.runner.run(invocation).map_err(|e| e.to_string())?;
        if output.success() {
            Ok(output.stdout)
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    pub(crate) fn output(&self, invocation: &Invocation) -> Result<String, String> {
        self.bytes(invocation)
            .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
    }

    pub(crate) fn command(&self) -> Invocation {
        Invocation::new("git")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(self.dir.as_os_str())
            .args(&self.identity)
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, String> {
        self.output(&self.command().args(args))
    }

    pub(crate) fn git_dir(&self) -> Result<PathBuf, String> {
        self.git(&["rev-parse", "--absolute-git-dir"])
            .map(PathBuf::from)
            .map_err(|e| format!("{} is not a git repository: {e}", self.dir.display()))
    }

    /// The migration in progress, if any.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        self.load(STATE_FILE)
    }

    /// What `file` in the git directory keeps.
    pub(crate) fn load<T: serde::de::DeserializeOwned>(&self, file: &str) -> Option<T> {
        let file = self.git_dir().ok()?.join(file);
        serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()
    }

    /// Keep `state` in `file` in the git directory.
    pub(crate) fn keep<T: Serialize>(&self, file: &str, state: &T) -> Result<(), String> {
        let file = self.git_dir()?.join(file);
        let text = serde_json::to_string_pretty(state).unwrap_or_default();
        std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))
    }

    fn save(&self, state: &State) -> Result<(), String> {
        self.keep(STATE_FILE, state)
    }

    /// Drop `file` from the git directory. A file git's own directory does
    /// not let go of is left for `--abort`.
    pub(crate) fn drop_file(&self, file: &str) {
        if let Ok(dir) = self.git_dir() {
            let _ = std::fs::remove_file(dir.join(file));
        }
    }

    fn forget(&self) {
        self.drop_file(STATE_FILE);
    }

    /// Whether `git am` stopped in the middle.
    fn applying(&self) -> bool {
        self.in_git_dir("rebase-apply")
    }

    /// Whether `name` exists in the git directory.
    pub(crate) fn in_git_dir(&self, name: &str) -> bool {
        self.git_dir().is_ok_and(|dir| dir.join(name).exists())
    }

    /// Start migrating: fetch both series from the remote, create the
    /// branch from the target series and replay the addon's history onto
    /// it, then run the fixes.
    ///
    /// # Errors
    ///
    /// When a migration is already in progress, the worktree has
    /// uncommitted changes, the series cannot be fetched, the addon is not
    /// on the source series or already on the target, or git fails.
    pub fn start(&self, plan: Plan, tools: &Tools) -> Result<Outcome, String> {
        if let Some(state) = self.state() {
            return Err(format!(
                "the migration of {} is in progress: run `odx migrate --continue` or `odx migrate --abort`",
                state.plan.addon
            ));
        }
        self.clean()?;
        let (target, source) = (plan.target(), plan.source());
        let refspecs = [
            format!("+refs/heads/{}:{target}", plan.to),
            format!("+refs/heads/{}:{source}", plan.from),
        ];
        self.git(&["fetch", "--quiet", &plan.remote, &refspecs[0], &refspecs[1]])
            .map_err(|e| {
                format!(
                    "cannot fetch {} and {} from {}: {e}",
                    plan.from, plan.to, plan.remote
                )
            })?;
        let manifest = |rev: &str| {
            self.git(&[
                "cat-file",
                "-e",
                &format!("{rev}:{}/__manifest__.py", plan.addon),
            ])
            .is_ok()
        };
        if !manifest(&source) {
            return Err(format!(
                "{} is not on {}/{}",
                plan.addon, plan.remote, plan.from
            ));
        }
        if manifest(&target) {
            return Err(format!(
                "{} is already on {}/{}",
                plan.addon, plan.remote, plan.to
            ));
        }
        let range = format!("{target}..{source}");
        let patches = self.bytes(&self.command().args([
            "format-patch",
            "--keep-subject",
            "--stdout",
            &range,
            "--",
            &plan.addon,
        ]))?;
        let previous = self.checked_out()?;
        let base = self.git(&["rev-parse", &target])?;
        let branch = plan.branch();
        self.git(&["switch", "--quiet", "--no-track", "-c", &branch, &target])
            .map_err(|e| format!("cannot create branch {branch}: {e}"))?;
        // Remembered for `odx migrate pr`.
        for (key, value) in [
            ("odxMigrateFrom", &plan.from),
            ("odxMigrateRemote", &plan.remote),
        ] {
            self.git(&["config", &format!("branch.{branch}.{key}"), value])?;
        }
        let state = State {
            plan,
            step: Step::History,
            previous,
            base,
        };
        self.save(&state)?;
        let applied = self.output(
            &self
                .command()
                .args(["am", "-3", "--keep", "--quiet"])
                .stdin(patches),
        );
        if let Err(e) = applied {
            return Ok(Outcome::Stopped(stopped(&e, MIGRATE)));
        }
        self.fixes(state, tools)
    }

    /// The top of the worktree.
    pub(crate) fn root(&self) -> Result<PathBuf, String> {
        self.git(&["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
    }

    /// Run a program through the worktree's runner.
    pub(crate) fn run(&self, invocation: &Invocation) -> std::io::Result<odx_core::Output> {
        self.runner.run(invocation)
    }

    /// Refuse a worktree with uncommitted changes.
    pub(crate) fn clean(&self) -> Result<(), String> {
        let changes = self.git(&["status", "--porcelain", "--untracked-files=no"])?;
        if changes.is_empty() {
            Ok(())
        } else {
            Err("the worktree has uncommitted changes; commit or stash them first".to_owned())
        }
    }

    /// The branch checked out, or the commit when detached.
    pub(crate) fn checked_out(&self) -> Result<String, String> {
        let name = self.git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let head = self.git(&["rev-parse", "HEAD"])?;
        Ok(Some(name).filter(|name| name != "HEAD").unwrap_or(head))
    }

    /// Go on after a conflict: `git am --continue` unless the history is
    /// already replayed, then the fixes.
    ///
    /// # Errors
    ///
    /// When no migration is in progress, or git fails.
    pub fn resume(&self, tools: &Tools) -> Result<Outcome, String> {
        let Some(state) = self.state() else {
            return Err("no migration is in progress".to_owned());
        };
        if state.step == Step::History && self.applying() {
            let continued = self.git(&["am", "--continue", "--quiet"]);
            if let Err(e) = continued {
                return Ok(Outcome::Stopped(stopped(&e, MIGRATE)));
            }
        }
        self.fixes(state, tools)
    }

    /// Give up: abort `git am` if it stopped, check out what was checked
    /// out before and delete the migration branch.
    ///
    /// # Errors
    ///
    /// When no migration is in progress, or git fails.
    pub fn abort(&self) -> Result<State, String> {
        let Some(state) = self.state() else {
            return Err("no migration is in progress".to_owned());
        };
        if self.applying() {
            self.git(&["am", "--abort"])?;
        }
        self.git(&["checkout", "--quiet", &state.previous])?;
        self.git(&["branch", "--quiet", "-D", &state.plan.branch()])?;
        self.forget();
        Ok(state)
    }

    /// Migrate addons in place, as a project does with its own: for each
    /// directory (relative to the worktree), odoo-lint's upgrade fixes and
    /// version bump, old migration scripts removed and pre-commit run, in
    /// a commit `[MIG] <addon>: Migration to <to>` when anything changed.
    /// The addons and the steps that could not run.
    ///
    /// # Errors
    ///
    /// When the worktree has uncommitted changes, or git fails.
    pub fn migrate_in_place(
        &self,
        dirs: &[String],
        to: &str,
        tools: &Tools,
    ) -> Result<(Vec<InPlace>, Vec<String>), String> {
        self.clean()?;
        let mut warnings = Vec::new();
        let mut done = Vec::new();
        for dir in dirs {
            let fix = Invocation::new(&tools.odl).current_dir(&self.dir).args([
                "upgrade-check",
                "--target",
                to,
                "--fix",
                "--unsafe-fixes",
                "--bump-version",
                dir,
            ]);
            if let Err(e) = self.runner.run(&fix) {
                let warning = format!(
                    "{}: {e}; install odoo-lint for the automatic upgrade fixes",
                    tools.odl
                );
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
            let scripts = format!("{dir}/migrations");
            if self.dir.join(&scripts).is_dir() {
                self.git(&["rm", "-r", "--quiet", "--", &scripts])?;
            }
            self.pre_commit(dir, tools, &mut warnings)?;
            let name = dir.rsplit('/').next().unwrap_or(dir);
            let committed = self.commit(dir, &format!("[MIG] {name}: Migration to {to}"), false)?;
            done.push(InPlace {
                addon: dir.clone(),
                committed,
                findings: self.findings(dir, to, tools),
            });
        }
        Ok((done, warnings))
    }

    /// The pre-commit fixes in their own commit, then odoo-lint's upgrade
    /// fixes, the version bump and the removal of old migration scripts in
    /// the `[MIG]` commit.
    fn fixes(&self, mut state: State, tools: &Tools) -> Result<Outcome, String> {
        state.step = Step::Fixes;
        self.save(&state)?;
        let plan = &state.plan;
        let addon = plan.addon.as_str();
        let tools = &Tools {
            odl: tools.odl.clone(),
            pre_commit: tools.pre_commit.clone().filter(|_| !plan.no_pre_commit),
        };
        let (squashed, conflict) = if plan.no_squash {
            (0, None)
        } else {
            self.squash_administrative(&state.base, &plan.squash)
        };
        let history = self
            .git(&["rev-list", "--count", &format!("{}..HEAD", state.base)])?
            .parse()
            .unwrap_or_default();
        let mut summary = Summary {
            branch: plan.branch(),
            history,
            squashed,
            commits: Vec::new(),
            findings: Vec::new(),
            warnings: conflict.into_iter().collect(),
        };
        self.pre_commit(addon, tools, &mut summary.warnings)?;
        let subject = format!("[IMP] {addon}: pre-commit auto fixes");
        if self.commit(addon, &subject, false)? {
            summary.commits.push(subject);
        }
        let fix = Invocation::new(&tools.odl).current_dir(&self.dir).args([
            "upgrade-check",
            "--target",
            &plan.to,
            "--fix",
            "--unsafe-fixes",
            "--bump-version",
            addon,
        ]);
        // odl exits non-zero while findings remain; only a missing program
        // is a problem.
        if let Err(e) = self.runner.run(&fix) {
            summary.warnings.push(format!(
                "{}: {e}; install odoo-lint for the automatic upgrade fixes",
                tools.odl
            ));
        }
        let scripts = format!("{addon}/migrations");
        if self.dir.join(&scripts).is_dir() {
            self.git(&["rm", "-r", "--quiet", "--", &scripts])?;
        }
        self.pre_commit(addon, tools, &mut summary.warnings)?;
        let subject = format!("[MIG] {addon}: Migration to {}", plan.to);
        self.commit(addon, &subject, true)?;
        summary.commits.push(subject);
        summary.findings = self.findings(addon, &plan.to, tools);
        self.forget();
        Ok(Outcome::Done(summary))
    }

    /// Run the repository's pre-commit hooks on the addon's files, when it
    /// has a configuration.
    fn pre_commit(
        &self,
        addon: &str,
        tools: &Tools,
        warnings: &mut Vec<String>,
    ) -> Result<(), String> {
        let Some(program) = tools
            .pre_commit
            .as_deref()
            .filter(|_| self.dir.join(".pre-commit-config.yaml").is_file())
        else {
            return Ok(());
        };
        let files = self.git(&["ls-files", "--", addon])?;
        let run = Invocation::new(program)
            .current_dir(&self.dir)
            .args(["run", "--files"])
            .args(files.lines());
        // Hooks that changed files exit non-zero; the changes are the point.
        if let Err(e) = self.runner.run(&run) {
            let warning = format!("{program}: {e}; the pre-commit fixes are left out");
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        Ok(())
    }

    /// Commit the addon's changes as `subject`; whether there was anything
    /// to commit (always true with `allow_empty`).
    pub(crate) fn commit(
        &self,
        addon: &str,
        subject: &str,
        allow_empty: bool,
    ) -> Result<bool, String> {
        self.git(&["add", "--all", "--", addon])?;
        let changed = self.git(&["diff", "--cached", "--quiet"]).is_err();
        if changed || allow_empty {
            self.git(&[
                "commit",
                "--quiet",
                "--no-verify",
                "--allow-empty",
                "-m",
                subject,
            ])?;
        }
        Ok(changed || allow_empty)
    }

    /// What odoo-lint leaves to fix by hand in `addon` for series `to`;
    /// nothing when it cannot tell.
    #[must_use]
    pub fn findings(&self, addon: &str, to: &str, tools: &Tools) -> Vec<Finding> {
        #[derive(Deserialize)]
        struct Module {
            #[serde(default)]
            violations: Vec<Finding>,
        }
        #[derive(Deserialize)]
        struct Answer {
            modules: Vec<Module>,
        }
        let check = Invocation::new(&tools.odl).current_dir(&self.dir).args([
            "upgrade-check",
            "--target",
            to,
            "--output-format",
            "json",
            "--show-findings",
            addon,
        ]);
        self.runner
            .run(&check)
            .ok()
            .and_then(|output| serde_json::from_slice::<Answer>(&output.stdout).ok())
            .map(|answer| {
                answer
                    .modules
                    .into_iter()
                    .flat_map(|module| module.violations)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The command `--continue` and `--abort` belong to.
const MIGRATE: &str = "odx migrate";

/// How to go on after git stopped replaying commits for `command`.
pub(crate) fn stopped(error: &str, command: &str) -> String {
    let detail = error
        .lines()
        .find(|line| line.starts_with("Patch failed") || line.starts_with("error:"))
        .unwrap_or("a commit does not apply");
    format!(
        "replaying the history stopped: {detail}\nresolve the conflict, `git add` the files, then run `{command} --continue` (or `{command} --abort`)"
    )
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

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn commit(dir: &Path, author: &str, message: &str) {
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "commit",
                "--quiet",
                "--author",
                &format!("{author} <{}@example.com>", author.to_lowercase()),
                "--date",
                "2024-03-01T10:00:00+01:00",
                "-m",
                message,
            ],
        );
    }

    /// An upstream with the addon on 18.0 (two commits by Jane, one of
    /// another addon) and an unrelated 19.0, and a clone of it.
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
        write(
            &up.join("my_addon/__manifest__.py"),
            "{\"version\": \"18.0.1.0.0\"}\n",
        );
        write(
            &up.join("my_addon/migrations/18.0.1.0.1/pre-migrate.py"),
            "\n",
        );
        commit(&up, "Jane", "[ADD] my_addon");
        write(&up.join("other/__manifest__.py"), "{}\n");
        commit(&up, "Joe", "[ADD] other");
        write(&up.join("my_addon/data.xml"), "<odoo/>\n");
        commit(&up, "Jane", "[IMP] my_addon: data");
        git(&up, &["checkout", "--quiet", "--orphan", "19.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        write(&up.join("README.md"), "19.0\n");
        write(&up.join(".pre-commit-config.yaml"), "repos: []\n");
        commit(&up, "Joe", "[INIT] 19.0");
        // Tests push to both series.
        git(&up, &["checkout", "--quiet", "--detach"]);
        let work = tmp.path().join("work");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                "--branch",
                "19.0",
                up.to_str().unwrap(),
                "work",
            ],
        );
        git(&work, &["config", "user.name", "Me"]);
        git(&work, &["config", "user.email", "me@example.com"]);
        (tmp, work)
    }

    /// The summary of a finished migration.
    ///
    /// Excluded from coverage, like the other outcome helpers: their
    /// panics only run when a test fails.
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn done(outcome: Result<Outcome, String>) -> Summary {
        match outcome {
            Ok(Outcome::Done(summary)) => summary,
            other => panic!("not done: {other:?}"),
        }
    }

    /// The message of a migration that stopped on a conflict.
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn halted(outcome: Result<Outcome, String>) -> String {
        match outcome {
            Ok(Outcome::Stopped(message)) => message,
            other => panic!("not stopped: {other:?}"),
        }
    }

    /// The error of a refused migration.
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn refused(outcome: Result<Outcome, String>) -> String {
        match outcome {
            Err(e) => e,
            other => panic!("not refused: {other:?}"),
        }
    }

    fn plan() -> Plan {
        Plan {
            addon: "my_addon".to_owned(),
            from: "18.0".to_owned(),
            to: "19.0".to_owned(),
            remote: "origin".to_owned(),
            no_pre_commit: false,
            no_squash: false,
            squash: Rules::default(),
        }
    }

    fn tools(odl: &str, pre_commit: Option<&str>) -> Tools {
        Tools {
            odl: odl.to_owned(),
            pre_commit: pre_commit.map(str::to_owned),
        }
    }

    /// The history under the `[MIG]` commit: author, author date,
    /// committer, subject.
    fn log(dir: &Path) -> String {
        git(
            dir,
            &[
                "log",
                "--format=%an|%ad|%cn|%s",
                "--date=iso-strict",
                "19.0-mig-my_addon~1",
            ],
        )
    }

    #[test]
    fn history_then_the_migration_commit() {
        let (_tmp, work) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        let missing = tools("odx-test-no-such-program", Some("odx-test-no-such-program"));
        let skipped = Plan {
            no_pre_commit: true,
            ..plan()
        };
        let summary = done(tree.start(skipped, &missing));
        assert_eq!(summary.branch, "19.0-mig-my_addon");
        assert_eq!(summary.history, 2);
        assert_eq!(summary.commits, ["[MIG] my_addon: Migration to 19.0"]);
        assert_eq!(summary.findings, []);
        assert_eq!(summary.warnings.len(), 1, "{:?}", summary.warnings);
        assert!(summary.warnings[0].contains("install odoo-lint"));
        // Authors and dates of the history are kept; the committer is the
        // user.
        assert_eq!(
            log(&work),
            "Jane|2024-03-01T10:00:00+01:00|Me|[IMP] my_addon: data\n\
             Jane|2024-03-01T10:00:00+01:00|Me|[ADD] my_addon\n\
             Joe|2024-03-01T10:00:00+01:00|Odx|[INIT] 19.0"
        );
        assert!(!work.join("my_addon/migrations").exists());
        assert!(work.join("my_addon/data.xml").exists());
        assert!(!work.join("other").exists());
        assert_eq!(tree.state(), None);
        assert_eq!(
            git(&work, &["branch", "--show-current"]),
            "19.0-mig-my_addon"
        );
        assert_eq!(
            git(
                &work,
                &["config", "branch.19.0-mig-my_addon.odxMigrateFrom"]
            ),
            "18.0"
        );
    }

    #[test]
    fn refusals() {
        let (tmp, work) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        let none = tools("odx-test-no-such-program", None);
        let start = |plan: Plan| refused(tree.start(plan, &none));
        let mut other = plan();
        other.addon = "nope".to_owned();
        assert_eq!(start(other), "nope is not on origin/18.0");
        let mut other = plan();
        other.remote = "nowhere".to_owned();
        assert!(start(other).starts_with("cannot fetch 18.0 and 19.0 from nowhere: "));
        write(&work.join("README.md"), "changed\n");
        assert!(start(plan()).contains("uncommitted changes"));
        git(&work, &["checkout", "--quiet", "--", "README.md"]);
        git(&work, &["branch", "19.0-mig-my_addon"]);
        assert!(start(plan()).starts_with("cannot create branch 19.0-mig-my_addon: "));
        git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
        // A state that cannot be kept stops the migration.
        let blocked = work.join(".git").join(STATE_FILE);
        std::fs::create_dir(&blocked).unwrap();
        assert!(start(plan()).contains(STATE_FILE));
        std::fs::remove_dir(&blocked).unwrap();
        git(&work, &["checkout", "--quiet", "19.0"]);
        git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
        let _ = done(tree.start(plan(), &none));
        // Once upstream has it, there is nothing to migrate.
        git(
            &work,
            &["push", "--quiet", "origin", "19.0-mig-my_addon:19.0"],
        );
        git(&work, &["checkout", "--quiet", "19.0"]);
        assert_eq!(start(plan()), "my_addon is already on origin/19.0");
        // Outside a repository nothing is in progress.
        let outside = Worktree::new(&OsRunner, tmp.path());
        assert_eq!(outside.state(), None);
        assert!(outside.start(plan(), &none).is_err());
        assert_eq!(
            outside.resume(&none),
            Err("no migration is in progress".to_owned())
        );
        assert_eq!(
            outside.abort(),
            Err("no migration is in progress".to_owned())
        );
    }

    /// The fixture with 19.0 holding a different `my_addon/data.xml`, so
    /// replaying the history stops on its last commit.
    fn conflicting() -> (tempfile::TempDir, PathBuf, Worktree<'static>) {
        let (tmp, work) = fixture();
        write(&work.join("my_addon/data.xml"), "<odoo><data/></odoo>\n");
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "data"]);
        git(&work, &["push", "--quiet", "origin", "HEAD:19.0"]);
        let tree = Worktree::new(&OsRunner, work.clone());
        (tmp, work, tree)
    }

    #[test]
    fn conflicts_continue() {
        let (_tmp, work, tree) = conflicting();
        let none = tools("odx-test-no-such-program", None);
        let message = halted(tree.start(plan(), &none));
        assert!(message.contains("odx migrate --continue"), "{message}");
        assert_eq!(tree.state().unwrap().step, Step::History);
        assert!(
            tree.start(plan(), &none)
                .unwrap_err()
                .contains("is in progress")
        );
        // Unresolved, it stops again.
        let _ = halted(tree.resume(&none));
        // Resolved: the patch applied by hand, staged.
        let patch = git(&work, &["am", "--show-current-patch=diff"]);
        git(&work, &["rm", "--quiet", "-f", "my_addon/data.xml"]);
        let applied = std::process::Command::new("git")
            .current_dir(&work)
            .args(["apply", "--index"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(format!("{patch}\n").as_bytes())
                    .unwrap();
                child.wait()
            })
            .unwrap();
        assert!(applied.success());
        let summary = done(tree.resume(&none));
        assert_eq!(summary.history, 2);
        // Continuing again finds nothing in progress.
        assert!(tree.resume(&none).is_err());
    }

    #[test]
    fn conflicts_abort() {
        let (_tmp, work, tree) = conflicting();
        let none = tools("odx-test-no-such-program", None);
        halted(tree.start(plan(), &none));
        let state = tree.abort().unwrap();
        assert_eq!(state.previous, "19.0");
        assert_eq!(git(&work, &["branch", "--show-current"]), "19.0");
        assert_eq!(git(&work, &["branch", "--list", "19.0-mig-*"]), "");
        assert_eq!(tree.state(), None);
        // `git am --abort` run by hand: aborting still restores the branch.
        halted(tree.start(plan(), &none));
        git(&work, &["am", "--abort"]);
        assert_eq!(tree.abort().unwrap().step, Step::History);
        // `git am --skip` run by hand: continuing goes on to the fixes.
        halted(tree.start(plan(), &none));
        git(&work, &["am", "--skip"]);
        assert_eq!(done(tree.resume(&none)).history, 1);
        assert_eq!(
            stopped("x", MIGRATE),
            "replaying the history stopped: a commit does not apply\nresolve the conflict, `git add` the files, then run `odx migrate --continue` (or `odx migrate --abort`)"
        );
    }

    #[test]
    fn own_addons_migrate_in_place() {
        let (_tmp, work) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        // An own addon of the 19.0 branch with old migration scripts.
        write(&work.join("own/a/__manifest__.py"), "{}\n");
        write(&work.join("own/a/migrations/18.0.1.0.1/pre.py"), "\n");
        write(&work.join("own/b/__manifest__.py"), "{}\n");
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "own"]);
        let dirs = ["own/a".to_owned(), "own/b".to_owned()];
        let missing = tools("odx-test-no-such-program", Some("odx-test-no-such-program"));
        let (done, warnings) = tree.migrate_in_place(&dirs, "19.0", &missing).unwrap();
        assert_eq!(
            done.iter().map(|addon| addon.committed).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(
            git(&work, &["log", "-1", "--format=%s"]),
            "[MIG] a: Migration to 19.0"
        );
        assert!(!work.join("own/a/migrations").exists());
        write(&work.join("own/b/x.py"), "x\n");
        git(&work, &["add", "-A"]);
        assert!(
            tree.migrate_in_place(&dirs, "19.0", &missing)
                .unwrap_err()
                .contains("uncommitted")
        );
    }

    /// A commit for [`administrative`]: author, e-mail, subject, files.
    type Entry<'a> = (&'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

    /// An upstream whose 18.0 history of `my_addon` is `steps`, an empty
    /// 19.0, and a clone on 19.0.
    fn administrative(steps: &[Entry<'_>]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
        for (author, email, subject, files) in steps {
            for (file, text) in *files {
                write(&up.join("my_addon").join(file), text);
            }
            git(&up, &["add", "-A"]);
            git(
                &up,
                &[
                    "commit",
                    "--quiet",
                    "--author",
                    &format!("{author} <{email}>"),
                    "-m",
                    subject,
                ],
            );
        }
        git(&up, &["checkout", "--quiet", "--orphan", "19.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        write(&up.join("README.md"), "19.0\n");
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "[INIT] 19.0"]);
        git(&up, &["checkout", "--quiet", "--detach"]);
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                "--branch",
                "19.0",
                up.to_str().unwrap(),
                "work",
            ],
        );
        let work = tmp.path().join("work");
        git(&work, &["config", "user.name", "Me"]);
        git(&work, &["config", "user.email", "me@example.com"]);
        (tmp, work)
    }

    const JANE: &str = "jane@example.com";
    const NL: &str = "nl@example.com";
    const BOT: &str = "oca-git-bot@odoo-community.org";
    const CI: &str = "oca-ci@odoo-community.org";

    /// The history under the `[MIG]` commit: author and subject.
    fn kept(work: &Path) -> String {
        git(
            work,
            &[
                "log",
                "--format=%an|%s",
                "19.0-mig-my_addon~1",
                "^origin/19.0",
            ],
        )
    }

    #[test]
    fn administrative_commits_are_squashed() {
        let manifest: &[(&str, &str)] = &[("__manifest__.py", "{}\n"), ("i18n/nl.po", "a\n")];
        let (_tmp, work) = administrative(&[
            ("Jane", JANE, "[ADD] my_addon", manifest),
            (
                "OCA-git-bot",
                BOT,
                "[BOT] post-merge updates",
                &[("README.rst", "r\n")],
            ),
            (
                "Nl",
                NL,
                "Translated using Weblate (Dutch)",
                &[("i18n/nl.po", "a\nb\n")],
            ),
            ("Jane", JANE, "[FIX] my_addon: x", &[("x.py", "x\n")]),
            (
                "Nl",
                NL,
                "Translated using Weblate (Dutch)",
                &[("i18n/nl.po", "a\nb\nc\n")],
            ),
            (
                "oca-ci",
                CI,
                "[UPD] Update my_addon.pot",
                &[("i18n/my_addon.pot", "p\n")],
            ),
        ]);
        let tree = Worktree::new(&OsRunner, &work);
        let summary = done(tree.start(plan(), &tools("odx-test-no-such-program", None)));
        assert_eq!((summary.history, summary.squashed), (3, 3));
        assert_eq!(
            kept(&work),
            "Jane|[FIX] my_addon: x\nNl|Translated using Weblate (Dutch)\nJane|[ADD] my_addon"
        );
        for file in ["README.rst", "x.py", "i18n/my_addon.pot"] {
            assert!(work.join("my_addon").join(file).exists(), "{file}");
        }
        assert_eq!(
            std::fs::read_to_string(work.join("my_addon/i18n/nl.po")).unwrap(),
            "a\nb\nc\n"
        );
        // Kept as replayed when asked.
        git(&work, &["checkout", "--quiet", "19.0"]);
        git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
        let unsquashed = Plan {
            no_squash: true,
            ..plan()
        };
        let summary = done(tree.start(unsquashed, &tools("odx-test-no-such-program", None)));
        assert_eq!((summary.history, summary.squashed), (6, 0));
    }

    #[test]
    fn squashes_that_conflict_fall_back() {
        let manifest: &[(&str, &str)] = &[("__manifest__.py", "{}\n"), ("i18n/nl.po", "a\n")];
        // The fix changes the line the first translation added: the second
        // translation cannot move before it, the bot commit can.
        let (_tmp, work) = administrative(&[
            ("Jane", JANE, "[ADD] my_addon", manifest),
            (
                "OCA-git-bot",
                BOT,
                "[BOT] post-merge updates",
                &[("README.rst", "r\n")],
            ),
            (
                "Nl",
                NL,
                "Translated using Weblate (Dutch)",
                &[("i18n/nl.po", "a\nb\n")],
            ),
            (
                "Jane",
                JANE,
                "[FIX] my_addon: b",
                &[("i18n/nl.po", "a\nB\n")],
            ),
            (
                "Nl",
                NL,
                "Translated using Weblate (Dutch)",
                &[("i18n/nl.po", "a\nB\nc\n")],
            ),
        ]);
        let none = tools("odx-test-no-such-program", None);
        let summary = done(Worktree::new(&OsRunner, &work).start(plan(), &none));
        assert_eq!((summary.history, summary.squashed), (4, 1));
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| w.starts_with("1 administrative")),
            "{:?}",
            summary.warnings
        );
        // A bot commit that cannot move either: the history is kept.
        let (_tmp, work) = administrative(&[
            ("Jane", JANE, "[ADD] my_addon", manifest),
            (
                "Nl",
                NL,
                "Translated using Weblate (Dutch)",
                &[("i18n/nl.po", "a\nb\n")],
            ),
            (
                "oca-ci",
                CI,
                "Update translation files",
                &[("i18n/nl.po", "a\nb2\n")],
            ),
        ]);
        let summary = done(Worktree::new(&OsRunner, &work).start(plan(), &none));
        assert_eq!((summary.history, summary.squashed), (3, 0));
        assert!(
            summary
                .warnings
                .iter()
                .any(|w| w.contains("are kept as they are")),
            "{:?}",
            summary.warnings
        );
    }

    #[test]
    fn every_command_can_fail() {
        use crate::testing::Faulty;
        let none = tools("odx-test-no-such-program", Some("odx-test-no-such-program"));
        let calls = |run: &dyn Fn(&Faulty)| {
            let counter = Faulty::new(usize::MAX, false);
            run(&counter);
            counter.calls.into_inner()
        };
        // A whole migration, failing at every command in turn.
        let start = |runner: &Faulty| {
            let (_tmp, work) = fixture();
            let _ = Worktree::new(runner, &work).start(plan(), &none);
        };
        // An abort after a conflict, failing at every command in turn.
        let abort = |runner: &Faulty| {
            let (_tmp, work, tree) = conflicting();
            halted(tree.start(plan(), &none));
            let _ = Worktree::new(runner, &work).abort();
        };
        let in_place = |runner: &Faulty| {
            let (_tmp, work) = fixture();
            write(&work.join("own/a/migrations/x.py"), "\n");
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "--quiet", "-m", "own"]);
            let missing = tools("odx-test-no-such-program", Some("odx-test-no-such-program"));
            let _ = Worktree::new(runner, &work).migrate_in_place(
                &["own/a".to_owned()],
                "19.0",
                &missing,
            );
        };
        for scenario in [&start as &dyn Fn(&Faulty), &abort, &in_place] {
            // A program that does not start fails the same way everywhere.
            scenario(&Faulty::new(0, true));
            for fail_at in 0..calls(scenario) {
                scenario(&Faulty::new(fail_at, false));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn pre_commit_and_odoo_lint_fixes() {
        use std::os::unix::fs::PermissionsExt as _;
        let (tmp, work) = fixture();
        // Without old migration scripts on 18.0, there is nothing to remove.
        git(&work, &["switch", "--quiet", "-c", "drop", "origin/18.0"]);
        git(&work, &["rm", "-r", "--quiet", "my_addon/migrations"]);
        git(&work, &["commit", "--quiet", "-m", "drop scripts"]);
        git(&work, &["push", "--quiet", "origin", "drop:18.0"]);
        git(&work, &["switch", "--quiet", "19.0"]);
        let script = |name: &str, body: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.display().to_string()
        };
        // pre-commit adds a trailing comment to every file it is given.
        let pre_commit = script(
            "pre-commit",
            "shift 2\nfor f in \"$@\"; do echo '# fixed' >> \"$f\"; done\nexit 1\n",
        );
        let odl = script(
            "odl",
            r#"case "$*" in
  *--fix*) echo '{"version": "19.0.1.0.0"}' > my_addon/__manifest__.py; exit 1;;
  *) echo '{"modules":[{"name":"my_addon","violations":[{"file_path":"my_addon/data.xml","line":1,"column":1,"code":"U1908","name":"x","message":"by hand"}]}]}'; exit 1;;
esac
"#,
        );
        let tree = Worktree::new(&OsRunner, &work);
        let summary = done(tree.start(plan(), &tools(&odl, Some(&pre_commit))));
        assert_eq!(
            summary.commits,
            [
                "[IMP] my_addon: pre-commit auto fixes",
                "[MIG] my_addon: Migration to 19.0"
            ]
        );
        assert_eq!(summary.warnings, Vec::<String>::new());
        assert_eq!(
            summary.findings,
            [Finding {
                file: "my_addon/data.xml".to_owned(),
                line: 1,
                code: "U1908".to_owned(),
                message: "by hand".to_owned(),
            }]
        );
        let manifest = std::fs::read_to_string(work.join("my_addon/__manifest__.py")).unwrap();
        assert!(manifest.contains("19.0.1.0.0"), "{manifest}");
        let changed = git(&work, &["show", "--name-only", "--format=", "HEAD"]);
        assert!(changed.contains("my_addon/__manifest__.py"), "{changed}");
    }
}
