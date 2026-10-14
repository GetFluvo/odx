//! The analysis pipeline shared by `odx suggest` and `odx port`: the
//! commits an addon has on a source branch and not on the target, grouped
//! by the pull request that merged them and classified.

use std::collections::BTreeSet;

use regex::Regex;

use crate::blacklist::{Blacklist, ORPHANED};
use crate::git::{Commit, Repo};

/// What a group of commits is, for porting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// A change worth porting.
    Portable,
    /// A migration or version-bump commit: cannot be ported.
    Migration,
    /// A change of many addons, or of generated files only.
    RepoWide,
    /// Translation files only; Weblate syncs them.
    Translations,
    /// Reverted by a later change on the source, or the revert itself: the
    /// pair cancels out.
    Reverted,
    /// In the target branch's blacklist.
    Blacklisted,
}

impl Class {
    /// How the class is written.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Migration => "migration commit",
            Self::RepoWide => "repo-wide change",
            Self::Translations => "translations",
            Self::Reverted => "reverted",
            Self::Blacklisted => "blacklisted",
        }
    }
}

/// How commits are classified (`[tool.odx.port]`).
#[derive(Debug, Clone)]
pub struct Settings {
    migration: Vec<Regex>,
    repo_wide_threshold: usize,
}

impl Settings {
    /// From `migration_commit_patterns` (default `^\[MIG\]` and
    /// `^\[ADD\].*\bmigration\b`, case-insensitive) and
    /// `repo_wide_threshold` (default 5 addons).
    ///
    /// # Errors
    ///
    /// When a pattern is no regular expression.
    pub fn new(
        patterns: Option<&[String]>,
        repo_wide_threshold: Option<u16>,
    ) -> Result<Self, String> {
        let defaults = [
            r"^\[MIG\]".to_owned(),
            r"^\[ADD\].*\bmigration\b".to_owned(),
        ];
        let migration = patterns
            .unwrap_or(&defaults)
            .iter()
            .map(|pattern| {
                Regex::new(&format!("(?i){pattern}"))
                    .map_err(|e| format!("migration_commit_patterns: {pattern:?}: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            migration,
            repo_wide_threshold: repo_wide_threshold.map_or(5, usize::from),
        })
    }
}

/// The commits of one pull request (or the orphaned commits) an addon has
/// on the source branch and not on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The pull request's number; `None` for orphaned commits.
    pub pr: Option<u64>,
    /// The blacklist key: `<owner>/<repo>#<n>` or `orphaned_commits`.
    pub key: String,
    /// The subject of its oldest commit.
    pub title: String,
    /// Its commits, newest first.
    pub commits: Vec<Commit>,
    /// What it is.
    pub class: Class,
    /// The blacklist's reason, when blacklisted.
    pub reason: Option<String>,
    /// The patch-ids of its commits (relative to the addon), in order.
    pub patch_ids: Vec<String>,
    /// The files its commits change in the addon, relative to it, sorted.
    pub files: Vec<String>,
}

/// What to compare.
#[derive(Debug, Clone)]
pub struct Query<'a> {
    /// The source branch (a ref in the repository).
    pub source: &'a str,
    /// The target branch and any other branches whose changes count as
    /// already present.
    pub targets: &'a [String],
    /// The addon's directory on the source branch.
    pub source_dir: &'a str,
    /// The addon's directory on the target branches.
    pub target_dir: &'a str,
    /// `<owner>/<repo>`, for blacklist keys.
    pub repository: &'a str,
    /// The target branch's blacklist for the addon.
    pub blacklist: &'a Blacklist,
}

/// The number right after `marker` in `text`.
fn number_after(text: &str, marker: &str) -> Option<u64> {
    let (_, rest) = text.split_once(marker)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The pull request named in a merge message or a squashed commit's
/// subject: `Merge PR #12` (ocabot), `Merge pull request #12` (GitHub),
/// `See merge request group/x!12` (GitLab), `Fix (#12)` (squash).
fn pull_request(merge: Option<&str>, subject: &str) -> Option<u64> {
    let merged = merge.and_then(|message| {
        number_after(message, "Merge PR #")
            .or_else(|| number_after(message, "Merge pull request #"))
            .or_else(|| {
                message
                    .split_once("See merge request ")
                    .and_then(|(_, reference)| number_after(reference, "!"))
            })
    });
    merged.or_else(|| {
        subject
            .trim_end()
            .strip_suffix(')')
            .and_then(|rest| number_after(rest.rsplit_once("(#")?.1, ""))
    })
}

/// The files under `dir/` of a commit's files, relative to it.
fn within<'f>(files: &'f [String], dir: &str) -> Vec<&'f str> {
    let prefix = format!("{dir}/");
    files
        .iter()
        .filter_map(|file| file.strip_prefix(&prefix))
        .collect()
}

/// Generated by the OCA bot: the README and its fragments' rendering.
fn generated(file: &str) -> bool {
    file == "README.rst" || file == "static/description/index.html"
}

fn translation(file: &str) -> bool {
    file.starts_with("i18n/")
        && std::path::Path::new(file)
            .extension()
            .is_some_and(|ext| ext == "po" || ext == "pot")
}

/// The class of a group whose commits change `files` (all of them, per
/// commit).
fn classify(commits: &[Commit], files: &[Vec<String>], dir: &str, settings: &Settings) -> Class {
    if commits.iter().any(|commit| {
        settings
            .migration
            .iter()
            .any(|re| re.is_match(&commit.subject))
    }) {
        return Class::Migration;
    }
    let mine: Vec<Vec<&str>> = files.iter().map(|files| within(files, dir)).collect();
    if mine.iter().flatten().all(|file| translation(file)) {
        return Class::Translations;
    }
    let wide = files.iter().any(|files| {
        let tops: BTreeSet<&str> = files
            .iter()
            .filter_map(|file| file.split_once('/').map(|(top, _)| top))
            .collect();
        tops.len() > settings.repo_wide_threshold
    });
    if wide || mine.iter().flatten().all(|file| generated(file)) {
        Class::RepoWide
    } else {
        Class::Portable
    }
}

/// The commits the addon has on `query.source` that no target has (by
/// patch-id, paths relative to the addon), grouped by pull request in
/// order of their newest commit, and classified.
///
/// # Errors
///
/// When git cannot read a branch.
pub fn analyse(
    repo: &Repo<'_>,
    query: &Query<'_>,
    settings: &Settings,
) -> Result<Vec<Group>, String> {
    let mut present: BTreeSet<String> = BTreeSet::new();
    for target in query.targets {
        present.extend(repo.patch_ids(target, query.target_dir)?.into_values());
    }
    let ids = repo.patch_ids(query.source, query.source_dir)?;
    // The source was read above, so these cannot fail.
    let missing: Vec<Commit> = repo
        .commits(query.source, query.source_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|commit| ids.get(&commit.sha).is_none_or(|id| !present.contains(id)))
        .collect();
    let line = repo.first_parents(query.source).unwrap_or_default();
    let mut groups: Vec<Group> = Vec::new();
    let mut tips: std::collections::BTreeMap<Option<u64>, String> =
        std::collections::BTreeMap::new();
    for commit in missing {
        let merge = repo.merged_by(&commit.sha, query.source, &line);
        let pr = pull_request(
            merge.as_ref().map(|merge| merge.message.as_str()),
            &commit.subject,
        );
        // A pull request whose last commit is a migration is a migration,
        // even when that commit already reached the target.
        if let Some(tip) = merge.and_then(|merge| merge.tip) {
            tips.entry(pr).or_insert(tip);
        }
        let key = pr.map_or_else(
            || ORPHANED.to_owned(),
            |n| format!("{}#{n}", query.repository),
        );
        match groups.iter_mut().find(|group| group.key == key) {
            Some(group) => {
                group.title.clone_from(&commit.subject);
                group.commits.push(commit);
            }
            None => groups.push(Group {
                pr,
                key,
                title: commit.subject.clone(),
                commits: vec![commit],
                class: Class::Portable,
                reason: None,
                patch_ids: Vec::new(),
                files: Vec::new(),
            }),
        }
    }
    for group in &mut groups {
        let files: Vec<Vec<String>> = group
            .commits
            .iter()
            .map(|commit| repo.files(&commit.sha).unwrap_or_default())
            .collect();
        let mine: BTreeSet<&str> = files
            .iter()
            .flat_map(|files| within(files, query.source_dir))
            .collect();
        group.files = mine.into_iter().map(str::to_owned).collect();
        group.patch_ids = group
            .commits
            .iter()
            .filter_map(|commit| ids.get(&commit.sha).cloned())
            .collect();
        group.reason = query.blacklist.reason(&group.key).map(str::to_owned);
        let tip_migrates = tips
            .get(&group.pr)
            .is_some_and(|tip| settings.migration.iter().any(|re| re.is_match(tip)));
        group.class = if group.reason.is_some() {
            Class::Blacklisted
        } else if tip_migrates {
            Class::Migration
        } else {
            classify(&group.commits, &files, query.source_dir, settings)
        };
    }
    reverted(&mut groups);
    Ok(groups)
}

/// Mark a change and the later one that reverts it (`Revert "<subject>"`)
/// as reverted, unless blacklisted.
fn reverted(groups: &mut [Group]) {
    let reverts: Vec<(usize, String)> = groups
        .iter()
        .enumerate()
        .flat_map(|(at, group)| {
            group.commits.iter().filter_map(move |commit| {
                let subject = commit
                    .subject
                    .strip_prefix("Revert \"")?
                    .strip_suffix('"')?;
                Some((at, subject.to_owned()))
            })
        })
        .collect();
    for (at, subject) in reverts {
        // A revert of something the target has may still need porting.
        let Some(original) = groups
            .iter()
            .position(|group| group.commits.iter().any(|commit| commit.subject == subject))
        else {
            continue;
        };
        for index in [at, original] {
            if let Some(group) = groups
                .get_mut(index)
                .filter(|group| group.class != Class::Blacklisted)
            {
                group.class = Class::Reverted;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::system::OsRunner;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Jane")
            .env("GIT_AUTHOR_EMAIL", "jane@example.com")
            .env("GIT_COMMITTER_NAME", "odx")
            .env("GIT_COMMITTER_EMAIL", "odx@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn change(dir: &Path, files: &[(&str, &str)], message: &str) {
        for (file, content) in files {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", message]);
    }

    /// A change merged as pull request `n` through a merge commit.
    fn merged(dir: &Path, n: u32, files: &[(&str, &str)], message: &str) {
        git(dir, &["checkout", "--quiet", "-b", &format!("pr{n}")]);
        change(dir, files, message);
        git(dir, &["checkout", "--quiet", "16.0"]);
        git(
            dir,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                &format!("pr{n}"),
                "-m",
                &format!("Merge PR #{n} into 16.0\n\nSigned-off-by x"),
            ],
        );
    }

    /// Pull requests on 16.0 that 17.0 (where `pos_x` is `pos_y`) has or
    /// has not, a blacklist on 17.0.
    fn fixture(dir: &Path) {
        git(dir, &["init", "--quiet", "--initial-branch=16.0"]);
        change(
            dir,
            &[("pos_x/__manifest__.py", "{}"), ("pos_x/models.py", "a\n")],
            "[MIG] pos_x: Migration to 16.0",
        );
        merged(
            dir,
            1,
            &[("pos_x/models.py", "a\nfix a\n")],
            "[FIX] pos_x: a",
        );
        merged(dir, 2, &[("pos_x/views.xml", "b\n")], "[FIX] pos_x: b");
        change(dir, &[("pos_x/wizard.py", "c\n")], "[IMP] pos_x: c (#3)");
        merged(
            dir,
            4,
            &[("pos_x/i18n/nl.po", "x"), ("pos_x/i18n/pos_x.pot", "y")],
            "[UPD] translations",
        );
        let wide: Vec<(String, &str)> = (0..7).map(|i| (format!("addon_{i}/x.py"), "w")).collect();
        let mut files: Vec<(&str, &str)> = wide.iter().map(|(f, c)| (f.as_str(), *c)).collect();
        files.push(("pos_x/tests/x.py", "w"));
        merged(dir, 5, &files, "[IMP] all: tests");
        merged(
            dir,
            6,
            &[
                ("pos_x/README.rst", "readme"),
                ("pos_x/static/description/index.html", "h"),
            ],
            "[BOT] readme",
        );
        merged(dir, 7, &[("pos_x/report.py", "r\n")], "[ADD] pos_x: report");
        merged(dir, 8, &[("pos_x/cache.py", "c\n")], "[IMP] pos_x: cache");
        merged(
            dir,
            9,
            &[("pos_x/cache.py", "")],
            "Revert \"[IMP] pos_x: cache\"",
        );
        merged(
            dir,
            10,
            &[("pos_x/cache.py", "c, again\n")],
            "Revert \"[IMP] pos_x: gone\"",
        );
        // A migration pull request whose [MIG] commit is not in pos_x.
        git(dir, &["checkout", "--quiet", "-b", "pr11"]);
        change(
            dir,
            &[("pos_x/lint.py", "l\n")],
            "[IMP] pos_x: pre-commit auto fixes",
        );
        change(
            dir,
            &[("pos_x/lint.py", "l\nm\n")],
            "[IMP] pos_x: more fixes",
        );
        change(
            dir,
            &[("other/y.py", "y\n")],
            "[MIG] other: Migration to 16.0",
        );
        git(dir, &["checkout", "--quiet", "16.0"]);
        git(
            dir,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                "pr11",
                "-m",
                "Merge PR #11 into 16.0",
            ],
        );
        // 17.0 has the migration and fix a (renamed to pos_y), and a blacklist.
        git(dir, &["checkout", "--quiet", "--orphan", "17.0"]);
        git(dir, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            dir,
            &[
                ("pos_y/__manifest__.py", "{'version': '17.0'}"),
                ("pos_y/models.py", "a\n"),
            ],
            "[MIG] pos_y",
        );
        change(dir, &[("pos_y/models.py", "a\nfix a\n")], "[FW] fix a");
        change(
            dir,
            &[(
                ".oca/oca-port/blacklist/pos_y.json",
                "{\"pull_requests\": {\"OCA/pos#7\": \"not wanted on 17.0\"}}",
            )],
            "[BL]",
        );
    }

    #[test]
    fn reverts_cancel_out_unless_blacklisted() {
        let group = |key: &str, subject: &str, class: Class| Group {
            pr: None,
            key: key.to_owned(),
            title: subject.to_owned(),
            commits: vec![Commit {
                sha: key.to_owned(),
                subject: subject.to_owned(),
                author: String::new(),
                date: String::new(),
            }],
            class,
            reason: None,
            patch_ids: Vec::new(),
            files: Vec::new(),
        };
        let mut groups = vec![
            group("a", "[IMP] x", Class::Blacklisted),
            group("b", "Revert \"[IMP] x\"", Class::Portable),
            group("c", "Revert \"[IMP] x", Class::Portable),
        ];
        reverted(&mut groups);
        let classes: Vec<Class> = groups.iter().map(|group| group.class).collect();
        assert_eq!(
            classes,
            [Class::Blacklisted, Class::Reverted, Class::Portable]
        );
    }

    #[test]
    fn missing_commits_grouped_and_classified() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fixture(dir);
        let runner = OsRunner;
        let repo = Repo::new(&runner, dir);
        let blacklist =
            Blacklist::parse(&repo.show("17.0", &crate::blacklist::path("pos_y")).unwrap())
                .unwrap();
        let targets = ["17.0".to_owned()];
        let query = Query {
            source: "16.0",
            targets: &targets,
            source_dir: "pos_x",
            target_dir: "pos_y",
            repository: "OCA/pos",
            blacklist: &blacklist,
        };
        let settings = Settings::new(None, None).unwrap();
        let groups = analyse(&repo, &query, &settings).unwrap();
        let summary: Vec<String> = groups
            .iter()
            .map(|g| {
                format!(
                    "{} {} {} {}",
                    g.key,
                    g.class.name(),
                    g.commits.len(),
                    g.title
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                "OCA/pos#11 migration commit 2 [IMP] pos_x: pre-commit auto fixes",
                "OCA/pos#10 portable 1 Revert \"[IMP] pos_x: gone\"",
                "OCA/pos#9 reverted 1 Revert \"[IMP] pos_x: cache\"",
                "OCA/pos#8 reverted 1 [IMP] pos_x: cache",
                "OCA/pos#7 blacklisted 1 [ADD] pos_x: report",
                "OCA/pos#6 repo-wide change 1 [BOT] readme",
                "OCA/pos#5 repo-wide change 1 [IMP] all: tests",
                "OCA/pos#4 translations 1 [UPD] translations",
                "OCA/pos#3 portable 1 [IMP] pos_x: c (#3)",
                "OCA/pos#2 portable 1 [FIX] pos_x: b",
                "orphaned_commits migration commit 1 [MIG] pos_x: Migration to 16.0",
            ]
        );
        assert_eq!(groups[4].reason.as_deref(), Some("not wanted on 17.0"));
        assert_eq!(groups[4].files, ["report.py"]);
        assert_eq!(groups[4].patch_ids.len(), 1);
        assert_eq!(groups[8].pr, Some(3));
        assert_eq!(groups[9].commits[0].author, "Jane");
        assert_eq!(groups[10].pr, None);

        // A strict threshold, custom patterns, and failures.
        let strict = Settings::new(Some(&["^\\[IMP\\]".to_owned()]), Some(10)).unwrap();
        let groups = analyse(&repo, &query, &strict).unwrap();
        assert_eq!(groups[6].class, Class::Migration);
        assert_eq!(groups[10].class, Class::Portable);
        assert!(
            Settings::new(Some(&["(".to_owned()]), None)
                .unwrap_err()
                .starts_with("migration_commit_patterns: \"(\"")
        );
        let bad = Query {
            source: "nope",
            ..query.clone()
        };
        assert!(analyse(&repo, &bad, &settings).is_err());
        let bad = Query {
            targets: &["nope".to_owned()],
            ..query.clone()
        };
        assert!(analyse(&repo, &bad, &settings).is_err());
        assert!(repo.files("nope").is_err());
        let files = ["pos_y/models.py".to_owned()];
        let tree = repo.tree_of("17.0", &files).unwrap();
        assert_eq!(tree.len(), 40);
        assert_ne!(
            repo.tree_of("16.0", &["pos_x/models.py".to_owned()])
                .unwrap(),
            tree
        );
        assert!(repo.tree_of("nope", &files).is_err());
        let failing = odx_core::fake::ScriptedRunner::default();
        failing.push(Err(std::io::ErrorKind::NotFound.into()));
        assert!(Repo::new(&failing, dir).files("x").is_err());
        assert!(repo.commits("nope", "x").is_err());
        let line = repo.first_parents("16.0").unwrap();
        assert_eq!(repo.merged_by("16.0", "16.0", &line), None);
        assert_eq!(repo.merged_by("nope", "16.0", &BTreeSet::new()), None);
        assert!(repo.first_parents("nope").is_err());
        assert_eq!(Class::Portable.name(), "portable");
        let missing = Repo::new(&runner, dir.join("nowhere"));
        assert!(missing.patch_ids("16.0", "x").is_err());
    }

    #[test]
    fn pull_request_numbers_from_messages() {
        assert_eq!(
            pull_request(Some("Merge pull request #12 from x/y"), "s"),
            Some(12)
        );
        assert_eq!(
            pull_request(
                Some("Merge branch 'a' into 'b'\n\nSee merge request group/x!34"),
                "s"
            ),
            Some(34)
        );
        assert_eq!(
            pull_request(Some("Merge branch 'a'"), "fix (#56)"),
            Some(56)
        );
        assert_eq!(pull_request(None, "fix"), None);
        assert_eq!(pull_request(Some("Merge PR #x"), "fix (#y)"), None);
        assert_eq!(pull_request(None, "fix (#7) "), Some(7));
        assert_eq!(pull_request(None, "fix (7)"), None);
    }
}
