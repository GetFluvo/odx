use odx_config::lock::{LockSource, LockedMerge, LockedRepository, Lockfile, MergeStatus};
use odx_core::repo_key;
use odx_git::{Aggregated, Aggregation, GitError, Merger, StepOutcome};

use crate::{PlanError, Planned};

/// The `odx.lock` entry of a built repository: the commits that were
/// merged, their trees and patch ids, and the resulting tree.
///
/// # Errors
///
/// [`PlanError::Lock`] when git cannot compute a tree or patch id.
pub fn lock_entry(
    planned: &Planned,
    aggregation: &Aggregation,
    built: &Aggregated,
    merger: &Merger,
) -> Result<LockedRepository, PlanError> {
    let cache = &aggregation.cache_repo;
    let locked_merges = planned
        .steps
        .iter()
        .zip(&aggregation.steps)
        .zip(&built.steps)
        .map(|((step, applied), (_, outcome))| {
            Ok(LockedMerge {
                source: step.lock.clone(),
                sha: applied.sha.clone(),
                tree: merger.tree(cache, &applied.sha)?,
                patch_id: merger.patch_id(cache, &aggregation.base, &applied.sha)?,
                mirror_ref: None,
                status: match outcome {
                    StepOutcome::Skipped { .. } => MergeStatus::Skipped,
                    StepOutcome::Merged | StepOutcome::UpToDate => MergeStatus::Merged,
                },
                upstream_head: None,
                upstream_head_ci: None,
                upstream_head_classification: None,
                pinned_since: None,
            })
        })
        .collect::<Result<Vec<_>, GitError>>()
        .map_err(|e| PlanError::Lock {
            repository: planned.name.clone(),
            message: e.to_string(),
        })?;
    Ok(LockedRepository {
        name: planned.name.clone(),
        url: planned.url.clone(),
        base: planned.base_name.clone(),
        base_sha: aggregation.base.clone(),
        result_tree: built.tree.clone(),
        merges: locked_merges,
    })
}

/// Put `entries` into `lock`, replacing entries of the same name; with
/// `keep` set to the names still configured, entries of other repositories
/// are removed.
pub fn update_lock(lock: &mut Lockfile, entries: Vec<LockedRepository>, keep: Option<&[String]>) {
    if let Some(keep) = keep {
        lock.repositories.retain(|entry| keep.contains(&entry.name));
    }
    for entry in entries {
        lock.repositories.retain(|old| old.name != entry.name);
        lock.repositories.push(entry);
    }
    lock.repositories
        .sort_by(|a, b| (&a.name, &a.url).cmp(&(&b.name, &b.url)));
}

/// What changed for one locked item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Newly locked.
    Added,
    /// No longer locked.
    Removed,
    /// A new commit with the same patch id: rebased, same change.
    Rebased,
    /// A new commit with different changes.
    Changed,
}

impl ChangeKind {
    /// How the change is named in output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Rebased => "rebased (same patch)",
            Self::Changed => "changed",
        }
    }
}

/// One difference between two lockfiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The repository.
    pub repository: String,
    /// `base <branch>`, a merge label such as `#123`, or empty for the
    /// whole repository.
    pub item: String,
    /// What changed.
    pub kind: ChangeKind,
    /// The old commit, if any.
    pub old: Option<String>,
    /// The new commit, if any.
    pub new: Option<String>,
}

/// `#123`, `commit abc1234` or `branch fix`.
pub fn source_label(source: &LockSource) -> String {
    match (&source.pr, &source.sha, &source.branch) {
        (Some(pr), _, _) => format!("#{pr}"),
        (None, Some(sha), _) => format!("commit {sha}"),
        (None, None, branch) => format!("branch {}", branch.as_deref().unwrap_or_default()),
    }
}

fn change(
    repository: &str,
    item: String,
    kind: ChangeKind,
    old: Option<&String>,
    new: Option<&String>,
) -> Change {
    Change {
        repository: repository.to_owned(),
        item,
        kind,
        old: old.cloned(),
        new: new.cloned(),
    }
}

/// Changes of packages and of the Odoo core.
fn package_changes(old: &Lockfile, new: &Lockfile) -> Vec<Change> {
    let mut changes = Vec::new();
    for (dist, entry) in &new.packages {
        match old.packages.get(dist) {
            None => changes.push(change(
                dist,
                "package".to_owned(),
                ChangeKind::Added,
                None,
                Some(&entry.sha),
            )),
            Some(was) if was.sha != entry.sha => changes.push(change(
                dist,
                "package".to_owned(),
                ChangeKind::Changed,
                Some(&was.sha),
                Some(&entry.sha),
            )),
            Some(_) => {}
        }
    }
    for (dist, was) in &old.packages {
        if !new.packages.contains_key(dist) {
            changes.push(change(
                dist,
                "package".to_owned(),
                ChangeKind::Removed,
                Some(&was.sha),
                None,
            ));
        }
    }
    let rev = |lock: &Lockfile| lock.odoo.as_ref().map(|odoo| odoo.rev.clone());
    let (before, after) = (rev(old), rev(new));
    if before != after {
        let kind = match (&before, &after) {
            (None, _) => ChangeKind::Added,
            (_, None) => ChangeKind::Removed,
            _ => ChangeKind::Changed,
        };
        changes.push(change(
            "odoo",
            "core".to_owned(),
            kind,
            before.as_ref(),
            after.as_ref(),
        ));
    }
    changes
}

/// Every difference between `old` and `new`, by repository name.
///
/// ```
/// use odx_aggregate::{ChangeKind, lock_changes};
/// use odx_config::lock::Lockfile;
///
/// assert!(lock_changes(&Lockfile::new(), &Lockfile::new()).is_empty());
/// ```
pub fn lock_changes(old: &Lockfile, new: &Lockfile) -> Vec<Change> {
    let mut changes = Vec::new();
    for entry in &new.repositories {
        let Some(previous) = old
            .repositories
            .iter()
            .find(|o| o.name == entry.name && repo_key(&o.url) == repo_key(&entry.url))
        else {
            changes.push(change(
                &entry.name,
                String::new(),
                ChangeKind::Added,
                None,
                Some(&entry.base_sha),
            ));
            continue;
        };
        if previous.base_sha != entry.base_sha || previous.base != entry.base {
            changes.push(change(
                &entry.name,
                format!("base {}", entry.base),
                ChangeKind::Changed,
                Some(&previous.base_sha),
                Some(&entry.base_sha),
            ));
        }
        for merge in &entry.merges {
            let label = source_label(&merge.source);
            match previous.merges.iter().find(|m| m.source == merge.source) {
                None => changes.push(change(
                    &entry.name,
                    label,
                    ChangeKind::Added,
                    None,
                    Some(&merge.sha),
                )),
                Some(was) if was.sha != merge.sha => {
                    let kind = if was.patch_id == merge.patch_id {
                        ChangeKind::Rebased
                    } else {
                        ChangeKind::Changed
                    };
                    changes.push(change(
                        &entry.name,
                        label,
                        kind,
                        Some(&was.sha),
                        Some(&merge.sha),
                    ));
                }
                Some(_) => {}
            }
        }
        for was in &previous.merges {
            if !entry.merges.iter().any(|m| m.source == was.source) {
                changes.push(change(
                    &entry.name,
                    source_label(&was.source),
                    ChangeKind::Removed,
                    Some(&was.sha),
                    None,
                ));
            }
        }
    }
    for previous in &old.repositories {
        if !new
            .repositories
            .iter()
            .any(|n| n.name == previous.name && repo_key(&n.url) == repo_key(&previous.url))
        {
            changes.push(change(
                &previous.name,
                String::new(),
                ChangeKind::Removed,
                Some(&previous.base_sha),
                None,
            ));
        }
    }
    changes.extend(package_changes(old, new));
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_config;
    use odx_config::lock::{LockedPackage, OdooPin};
    use odx_config::parse_pyproject;
    use odx_core::system::OsRunner;
    use odx_git::{Step, Strategy};
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

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

    fn source(pr: Option<u32>, sha: Option<&str>, branch: Option<&str>) -> LockSource {
        LockSource {
            pr,
            sha: sha.map(str::to_owned),
            branch: branch.map(str::to_owned),
            remote: None,
            tag: None,
        }
    }

    fn merge(source: LockSource, sha: char, patch: char) -> LockedMerge {
        LockedMerge {
            source,
            sha: sha.to_string().repeat(40),
            tree: "t".repeat(40),
            patch_id: patch.to_string().repeat(40),
            mirror_ref: None,
            status: MergeStatus::Merged,
            upstream_head: None,
            upstream_head_ci: None,
            upstream_head_classification: None,
            pinned_since: None,
        }
    }

    fn repo(name: &str, base_sha: char, merges: Vec<LockedMerge>) -> LockedRepository {
        LockedRepository {
            name: name.to_owned(),
            url: format!("https://github.com/OCA/{name}.git"),
            base: "17.0".to_owned(),
            base_sha: base_sha.to_string().repeat(40),
            result_tree: "r".repeat(40),
            merges,
        }
    }

    #[test]
    fn labels_and_kinds() {
        assert_eq!(source_label(&source(Some(3), None, None)), "#3");
        assert_eq!(source_label(&source(None, Some("abc"), None)), "commit abc");
        assert_eq!(source_label(&source(None, None, Some("fix"))), "branch fix");
        let kinds = [
            ChangeKind::Added,
            ChangeKind::Removed,
            ChangeKind::Rebased,
            ChangeKind::Changed,
        ];
        assert_eq!(
            kinds.map(ChangeKind::as_str),
            ["added", "removed", "rebased (same patch)", "changed"]
        );
    }

    #[test]
    fn changes_between_lockfiles() {
        let pr = |n| source(Some(n), None, None);
        let mut old = Lockfile::new();
        old.repositories = vec![
            repo(
                "pos",
                'a',
                vec![
                    merge(pr(1), '1', 'p'),
                    merge(pr(2), '2', 'q'),
                    merge(pr(3), '3', 'r'),
                ],
            ),
            repo("gone", 'a', Vec::new()),
        ];
        let mut new = Lockfile::new();
        new.repositories = vec![
            repo(
                "pos",
                'b',
                vec![
                    merge(pr(1), '4', 'p'),
                    merge(pr(2), '5', 'x'),
                    merge(pr(4), '6', 's'),
                ],
            ),
            repo("web", 'c', Vec::new()),
        ];
        let summary: Vec<(String, String, ChangeKind)> = lock_changes(&old, &new)
            .into_iter()
            .map(|c| (c.repository, c.item, c.kind))
            .collect();
        let expected = [
            ("pos", "base 17.0", ChangeKind::Changed),
            ("pos", "#1", ChangeKind::Rebased),
            ("pos", "#2", ChangeKind::Changed),
            ("pos", "#4", ChangeKind::Added),
            ("pos", "#3", ChangeKind::Removed),
            ("web", "", ChangeKind::Added),
            ("gone", "", ChangeKind::Removed),
        ]
        .map(|(r, i, k)| (r.to_owned(), i.to_owned(), k));
        assert_eq!(summary, expected);
        assert_eq!(lock_changes(&new, &new), []);
    }

    #[test]
    fn package_and_core_changes() {
        let package = |sha: &str| LockedPackage {
            group: None,
            url: "u".into(),
            source: source(Some(1), None, None),
            subdirectory: None,
            sha: sha.repeat(40),
            mirror_ref: None,
        };
        let odoo = |rev: &str| OdooPin {
            url: "o".into(),
            rev: rev.repeat(40),
        };
        let mut old = Lockfile::new();
        old.packages.insert("kept".into(), package("k"));
        old.packages.insert("moved".into(), package("a"));
        old.packages.insert("gone".into(), package("g"));
        let mut new = Lockfile::new();
        new.packages.insert("kept".into(), package("k"));
        new.packages.insert("moved".into(), package("b"));
        new.packages.insert("new".into(), package("n"));
        new.odoo = Some(odoo("1"));
        let kinds = |old: &Lockfile, new: &Lockfile| -> Vec<(String, ChangeKind)> {
            lock_changes(old, new)
                .into_iter()
                .map(|c| (c.repository, c.kind))
                .collect()
        };
        assert_eq!(
            kinds(&old, &new),
            [
                ("moved".to_owned(), ChangeKind::Changed),
                ("new".to_owned(), ChangeKind::Added),
                ("gone".to_owned(), ChangeKind::Removed),
                ("odoo".to_owned(), ChangeKind::Added),
            ]
        );
        let mut later = new.clone();
        later.odoo = Some(odoo("2"));
        assert_eq!(
            kinds(&new, &later),
            [("odoo".to_owned(), ChangeKind::Changed)]
        );
        later.odoo = None;
        assert_eq!(
            kinds(&new, &later),
            [("odoo".to_owned(), ChangeKind::Removed)]
        );
    }

    #[test]
    fn updates_replace_and_drop_entries() {
        let mut lock = Lockfile::new();
        lock.repositories = vec![repo("web", 'a', Vec::new()), repo("pos", 'a', Vec::new())];
        update_lock(&mut lock, vec![repo("pos", 'b', Vec::new())], None);
        let names: Vec<(&str, &str)> = lock
            .repositories
            .iter()
            .map(|r| (r.name.as_str(), &r.base_sha[..1]))
            .collect();
        assert_eq!(names, [("pos", "b"), ("web", "a")]);
        update_lock(&mut lock, Vec::new(), Some(&["pos".to_owned()]));
        assert_eq!(lock.repositories.len(), 1);
    }

    #[test]
    fn entries_record_commits_trees_and_patch_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        std::fs::write(up.join("a.txt"), "a\n").unwrap();
        git(&up, &["add", "a.txt"]);
        git(&up, &["commit", "--quiet", "-m", "a"]);
        git(&up, &["checkout", "--quiet", "-b", "fix"]);
        std::fs::write(up.join("b.txt"), "b\n").unwrap();
        git(&up, &["add", "b.txt"]);
        git(&up, &["commit", "--quiet", "-m", "b"]);
        let cache = tmp.path().join("cache.git");
        git(
            tmp.path(),
            &[
                "clone",
                "--quiet",
                "--bare",
                up.to_str().unwrap(),
                cache.to_str().unwrap(),
            ],
        );
        let text = "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"u\"\nmerges = [{ branch = \"fix\" }, { branch = \"17.0\" }]\n";
        let config = parse_pyproject(text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config;
        let planned = plan_config(&config, tmp.path(), None).unwrap();
        let fix = git(&cache, &["rev-parse", "fix"]);
        let base = git(&cache, &["rev-parse", "17.0"]);
        let aggregation = Aggregation {
            cache_repo: cache.clone(),
            target_dir: tmp.path().join("work"),
            base: base.clone(),
            steps: vec![
                Step {
                    label: "branch fix".into(),
                    sha: fix.clone(),
                    skip_on_conflict: false,
                },
                Step {
                    label: "branch 17.0".into(),
                    sha: base.clone(),
                    skip_on_conflict: true,
                },
            ],
            strategy: Strategy::Rebase,
            mergiraf: None,
            keep_conflicts: false,
            force: false,
        };
        let merger = Merger::new(Arc::new(OsRunner));
        let mut built = merger.aggregate(&aggregation).unwrap();
        built.steps[1].1 = StepOutcome::Skipped { files: Vec::new() };
        let entry = lock_entry(&planned[0], &aggregation, &built, &merger).unwrap();
        assert_eq!((entry.name.as_str(), entry.base.as_str()), ("pos", "17.0"));
        assert_eq!(entry.base_sha, base);
        assert_eq!(entry.result_tree, built.tree);
        assert_eq!(entry.merges[0].sha, fix);
        assert_eq!(
            entry.merges[0].tree,
            git(&cache, &["rev-parse", "fix^{tree}"])
        );
        assert_eq!(entry.merges[0].status, MergeStatus::Merged);
        assert_eq!(entry.merges[1].patch_id, "0".repeat(40));
        assert_eq!(entry.merges[1].status, MergeStatus::Skipped);
        assert_eq!(entry.merges[1].source.branch.as_deref(), Some("17.0"));

        let broken = Aggregation {
            cache_repo: tmp.path().join("nope"),
            ..aggregation.clone()
        };
        let err = lock_entry(&planned[0], &broken, &built, &merger).unwrap_err();
        assert!(
            err.to_string().starts_with("pos: git rev-parse failed"),
            "{err}"
        );
        let no_patch_ids = Merger::new(Arc::new(FailOn("patch-id")));
        assert!(lock_entry(&planned[0], &aggregation, &built, &no_patch_ids).is_err());
    }

    /// Fails invocations with `needle` among their arguments.
    #[derive(Debug)]
    struct FailOn(&'static str);

    impl odx_core::ProcessRunner for FailOn {
        fn run(&self, invocation: &odx_core::Invocation) -> std::io::Result<odx_core::Output> {
            if invocation.arguments().iter().any(|a| a == self.0) {
                return Err(std::io::Error::other("injected"));
            }
            OsRunner.run(invocation)
        }
    }
}
