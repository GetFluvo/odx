//! Bringing an open migration up to date.
//!
//! The commits the addon got on the source series since its history was
//! replayed onto the migration branch are replayed the same way (each with its author and date), bot
//! commits left out, administrative commits squashed, then odoo-lint's
//! fixes for the target series in a `[FW]` commit of their own.
//!
//! What the branch already has is found by author, author date and
//! subject, which a replay and a squash keep: the newest source commit
//! the branch has is where the update starts, unless `since` names it.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use odx_core::Invocation;

use crate::migrate::{Outcome, Plan, State, Step, Summary, Tools, Worktree, stopped};
use crate::squash::is_bot;

/// The command `--continue` and `--abort` belong to.
const MIGRATE: &str = "odx migrate";

/// A commit by its author e-mail, author date and subject.
type Key = (String, String, String);

/// The commits of `log` (`%H %ae %aI %s`, separated by `\x1f`).
fn commits(log: &str) -> Vec<(String, Key)> {
    log.lines()
        .map(|line| {
            let mut fields = line.splitn(4, '\u{1f}');
            let mut field = || fields.next().unwrap_or_default().to_owned();
            let sha = field();
            (sha, (field(), field(), field()))
        })
        .collect()
}

impl Worktree<'_> {
    /// Bring the migration branch checked out up to date with what the
    /// addon got on `plan.from` since; `since` names the last source commit
    /// the branch has when odx cannot tell.
    ///
    /// # Errors
    ///
    /// When a migration is in progress, the worktree has uncommitted
    /// changes, the source cannot be fetched, odx cannot tell where the
    /// branch stands and `since` is not given, or git fails.
    pub fn update(
        &self,
        plan: Plan,
        since: Option<&str>,
        tools: &Tools,
    ) -> Result<Outcome, String> {
        if self.state().is_some() {
            return Err(
                "a migration is in progress: run `odx migrate --continue` or `odx migrate --abort`"
                    .to_owned(),
            );
        }
        self.clean()?;
        let source = format!("refs/remotes/{}/{}", plan.remote, plan.from);
        self.fetch(
            &plan.remote,
            &[format!("+refs/heads/{}:{source}", plan.from)],
        )?;
        let format = "--format=%H%x1f%ae%x1f%aI%x1f%s";
        let owned = plan.addon.clone();
        let addon = owned.as_str();
        let range = match since {
            Some(since) => format!("{since}..{source}"),
            None => source,
        };
        let candidates = commits(&self.git(&[
            "log",
            "--reverse",
            "--no-merges",
            format,
            &range,
            "--",
            addon,
        ])?);
        let here: BTreeSet<Key> = commits(&self.git(&["log", format, "HEAD", "--", addon])?)
            .into_iter()
            .map(|(_, key)| key)
            .collect();
        let start = match since {
            Some(_) => 0,
            None => candidates
                .iter()
                .rposition(|(_, key)| here.contains(key))
                .map(|at| at + 1)
                .ok_or_else(|| {
                    if candidates.is_empty() {
                        format!(
                            "{addon} has no history on {}/{}: name the series the migration comes from with --from",
                            plan.remote, plan.from
                        )
                    } else {
                        format!(
                            "no commit of {addon} on {} is on this branch yet; name the last one it has with --since <commit>",
                            plan.from
                        )
                    }
                })?,
        };
        let new: Vec<&String> = candidates
            .iter()
            .skip(start)
            .filter(|(_, key)| !here.contains(key))
            .filter(|(_, (email, _, subject))| !is_bot(email, subject, &plan.squash))
            .map(|(sha, _)| sha)
            .collect();
        let base = self.git(&["rev-parse", "HEAD"])?;
        let state = State {
            previous: self.checked_out()?,
            plan,
            step: Step::History,
            base,
            update: true,
        };
        self.save(&state)?;
        let began = Instant::now();
        let mut patches = Vec::new();
        for sha in &new {
            let patch = self
                .run(&self.command().args([
                    "format-patch",
                    "--keep-subject",
                    "--stdout",
                    "-1",
                    sha.as_str(),
                    "--",
                    addon,
                ]))
                .map(|output| output.stdout)
                .unwrap_or_default();
            patches.extend(patch);
        }
        if !patches.is_empty() {
            let applied = self.output(
                &self
                    .command()
                    .args(["am", "-3", "--keep", "--quiet"])
                    .stdin(patches),
            );
            if let Err(e) = applied {
                return Ok(Outcome::Stopped(stopped(&e, MIGRATE)));
            }
        }
        self.update_fixes(state, tools, began.elapsed())
    }

    /// After the update's history: the squash, then pre-commit and
    /// odoo-lint's fixes for the target series in a `[FW]` commit.
    pub(crate) fn update_fixes(
        &self,
        mut state: State,
        tools: &Tools,
        history_took: Duration,
    ) -> Result<Outcome, String> {
        state.step = Step::Fixes;
        self.save(&state)?;
        let plan = &state.plan;
        let addon = plan.addon.as_str();
        let tools = &Tools {
            odl: tools.odl.clone(),
            pre_commit: tools.pre_commit.clone().filter(|_| !plan.no_pre_commit),
        };
        let began = Instant::now();
        let (squashed, conflict) = if plan.no_squash {
            (0, None)
        } else {
            self.squash_administrative(&state.base, &plan.squash)
        };
        let squash_took = began.elapsed();
        let history = self
            .git(&["rev-list", "--count", &format!("{}..HEAD", state.base)])?
            .parse()
            .unwrap_or_default();
        let mut summary = Summary {
            branch: plan.branch(),
            source: format!("{}/{}", plan.remote, plan.from),
            history,
            squashed,
            commits: Vec::new(),
            findings: Vec::new(),
            warnings: conflict.into_iter().collect(),
            timings: Vec::new(),
            update: true,
        };
        let began = Instant::now();
        if history > 0 {
            let fix = Invocation::new(&tools.odl).current_dir(self.root()?).args([
                "upgrade-check",
                "--target",
                &plan.to,
                "--fix",
                "--unsafe-fixes",
                addon,
            ]);
            if let Err(e) = self.run(&fix) {
                summary.warnings.push(format!(
                    "{}: {e}; install odoo-lint for the automatic upgrade fixes",
                    tools.odl
                ));
            }
        }
        let lint_took = began.elapsed();
        let began = Instant::now();
        if history > 0 {
            self.pre_commit(addon, tools, &mut summary.warnings)?;
        }
        let pre_commit_took = began.elapsed();
        let subject = format!(
            "[FW] {addon}: {} fixes for the changes from {}",
            plan.to, plan.from
        );
        if self.commit(addon, &subject, false)? {
            summary.commits.push(subject);
        }
        summary.findings = self.findings(addon, &plan.to, tools);
        summary.timings = Self::timings(&[
            ("history", history_took),
            ("squash", squash_took),
            ("pre-commit", pre_commit_took),
            ("odoo-lint", lint_took),
        ]);
        self.forget();
        Ok(Outcome::Done(summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::squash::Rules;
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

    /// A commit on the checked-out branch of `dir` by `author <email>`.
    fn commit(
        dir: &Path,
        author: &str,
        email: &str,
        subject: &str,
        files: &[(&str, &str)],
    ) -> String {
        for (file, text) in files {
            let path = dir.join("my_addon").join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(
            dir,
            &[
                "commit",
                "--quiet",
                "--author",
                &format!("{author} <{email}>"),
                "-m",
                subject,
            ],
        );
        git(dir, &["rev-parse", "HEAD"])
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

    fn tools() -> Tools {
        Tools {
            odl: "odx-test-no-such-program".to_owned(),
            pre_commit: None,
        }
    }

    /// The summary of a finished run.
    ///
    /// Excluded from coverage: its panic only runs when a test fails.
    #[cfg_attr(coverage_nightly, coverage(off))]
    fn done(outcome: Result<Outcome, String>) -> Summary {
        match outcome {
            Ok(Outcome::Done(summary)) => summary,
            other => panic!("not done: {other:?}"),
        }
    }

    #[cfg_attr(coverage_nightly, coverage(off))]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "called with the outcome of a run"
    )]
    fn halted(outcome: Result<Outcome, String>) {
        assert!(matches!(outcome, Ok(Outcome::Stopped(_))), "{outcome:?}");
    }

    /// An upstream with `my_addon` on 18.0 and an empty 19.0, and a clone
    /// on the migration branch `odx migrate addon` made.
    fn migrated() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
        commit(
            &up,
            "Jane",
            "jane@example.com",
            "[ADD] my_addon",
            &[("__manifest__.py", "{}\n"), ("a.py", "a\n")],
        );
        git(&up, &["checkout", "--quiet", "--orphan", "19.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        std::fs::write(up.join("README.md"), "19.0\n").unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "[INIT] 19.0"]);
        git(&up, &["checkout", "--quiet", "18.0"]);
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
        done(Worktree::new(&OsRunner, &work).start(plan(), &tools()));
        (tmp, up, work)
    }

    #[test]
    fn new_source_commits_come_in() {
        let (_tmp, up, work) = migrated();
        commit(
            &up,
            "Jane",
            "jane@example.com",
            "[FIX] my_addon: b",
            &[("b.py", "b\n")],
        );
        commit(
            &up,
            "OCA-git-bot",
            "oca-git-bot@odoo-community.org",
            "[BOT] post-merge updates",
            &[("README.rst", "r\n")],
        );
        commit(
            &up,
            "Nl",
            "nl@example.com",
            "Translated using Weblate (Dutch)",
            &[("i18n/nl.po", "x\n")],
        );
        let tree = Worktree::new(&OsRunner, &work);
        let summary = done(tree.update(plan(), None, &tools()));
        assert_eq!((summary.history, summary.squashed), (2, 0));
        assert_eq!(
            git(&work, &["log", "-2", "--format=%an|%s"]),
            "Nl|Translated using Weblate (Dutch)\nJane|[FIX] my_addon: b"
        );
        assert!(!work.join("my_addon/README.rst").exists());
        assert_eq!(tree.state(), None);
        // Up to date: nothing comes in.
        assert_eq!(done(tree.update(plan(), None, &tools())).history, 0);
    }

    #[test]
    fn since_when_the_branch_does_not_say() {
        let (_tmp, up, work) = migrated();
        git(
            &work,
            &["checkout", "--quiet", "-b", "19.0-mig-other", "origin/19.0"],
        );
        let tree = Worktree::new(&OsRunner, &work);
        let error = tree.update(plan(), None, &tools()).unwrap_err();
        assert!(error.contains("--since"), "{error}");
        let elsewhere = Plan {
            addon: "other".to_owned(),
            ..plan()
        };
        let error = tree.update(elsewhere, None, &tools()).unwrap_err();
        assert!(
            error.contains("other has no history on origin/18.0"),
            "{error}"
        );
        let first = git(&up, &["rev-list", "--max-parents=0", "18.0"]);
        commit(
            &up,
            "Jane",
            "jane@example.com",
            "[FIX] my_addon: c",
            &[("c.py", "c\n")],
        );
        // The addon is not on this branch: the fix touches a file it has not.
        let unsquashed = Plan {
            no_squash: true,
            ..plan()
        };
        let summary = done(tree.update(unsquashed, Some(&first), &tools()));
        assert_eq!(summary.history, 1);
        // Refusals.
        std::fs::write(work.join("README.md"), "changed\n").unwrap();
        assert!(
            tree.update(plan(), None, &tools())
                .unwrap_err()
                .contains("uncommitted")
        );
    }

    #[test]
    fn conflicts_continue_or_abort() {
        let (_tmp, up, work) = migrated();
        // The branch changes a.py; 18.0 changes it too.
        std::fs::write(work.join("my_addon/a.py"), "a, 19.0\n").unwrap();
        git(&work, &["commit", "--quiet", "-am", "[IMP] my_addon: 19.0"]);
        commit(
            &up,
            "Jane",
            "jane@example.com",
            "[FIX] my_addon: a",
            &[("a.py", "a, fixed\n")],
        );
        let tree = Worktree::new(&OsRunner, &work);
        let before = git(&work, &["rev-parse", "HEAD"]);
        halted(tree.update(plan(), None, &tools()));
        assert!(
            tree.update(plan(), None, &tools())
                .unwrap_err()
                .contains("in progress")
        );
        let state = tree.abort().unwrap();
        assert!(state.update);
        assert_eq!(git(&work, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            git(&work, &["branch", "--show-current"]),
            "19.0-mig-my_addon"
        );
        // Resolved by hand, continuing finishes.
        halted(tree.update(plan(), None, &tools()));
        std::fs::write(work.join("my_addon/a.py"), "a, 19.0, fixed\n").unwrap();
        git(&work, &["add", "-A"]);
        let summary = done(tree.resume(&tools()));
        assert_eq!(summary.history, 1);
        assert_eq!(
            git(&work, &["log", "-1", "--format=%an|%s"]),
            "Jane|[FIX] my_addon: a"
        );
    }

    #[test]
    fn every_command_can_fail() {
        use crate::testing::Faulty;
        let calls = |run: &dyn Fn(&Faulty)| {
            let counter = Faulty::new(usize::MAX, false);
            run(&counter);
            counter.calls.into_inner()
        };
        let update = |runner: &Faulty| {
            let (_tmp, up, work) = migrated();
            commit(
                &up,
                "Jane",
                "jane@example.com",
                "[FIX] my_addon: b",
                &[("b.py", "b\n")],
            );
            std::fs::write(work.join(".pre-commit-config.yaml"), "repos: []\n").unwrap();
            let tools = Tools {
                pre_commit: Some("true".to_owned()),
                ..tools()
            };
            let _ = Worktree::new(runner, &work).update(plan(), None, &tools);
        };
        update(&Faulty::new(0, true));
        for fail_at in 0..calls(&update) {
            update(&Faulty::new(fail_at, false));
        }
    }
}
