//! Porting commits to another series: cherry-picked with `git cherry-pick
//! -x` onto a branch of the target series, authors and dates kept, then
//! odoo-lint's upgrade fixes per addon, each in a `[FW]` commit.
//!
//! A conflict stops at git's own cherry-pick state; the port is kept in
//! `<git dir>/odx-port.json` so it can be continued or aborted.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::blacklist::{self, Blacklist};
use crate::migrate::{Finding, Tools, Worktree, stopped};

/// The file, in the repository's git directory, that keeps a port.
const STATE_FILE: &str = "odx-port.json";

/// The command `--continue` and `--abort` belong to.
const PORT: &str = "odx port";

/// What to port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortPlan {
    /// The branch to create.
    pub branch: String,
    /// The target series.
    pub to: String,
    /// The remote the target series is read from.
    pub remote: String,
    /// The commits, oldest first; their objects must be in the repository.
    pub commits: Vec<String>,
    /// The addons the commits change, for the upgrade fixes.
    pub addons: Vec<String>,
    /// Leave odoo-lint's upgrade fixes out.
    #[serde(default)]
    pub no_migrate: bool,
    /// What the caller wants back when the port is continued.
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

/// A port in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortState {
    /// What is ported.
    pub plan: PortPlan,
    /// The branch (or commit) checked out before, restored by an abort.
    pub previous: String,
    /// The commit the branch started from.
    pub base: String,
}

/// A finished port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ported {
    /// The branch.
    pub branch: String,
    /// The commits cherry-picked.
    pub picked: usize,
    /// The subjects of the commits the fixes made.
    pub commits: Vec<String>,
    /// What odoo-lint leaves to fix by hand in the addons.
    pub findings: Vec<Finding>,
    /// Steps that could not run, and why.
    pub warnings: Vec<String>,
}

/// How a port ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortOutcome {
    /// The branch is ready for review.
    Done(Ported),
    /// A cherry-pick stopped on a conflict; the message says how to go on.
    Stopped(String),
}

impl Worktree<'_> {
    /// The port in progress, if any.
    #[must_use]
    pub fn port_state(&self) -> Option<PortState> {
        self.load(STATE_FILE)
    }

    /// Fetch `refspecs` from `remote`.
    ///
    /// # Errors
    ///
    /// When git cannot fetch them.
    pub fn fetch(&self, remote: &str, refspecs: &[String]) -> Result<(), String> {
        let mut args = vec!["fetch", "--quiet", remote];
        args.extend(refspecs.iter().map(String::as_str));
        self.git(&args)
            .map(drop)
            .map_err(|e| format!("cannot fetch from {remote}: {e}"))
    }

    /// Start porting: fetch the target series, create the branch from it,
    /// cherry-pick the commits, then run the fixes.
    ///
    /// # Errors
    ///
    /// When a port or a migration is already in progress, the worktree has
    /// uncommitted changes, the target cannot be fetched, or git fails.
    pub fn port(&self, plan: PortPlan, tools: &Tools) -> Result<PortOutcome, String> {
        if self.has(STATE_FILE) || self.migrating() {
            return Err(format!(
                "a port or migration is in progress: run `{PORT} --continue` or `{PORT} --abort` (`odx migrate` for a migration)"
            ));
        }
        self.clean()?;
        let target = format!("refs/remotes/{}/{}", plan.remote, plan.to);
        self.fetch(&plan.remote, &[format!("+refs/heads/{}:{target}", plan.to)])?;
        let previous = self.checked_out()?;
        let base = self.git(&["rev-parse", &target])?;
        let branch = plan.branch.clone();
        self.git(&["switch", "--quiet", "--no-track", "-c", &branch, &target])
            .map_err(|e| format!("cannot create branch {branch}: {e}"))?;
        let state = PortState {
            plan,
            previous,
            base,
        };
        self.keep(STATE_FILE, &state)?;
        let mut pick = vec!["-c", "core.editor=true", "cherry-pick", "-x"];
        pick.extend(state.plan.commits.iter().map(String::as_str));
        if let Err(e) = self.git(&pick) {
            return Ok(PortOutcome::Stopped(stopped(&e, PORT)));
        }
        self.port_fixes(&state, tools)
    }

    /// Fetch series `to` from `remote` and check out a new branch `branch`
    /// from it.
    ///
    /// # Errors
    ///
    /// When a port or migration is in progress, the worktree has
    /// uncommitted changes, the series cannot be fetched, or the branch
    /// cannot be created.
    pub fn branch_from(&self, remote: &str, to: &str, branch: &str) -> Result<(), String> {
        if self.has(STATE_FILE) || self.migrating() {
            return Err(format!(
                "a port or migration is in progress: finish it with `{PORT} --continue` or `{PORT} --abort` first"
            ));
        }
        self.clean()?;
        let target = format!("refs/remotes/{remote}/{to}");
        self.fetch(remote, &[format!("+refs/heads/{to}:{target}")])?;
        self.git(&["switch", "--quiet", "--no-track", "-c", branch, &target])
            .map(drop)
            .map_err(|e| format!("cannot create branch {branch}: {e}"))
    }

    /// Add `entries` (key and reason) to `addon`'s oca-port blacklist in
    /// the worktree, written as oca-port writes it, and commit only that
    /// file as `[FW] <addon>: blacklist <n> port suggestion(s)`; the
    /// subject.
    ///
    /// # Errors
    ///
    /// When an entry has no reason, the file cannot be written, or git
    /// fails.
    pub fn blacklist(&self, addon: &str, entries: &[(String, String)]) -> Result<String, String> {
        let root = self.root()?;
        let relative = blacklist::path(addon);
        let file = root.join(&relative);
        // A blacklist that cannot be read is not replaced by the new entries
        // alone.
        let mut list = match std::fs::read_to_string(&file) {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(Blacklist::default())
            }
            read => read
                .map_err(|e| e.to_string())
                .and_then(|text| Blacklist::parse(&text)),
        }
        .map_err(|e| format!("{}: {e}", file.display()))?;
        for (key, reason) in entries {
            list.add(key, reason)?;
        }
        let dir = root.join(".oca/oca-port/blacklist");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(&file, list.render()).map_err(|e| format!("{}: {e}", file.display()))?;
        self.git(&["add", "--", &relative])?;
        let subject = format!(
            "[FW] {addon}: blacklist {} port suggestion(s)",
            entries.len()
        );
        self.git(&[
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            &subject,
            "--",
            &relative,
        ])?;
        Ok(subject)
    }

    /// Whether a cherry-pick stopped in the middle.
    fn picking(&self) -> bool {
        self.in_git_dir("CHERRY_PICK_HEAD") || self.in_git_dir("sequencer")
    }

    /// Go on after a conflict: `git cherry-pick --continue` unless the
    /// commits are all picked, then the fixes.
    ///
    /// # Errors
    ///
    /// When no port is in progress, or git fails.
    pub fn port_resume(&self, tools: &Tools) -> Result<PortOutcome, String> {
        let Some(state) = self.port_state() else {
            return Err("no port is in progress".to_owned());
        };
        if self.picking() {
            let continued = self.git(&["-c", "core.editor=true", "cherry-pick", "--continue"]);
            if let Err(e) = continued {
                return Ok(PortOutcome::Stopped(stopped(&e, PORT)));
            }
        }
        self.port_fixes(&state, tools)
    }

    /// Give up: abort the cherry-pick if it stopped, check out what was
    /// checked out before and delete the branch.
    ///
    /// # Errors
    ///
    /// When no port is in progress, or git fails.
    pub fn port_abort(&self) -> Result<PortState, String> {
        let Some(state) = self.port_state() else {
            return Err("no port is in progress".to_owned());
        };
        if self.picking() {
            self.git(&["cherry-pick", "--abort"])?;
        }
        self.git(&["checkout", "--quiet", &state.previous])?;
        self.git(&["branch", "--quiet", "-D", &state.plan.branch])?;
        self.drop_file(STATE_FILE);
        Ok(state)
    }

    /// odoo-lint's upgrade fixes for every addon still there, each in a
    /// `[FW]` commit, and what it leaves to fix by hand.
    fn port_fixes(&self, state: &PortState, tools: &Tools) -> Result<PortOutcome, String> {
        let plan = &state.plan;
        let picked = self
            .git(&["rev-list", "--count", &format!("{}..HEAD", state.base)])?
            .parse()
            .unwrap_or_default();
        let mut ported = Ported {
            branch: plan.branch.clone(),
            picked,
            commits: Vec::new(),
            findings: Vec::new(),
            warnings: Vec::new(),
        };
        let root = self.root()?;
        let addons = plan
            .addons
            .iter()
            .filter(|addon| root.join(addon).join("__manifest__.py").is_file());
        for addon in addons {
            if !plan.no_migrate {
                let fix = odx_core::Invocation::new(&tools.odl)
                    .current_dir(&root)
                    .args([
                        "upgrade-check",
                        "--target",
                        &plan.to,
                        "--fix",
                        "--unsafe-fixes",
                        addon,
                    ]);
                if let Err(e) = self.run(&fix) {
                    let warning = format!(
                        "{}: {e}; install odoo-lint for the automatic upgrade fixes",
                        tools.odl
                    );
                    if !ported.warnings.contains(&warning) {
                        ported.warnings.push(warning);
                    }
                }
                let subject = format!("[FW] {addon}: apply {} migration fixes", plan.to);
                if self.commit(addon, &subject, false)? {
                    ported.commits.push(subject);
                }
            }
            ported
                .findings
                .extend(self.findings(addon, &plan.to, tools));
        }
        self.drop_file(STATE_FILE);
        Ok(PortOutcome::Done(ported))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::system::OsRunner;
    use std::path::{Path, PathBuf};

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

    fn commit(dir: &Path, author: &str, message: &str) -> String {
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
        git(dir, &["rev-parse", "HEAD"])
    }

    /// An upstream with `my_addon` on 18.0 and, migrated, on 19.0; 18.0
    /// then gets a fix by Jane. Returns the clone and the fix.
    fn fixture() -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
        write(&up.join("my_addon/__manifest__.py"), "{}\n");
        write(&up.join("my_addon/data.xml"), "<odoo/>\n");
        write(&up.join("other/__manifest__.py"), "{}\n");
        commit(&up, "Joe", "[ADD] my_addon");
        git(&up, &["checkout", "--quiet", "-b", "19.0"]);
        write(
            &up.join("my_addon/__manifest__.py"),
            "{\"version\": \"19.0.1.0.0\"}\n",
        );
        commit(&up, "Joe", "[MIG] my_addon: Migration to 19.0");
        git(&up, &["checkout", "--quiet", "18.0"]);
        write(&up.join("my_addon/data.xml"), "<odoo><data/></odoo>\n");
        let fix = commit(&up, "Jane", "[FIX] my_addon: data");
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
        (tmp, work, fix)
    }

    fn plan(fix: &str) -> PortPlan {
        PortPlan {
            branch: "19.0-port-1".to_owned(),
            to: "19.0".to_owned(),
            remote: "origin".to_owned(),
            commits: vec![fix.to_owned()],
            addons: ["my_addon", "other", "gone"].map(str::to_owned).to_vec(),
            no_migrate: false,
            meta: BTreeMap::new(),
        }
    }

    fn tools() -> Tools {
        Tools {
            odl: "odx-test-no-such-program".to_owned(),
            pre_commit: None,
        }
    }

    /// The summary of a finished port.
    ///
    /// Excluded from coverage, like the other outcome helper: their panics
    /// only run when a test fails.
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn done(outcome: Result<PortOutcome, String>) -> Ported {
        match outcome {
            Ok(PortOutcome::Done(ported)) => ported,
            other => panic!("not done: {other:?}"),
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    fn halted(outcome: Result<PortOutcome, String>) -> String {
        match outcome {
            Ok(PortOutcome::Stopped(message)) => message,
            other => panic!("not stopped: {other:?}"),
        }
    }

    #[test]
    fn cherry_picks_keep_authors() {
        let (_tmp, work, fix) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        let ported = done(tree.port(plan(&fix), &tools()));
        assert_eq!((ported.branch.as_str(), ported.picked), ("19.0-port-1", 1));
        assert_eq!(ported.commits, Vec::<String>::new());
        assert_eq!(ported.warnings.len(), 1, "{:?}", ported.warnings);
        assert_eq!(
            git(
                &work,
                &["log", "-1", "--format=%an|%ad|%cn|%s", "--date=iso-strict"]
            ),
            "Jane|2024-03-01T10:00:00+01:00|Me|[FIX] my_addon: data"
        );
        let message = git(&work, &["log", "-1", "--format=%b"]);
        assert_eq!(message, format!("(cherry picked from commit {fix})"));
        assert_eq!(tree.port_state(), None);
        // Without the upgrade fixes, odoo-lint is not asked to fix.
        git(&work, &["checkout", "--quiet", "19.0"]);
        let quiet = PortPlan {
            branch: "19.0-port-2".to_owned(),
            no_migrate: true,
            ..plan(&fix)
        };
        assert_eq!(done(tree.port(quiet, &tools())).warnings.len(), 0);
    }

    #[test]
    fn refusals() {
        let (tmp, work, fix) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        let refused = |plan: PortPlan| tree.port(plan, &tools()).unwrap_err();
        write(&work.join("my_addon/data.xml"), "changed\n");
        assert!(refused(plan(&fix)).contains("uncommitted changes"));
        git(&work, &["checkout", "--quiet", "--", "."]);
        let mut other = plan(&fix);
        other.remote = "nowhere".to_owned();
        assert!(refused(other).starts_with("cannot fetch from nowhere: "));
        git(&work, &["branch", "19.0-port-1"]);
        assert!(refused(plan(&fix)).starts_with("cannot create branch 19.0-port-1: "));
        git(&work, &["branch", "-D", "19.0-port-1"]);
        let outside = Worktree::new(&OsRunner, tmp.path());
        assert_eq!(
            outside.port_resume(&tools()),
            Err("no port is in progress".to_owned())
        );
        assert_eq!(
            outside.port_abort(),
            Err("no port is in progress".to_owned())
        );
    }

    /// The fixture with 19.0 changing the same line, so the fix conflicts.
    fn conflicting() -> (tempfile::TempDir, PathBuf, String) {
        let (tmp, work, fix) = fixture();
        write(&work.join("my_addon/data.xml"), "<odoo><record/></odoo>\n");
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "record"]);
        git(&work, &["push", "--quiet", "origin", "HEAD:19.0"]);
        (tmp, work, fix)
    }

    #[test]
    fn conflicts_continue_or_abort() {
        let (_tmp, work, fix) = conflicting();
        let tree = Worktree::new(&OsRunner, &work);
        let message = halted(tree.port(plan(&fix), &tools()));
        assert!(message.contains("odx port --continue"), "{message}");
        assert!(
            tree.port(plan(&fix), &tools())
                .unwrap_err()
                .contains("in progress")
        );
        // Unresolved, it stops again.
        halted(tree.port_resume(&tools()));
        write(
            &work.join("my_addon/data.xml"),
            "<odoo><data/><record/></odoo>\n",
        );
        git(&work, &["add", "-A"]);
        let ported = done(tree.port_resume(&tools()));
        assert_eq!(ported.picked, 1);
        assert_eq!(git(&work, &["log", "-1", "--format=%an"]), "Jane");
        // Aborted, the branch is gone; resolved by hand, continuing goes on.
        git(&work, &["checkout", "--quiet", "19.0"]);
        git(&work, &["branch", "-D", "19.0-port-1"]);
        halted(tree.port(plan(&fix), &tools()));
        let state = tree.port_abort().unwrap();
        assert_eq!(state.previous, "19.0");
        assert_eq!(git(&work, &["branch", "--list", "19.0-port-*"]), "");
        halted(tree.port(plan(&fix), &tools()));
        git(&work, &["cherry-pick", "--skip"]);
        assert_eq!(done(tree.port_resume(&tools())).picked, 0);
        git(&work, &["checkout", "--quiet", "19.0"]);
        halted(tree.port(
            PortPlan {
                branch: "19.0-port-3".to_owned(),
                ..plan(&fix)
            },
            &tools(),
        ));
        git(&work, &["cherry-pick", "--abort"]);
        assert_eq!(tree.port_abort().unwrap().plan.branch, "19.0-port-3");
    }

    #[test]
    fn every_command_can_fail() {
        use crate::testing::Faulty;
        let calls = |run: &dyn Fn(&Faulty)| {
            let counter = Faulty::new(usize::MAX, false);
            run(&counter);
            counter.calls.into_inner()
        };
        let start = |runner: &Faulty| {
            let (_tmp, work, fix) = fixture();
            let _ = Worktree::new(runner, &work).port(plan(&fix), &tools());
        };
        let abort = |runner: &Faulty| {
            let (_tmp, work, fix) = conflicting();
            halted(Worktree::new(&OsRunner, &work).port(plan(&fix), &tools()));
            let _ = Worktree::new(runner, &work).port_abort();
        };
        let resume = |runner: &Faulty| {
            let (_tmp, work, fix) = conflicting();
            halted(Worktree::new(&OsRunner, &work).port(plan(&fix), &tools()));
            git(&work, &["cherry-pick", "--skip"]);
            let _ = Worktree::new(runner, &work).port_resume(&tools());
        };
        let branching = |runner: &Faulty| {
            let (_tmp, work, _fix) = fixture();
            let _ = Worktree::new(runner, &work).branch_from("origin", "19.0", "19.0-sweep");
        };
        let listing = |runner: &Faulty| {
            let (_tmp, work, _fix) = fixture();
            let entry = [("OCA/x#1".to_owned(), "r".to_owned())];
            let _ = Worktree::new(runner, &work).blacklist("my_addon", &entry);
        };
        for scenario in [
            &start as &dyn Fn(&Faulty),
            &abort,
            &resume,
            &listing,
            &branching,
        ] {
            scenario(&Faulty::new(0, true));
            for fail_at in 0..calls(scenario) {
                scenario(&Faulty::new(fail_at, false));
            }
        }
    }

    #[test]
    fn branches_from_the_target() {
        let (_tmp, work, _fix) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        tree.branch_from("origin", "19.0", "19.0-sweep").unwrap();
        assert_eq!(git(&work, &["branch", "--show-current"]), "19.0-sweep");
        let error = tree
            .branch_from("origin", "19.0", "19.0-sweep")
            .unwrap_err();
        assert!(
            error.starts_with("cannot create branch 19.0-sweep: "),
            "{error}"
        );
        write(&work.join("my_addon/data.xml"), "x\n");
        assert!(
            tree.branch_from("origin", "19.0", "b")
                .unwrap_err()
                .contains("uncommitted")
        );
        git(&work, &["checkout", "--quiet", "--", "."]);
        let (_conflict_tmp, other, other_fix) = conflicting();
        let busy = Worktree::new(&OsRunner, &other);
        halted(busy.port(plan(&other_fix), &tools()));
        assert!(
            busy.branch_from("origin", "19.0", "b")
                .unwrap_err()
                .contains("in progress")
        );
    }

    #[test]
    fn blacklists_are_written_and_committed() {
        let (_tmp, work, _fix) = fixture();
        let tree = Worktree::new(&OsRunner, &work);
        let entry = |key: &str, reason: &str| (key.to_owned(), reason.to_owned());
        let subject = tree
            .blacklist(
                "my_addon",
                &[
                    entry("OCA/x#1", "repo-wide"),
                    entry("orphaned_commits", "migration"),
                ],
            )
            .unwrap();
        assert_eq!(subject, "[FW] my_addon: blacklist 2 port suggestion(s)");
        let path = work.join(".oca/oca-port/blacklist/my_addon.json");
        tree.blacklist("my_addon", &[entry("OCA/x#2", "superseded: déjà fait")])
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"pull_requests\": {\n    \"OCA/x#1\": \"repo-wide\",\n    \"orphaned_commits\": \"migration\",\n    \"OCA/x#2\": \"superseded: déjà fait\"\n  }\n}\n"
        );
        assert_eq!(git(&work, &["status", "--porcelain"]), "");
        assert_eq!(
            git(&work, &["log", "-1", "--format=%s"]),
            "[FW] my_addon: blacklist 1 port suggestion(s)"
        );
        // No reason, no entry; files that cannot be written.
        assert!(
            tree.blacklist("my_addon", &[entry("OCA/x#3", " ")])
                .unwrap_err()
                .contains("needs a reason")
        );
        // A blacklist that is not one is left as it is.
        let corrupt = work.join(".oca/oca-port/blacklist/corrupt.json");
        std::fs::write(&corrupt, "{ not json").unwrap();
        assert!(
            tree.blacklist("corrupt", &[entry("OCA/x#3", "r")])
                .unwrap_err()
                .contains("corrupt.json")
        );
        assert_eq!(std::fs::read_to_string(&corrupt).unwrap(), "{ not json");
        std::fs::create_dir_all(work.join(".oca/oca-port/blacklist/dir.json")).unwrap();
        assert!(
            tree.blacklist("dir", &[entry("OCA/x#3", "r")])
                .unwrap_err()
                .contains("dir.json")
        );
        std::fs::remove_dir_all(work.join(".oca")).unwrap();
        std::fs::write(work.join(".oca"), "").unwrap();
        assert!(
            tree.blacklist("my_addon", &[entry("OCA/x#3", "r")])
                .unwrap_err()
                .contains("blacklist")
        );
        std::fs::remove_file(work.join(".oca")).unwrap();
        git(&work, &["checkout", "--quiet", "--", "."]);
        let outside = Worktree::new(&OsRunner, work.parent().unwrap());
        assert!(outside.blacklist("my_addon", &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn upgrade_fixes_per_addon() {
        use std::os::unix::fs::PermissionsExt as _;
        let (tmp, work, fix) = fixture();
        let odl = tmp.path().join("odl");
        std::fs::write(
            &odl,
            "#!/bin/sh\ncase \"$*\" in\n  *--fix*) echo '# fixed' >> my_addon/data.xml; exit 1;;\n  *) echo '{\"modules\":[{\"violations\":[{\"file_path\":\"my_addon/data.xml\",\"line\":1,\"code\":\"U1\",\"message\":\"m\"}]}]}';;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tools = Tools {
            odl: odl.display().to_string(),
            pre_commit: None,
        };
        let tree = Worktree::new(&OsRunner, &work);
        let ported = done(tree.port(plan(&fix), &tools));
        assert_eq!(
            ported.commits,
            ["[FW] my_addon: apply 19.0 migration fixes"]
        );
        assert_eq!(ported.findings.len(), 2);
        assert_eq!(ported.warnings, Vec::<String>::new());
    }
}
