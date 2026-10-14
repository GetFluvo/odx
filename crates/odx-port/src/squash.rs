//! Squashing the administrative commits of a replayed history.
//!
//! As OCA asks of a migration: bot commits (post-merge updates, README and
//! `.pot` regeneration) go into the real commit they follow, and Weblate
//! commits into the first earlier one of the same language by the same
//! translator.
//!
//! The squash is a `git rebase -i` with a todo list odx writes. When it
//! conflicts, odx squashes one administrative commit at a time and keeps
//! those that apply cleanly; the others stay as they are.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::migrate::Worktree;

/// What makes a commit administrative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// Author e-mails of bots.
    pub authors: Vec<String>,
    /// Text in a subject that marks a bot commit.
    pub subjects: Vec<String>,
}

impl Default for Rules {
    /// The OCA bots and their commits.
    fn default() -> Self {
        let owned = |list: &[&str]| list.iter().map(|item| (*item).to_owned()).collect();
        Self {
            authors: owned(&[
                "transbot@odoo-community.org",
                "noreply@weblate.org",
                "oca-git-bot@odoo-community.org",
                "oca+oca-travis@odoo-community.org",
                "oca-ci@odoo-community.org",
                "shopinvader-git-bot@shopinvader.com",
            ]),
            subjects: owned(&[
                "Update translation files",
                "[BOT] post-merge updates",
                "[UPD] README.rst",
            ]),
        }
    }
}

/// A commit as the squash sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Commit {
    sha: String,
    email: String,
    subject: String,
}

/// What a commit is to the squash.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// A change of its own.
    Real,
    /// A Weblate commit for a language, by a translator.
    Translation { language: String, email: String },
    /// A bot's commit.
    Bot,
}

/// A commit kept, with the ones squashed into it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kept {
    sha: String,
    kind: Kind,
    fixups: Vec<String>,
}

/// The language of a Weblate commit: `Translated using Weblate (Dutch)`.
fn translation(subject: &str) -> Option<String> {
    let weblate = [
        "Translated using Weblate",
        "Added translation using Weblate",
    ];
    if !weblate.iter().any(|marker| subject.contains(marker)) {
        return None;
    }
    let (_, rest) = subject.rsplit_once('(')?;
    let (language, _) = rest.split_once(')')?;
    Some(language.trim().to_owned())
}

fn kind(commit: &Commit, rules: &Rules) -> Kind {
    if let Some(language) = translation(&commit.subject) {
        return Kind::Translation {
            language,
            email: commit.email.clone(),
        };
    }
    let bot = rules.authors.contains(&commit.email)
        || rules
            .subjects
            .iter()
            .any(|marker| commit.subject.contains(marker));
    if bot { Kind::Bot } else { Kind::Real }
}

/// The commits to keep, oldest first, with what goes into each: a bot
/// commit into the nearest earlier real commit, a Weblate commit into the
/// nearest earlier kept one of its language and translator; only the
/// commits `squash` allows.
fn plan(commits: &[Commit], rules: &Rules, squash: &dyn Fn(&str) -> bool) -> Vec<Kept> {
    let mut kept: Vec<Kept> = Vec::new();
    for commit in commits {
        let kind = kind(commit, rules);
        let into = match &kind {
            _ if !squash(&commit.sha) => None,
            Kind::Translation { .. } => kept.iter_mut().rev().find(|earlier| earlier.kind == kind),
            Kind::Bot => kept
                .iter_mut()
                .rev()
                .find(|earlier| earlier.kind == Kind::Real),
            Kind::Real => None,
        };
        match into {
            Some(earlier) => earlier.fixups.push(commit.sha.clone()),
            None => kept.push(Kept {
                sha: commit.sha.clone(),
                kind,
                fixups: Vec::new(),
            }),
        }
    }
    kept
}

/// The rebase todo list of `kept`.
fn todo(kept: &[Kept]) -> String {
    let mut list = String::new();
    for commit in kept {
        let _ = writeln!(list, "pick {}", commit.sha);
        for fixup in &commit.fixups {
            let _ = writeln!(list, "fixup {fixup}");
        }
    }
    list
}

impl Worktree<'_> {
    /// Squash the administrative commits of `base..HEAD` by `rules`: the
    /// number squashed, and a warning when a conflict kept them.
    pub(crate) fn squash_administrative(
        &self,
        base: &str,
        rules: &Rules,
    ) -> (usize, Option<String>) {
        let range = format!("{base}..HEAD");
        let log = self
            .git(&["log", "--reverse", "--format=%H%x1f%ae%x1f%s", &range])
            .unwrap_or_default();
        let commits: Vec<Commit> = log
            .lines()
            .map(|line| {
                let mut fields = line.splitn(3, '\u{1f}');
                let mut field = || fields.next().unwrap_or_default().to_owned();
                Commit {
                    sha: field(),
                    email: field(),
                    subject: field(),
                }
            })
            .collect();
        let everything = plan(&commits, rules, &|_| true);
        let candidates: Vec<String> = everything
            .iter()
            .flat_map(|commit| commit.fixups.iter().cloned())
            .collect();
        if candidates.is_empty() || self.rebase_onto(base, &todo(&everything)) {
            return (candidates.len(), None);
        }
        // One at a time, each attempt from the history as replayed.
        let head = self.git(&["rev-parse", "HEAD"]).unwrap_or_default();
        let mut taken: Vec<String> = Vec::new();
        for candidate in &candidates {
            taken.push(candidate.clone());
            let kept = plan(&commits, rules, &|sha| taken.iter().any(|t| t == sha));
            if !self.rebase_onto(base, &todo(&kept)) {
                taken.pop();
            }
            let _ = self.git(&["reset", "--quiet", "--hard", &head]);
        }
        if !taken.is_empty() {
            let kept = plan(&commits, rules, &|sha| taken.iter().any(|t| t == sha));
            let _ = self.rebase_onto(base, &todo(&kept));
        }
        let left = candidates.len() - taken.len();
        (
            taken.len(),
            Some(format!(
                "{left} administrative commit(s) conflict when squashed and are kept as they are"
            )),
        )
    }

    /// `git rebase -i base` with `todo` as the list; aborted on failure.
    fn rebase_onto(&self, base: &str, todo: &str) -> bool {
        let Ok(dir) = self.git_dir() else {
            return false;
        };
        let file = dir.join("odx-squash-todo");
        if std::fs::write(&file, todo).is_err() {
            return false;
        }
        let path = file.to_string_lossy().replace('\\', "/");
        let editor = format!("sequence.editor=cp '{path}'");
        let done = self
            .git(&[
                "-c",
                "core.editor=true",
                "-c",
                &editor,
                "rebase",
                "-i",
                "--quiet",
                base,
            ])
            .is_ok();
        if !done {
            let _ = self.git(&["rebase", "--abort"]);
        }
        let _ = std::fs::remove_file(&file);
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, email: &str, subject: &str) -> Commit {
        Commit {
            sha: sha.to_owned(),
            email: email.to_owned(),
            subject: subject.to_owned(),
        }
    }

    fn shape(kept: &[Kept]) -> Vec<String> {
        kept.iter()
            .map(|commit| format!("{}<{}", commit.sha, commit.fixups.join(",")))
            .collect()
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=Odx", "-c", "user.email=odx@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A repository with a real commit and a bot commit on top of `base`.
    fn history() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "--quiet", "--initial-branch=main"]);
        git(dir, &["commit", "--quiet", "--allow-empty", "-m", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join("a.py"), "a\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", "[ADD] a"]);
        std::fs::write(dir.join("README.rst"), "r\n").unwrap();
        git(dir, &["add", "-A"]);
        git(
            dir,
            &["commit", "--quiet", "-m", "[BOT] post-merge updates"],
        );
        (tmp, base)
    }

    #[test]
    fn failures_keep_the_history() {
        let rules = Rules::default();
        // Whatever git command fails, nothing breaks.
        for fail_at in 0..8 {
            let (tmp, base) = history();
            let faulty = crate::testing::Faulty::new(fail_at, false);
            let _ = Worktree::new(&faulty, tmp.path()).squash_administrative(&base, &rules);
        }
        // A todo list that cannot be written.
        let (tmp, base) = history();
        std::fs::create_dir_all(tmp.path().join(".git/odx-squash-todo")).unwrap();
        let runner = odx_core::system::OsRunner;
        let (squashed, warning) =
            Worktree::new(&runner, tmp.path()).squash_administrative(&base, &rules);
        assert_eq!(squashed, 0);
        assert!(warning.is_some());
        assert_eq!(
            git(
                tmp.path(),
                &["rev-list", "--count", &format!("{base}..HEAD")]
            ),
            "2"
        );
    }

    #[test]
    fn plans() {
        let rules = Rules::default();
        let commits = [
            commit("bot0", "oca-ci@odoo-community.org", "[UPD] Update x.pot"),
            commit("add", "jane@example.com", "[ADD] x"),
            commit("readme", "someone@example.com", "[UPD] README.rst"),
            commit("nl1", "nl@example.com", "Translated using Weblate (Dutch)"),
            commit("fix", "jane@example.com", "[FIX] x"),
            commit("nl2", "nl@example.com", "Translated using Weblate (Dutch)"),
            commit(
                "nl3",
                "other@example.com",
                "Added translation using Weblate (Dutch)",
            ),
            commit("pot", "oca-ci@odoo-community.org", "[UPD] Update x.pot"),
            commit("odd", "x@example.com", "Translated using Weblate"),
        ];
        assert_eq!(
            shape(&plan(&commits, &rules, &|_| true)),
            ["bot0<", "add<readme", "nl1<nl2", "fix<pot", "nl3<", "odd<"]
        );
        assert_eq!(
            shape(&plan(&commits, &rules, &|sha| sha != "nl2")),
            [
                "bot0<",
                "add<readme",
                "nl1<",
                "fix<pot",
                "nl2<",
                "nl3<",
                "odd<"
            ]
        );
        assert_eq!(
            todo(&plan(&commits[1..3], &rules, &|_| true)),
            "pick add\nfixup readme\n"
        );
        assert_eq!(translation("Translated using Weblate (Dutch"), None);
    }
}
