//! Verdicts and mutes for odx: version-aware suppression of upstream change suggestions, shared across projects.
//!
//! Part of [odx](https://github.com/getfluvo/odx). A verdict states a fact
//! about an upstream change for an addon on a target series ("not needed on
//! 19.0"); a mute is a project's decision. Both carry a [`Kind`] and a
//! mandatory reason, are keyed by the change's patch-ids (and its PR as the
//! human handle), and are kept in TOML files: `verdicts/<series>/<addon>.toml`
//! for the organisation, `projects/<id>/mutes.toml` per project, or a
//! project's own `.odx/mutes.toml` without a verdict repository. Entries are
//! appended and never deleted; revoking records who, when and why.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

/// What a verdict or mute says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// The change does not apply to the target series.
    NotApplicable,
    /// The problem does not exist on the target series.
    NotNeeded,
    /// Another change on the target covers it.
    Superseded,
    /// Present on the target by another route.
    AlreadyPresent,
    /// Rejected for other reasons.
    Declined,
    /// The project does not want it.
    NotWanted,
    /// Review later.
    Deferred,
}

impl Kind {
    /// How the kind is written.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::NotNeeded => "not-needed",
            Self::Superseded => "superseded",
            Self::AlreadyPresent => "already-present",
            Self::Declined => "declined",
            Self::NotWanted => "not-wanted",
            Self::Deferred => "deferred",
        }
    }

    /// The scope an entry of this kind gets unless one is chosen: facts
    /// about the code are for the organisation, decisions for the project.
    #[must_use]
    pub const fn default_scope(self) -> Scope {
        match self {
            Self::NotApplicable | Self::NotNeeded | Self::Superseded | Self::AlreadyPresent => {
                Scope::Org
            }
            Self::Declined | Self::NotWanted | Self::Deferred => Scope::Project,
        }
    }

    /// Whether a change of the touched files on the target series makes
    /// the entry stale by default.
    #[must_use]
    pub const fn rechecks(self) -> bool {
        !matches!(self, Self::Declined | Self::NotWanted)
    }
}

/// Who an entry applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Some environments of one project.
    Env,
    /// One project.
    Project,
    /// Every project of the organisation.
    Org,
}

/// When an entry is looked at again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recheck {
    /// `target-code-change` or `never`.
    pub on: String,
    /// The repository of the target series.
    pub repo: String,
    /// The touched files, as on the target series.
    pub files: Vec<String>,
    /// [`tree_id`]-style id of those files on the target at verdict time.
    pub target_tree: String,
}

/// A revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revoked {
    /// Who.
    pub by: String,
    /// When (`YYYY-MM-DD`).
    pub date: String,
    /// Why.
    pub reason: String,
}

/// A verdict or a mute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `v:<series>:<addon>:<patch-id prefix>` or `m:...` for mutes.
    pub id: String,
    /// The human handle: `<owner>/<repo>#<n>` or `orphaned_commits`.
    pub change: String,
    /// The patch-ids of the change's commits.
    pub patch_ids: Vec<String>,
    /// The series the change came from.
    pub source_series: String,
    /// The series it was judged for.
    pub target_series: String,
    /// The addon.
    pub addon: String,
    /// What it says.
    pub kind: Kind,
    /// Why; never empty.
    pub reason: String,
    /// Who recorded it.
    pub author: String,
    /// When (`YYYY-MM-DD`).
    pub date: String,
    /// Expiry (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// For environment mutes, the environments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<String>>,
    /// When to look again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recheck: Option<Recheck>,
    /// Set when revoked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<Revoked>,
}

/// Where an entry stands for a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// It suppresses the suggestion.
    Applies,
    /// It did, but the target code changed since: the suggestion
    /// resurfaces with the old reason.
    Stale,
}

impl Entry {
    /// Whether the entry is about this change: a shared patch-id, or the
    /// same handle, for the same addon and target series.
    #[must_use]
    pub fn concerns(&self, change: &str, patch_ids: &[String], addon: &str, target: &str) -> bool {
        self.addon == addon
            && self.target_series == target
            && (self.change == change || self.patch_ids.iter().any(|id| patch_ids.contains(id)))
    }

    /// Whether it is in force on `today` (`YYYY-MM-DD`) for `env` (an
    /// environment mute only for its environments).
    #[must_use]
    pub fn in_force(&self, today: &str, env: Option<&str>) -> bool {
        self.revoked.is_none()
            && self.until.as_deref().is_none_or(|until| until >= today)
            && self
                .environments
                .as_ref()
                .is_none_or(|envs| env.is_some_and(|env| envs.iter().any(|e| e == env)))
    }

    /// Applies, or stale when it rechecks on target code changes and
    /// `current_tree` (the touched files on the target now) differs.
    #[must_use]
    pub fn standing(&self, current_tree: Option<&str>) -> Standing {
        match (&self.recheck, current_tree) {
            (Some(recheck), Some(tree))
                if recheck.on == "target-code-change" && recheck.target_tree != tree =>
            {
                Standing::Stale
            }
            _ => Standing::Applies,
        }
    }

    /// The entry as a TOML table.
    #[must_use]
    pub fn table(&self) -> Table {
        let mut table = Table::new();
        let strings = |list: &[String]| {
            let mut array = Array::new();
            for item in list {
                array.push(item.as_str());
            }
            array
        };
        table.insert("id", value(&self.id));
        table.insert("change", value(&self.change));
        table.insert("patch_ids", value(strings(&self.patch_ids)));
        table.insert("source_series", value(&self.source_series));
        table.insert("target_series", value(&self.target_series));
        table.insert("addon", value(&self.addon));
        table.insert("kind", value(self.kind.name()));
        table.insert("reason", value(&self.reason));
        table.insert("author", value(&self.author));
        table.insert("date", value(&self.date));
        if let Some(until) = &self.until {
            table.insert("until", value(until));
        }
        if let Some(envs) = &self.environments {
            table.insert("environments", value(strings(envs)));
        }
        if let Some(recheck) = &self.recheck {
            let mut inner = Table::new();
            inner.insert("on", value(&recheck.on));
            inner.insert("repo", value(&recheck.repo));
            inner.insert("files", value(strings(&recheck.files)));
            inner.insert("target_tree", value(&recheck.target_tree));
            table.insert("recheck", Item::Table(inner));
        }
        table
    }
}

/// The file of organisation verdicts for `addon` on `series` in a verdict
/// repository at `root`.
#[must_use]
pub fn verdict_file(root: &Path, series: &str, addon: &str) -> PathBuf {
    root.join("verdicts")
        .join(series)
        .join(format!("{addon}.toml"))
}

/// The mutes file of project `project` in a verdict repository at `root`.
#[must_use]
pub fn mutes_file(root: &Path, project: &str) -> PathBuf {
    root.join("projects").join(project).join("mutes.toml")
}

/// The array of entries a file keeps: `verdict` or `mute`.
fn key_of(file: &Path) -> &'static str {
    if file.file_name().is_some_and(|name| name == "mutes.toml") {
        "mute"
    } else {
        "verdict"
    }
}

/// The entries of `file`; none when it does not exist.
///
/// # Errors
///
/// When the file cannot be read or is not a file of entries.
pub fn read(file: &Path) -> Result<Vec<Entry>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        verdict: Vec<Entry>,
        #[serde(default)]
        mute: Vec<Entry>,
    }
    let Ok(text) = std::fs::read_to_string(file) else {
        return Ok(Vec::new());
    };
    let parsed: File = toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(parsed.verdict.into_iter().chain(parsed.mute).collect())
}

/// Every entry under a verdict repository at `root`, with its file.
///
/// # Errors
///
/// When a file cannot be read.
pub fn read_all(root: &Path) -> Result<Vec<(PathBuf, Entry)>, String> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["verdicts", "projects"] {
        for group in std::fs::read_dir(root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
        {
            for file in std::fs::read_dir(group.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                files.push(file.path());
            }
        }
    }
    files.retain(|file| file.extension().is_some_and(|ext| ext == "toml"));
    files.sort();
    let mut all = Vec::new();
    for file in files {
        all.extend(read(&file)?.into_iter().map(|entry| (file.clone(), entry)));
    }
    Ok(all)
}

/// Load `file` for editing (empty when missing).
fn load(file: &Path) -> Result<DocumentMut, String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .parse()
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn store(file: &Path, doc: &DocumentMut) -> Result<(), String> {
    file.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(file, doc.to_string()))
        .map_err(|e| format!("{}: {e}", file.display()))
}

/// Append `entry` to `file`, keeping what is there byte for byte.
///
/// # Errors
///
/// When the reason is empty, or the file cannot be read or written.
pub fn append(file: &Path, entry: &Entry) -> Result<(), String> {
    if entry.reason.trim().is_empty() {
        return Err("a verdict needs a reason".to_owned());
    }
    let mut doc = load(file)?;
    let key = key_of(file);
    let mut entries = match doc.remove(key) {
        Some(Item::ArrayOfTables(entries)) => entries,
        _ => ArrayOfTables::new(),
    };
    entries.push(entry.table());
    doc.insert(key, Item::ArrayOfTables(entries));
    store(file, &doc)
}

/// Change the entry `id` in `file` with `edit`; whether it was there.
///
/// # Errors
///
/// When the file cannot be read or written.
pub fn update(file: &Path, id: &str, edit: impl FnOnce(&mut Table)) -> Result<bool, String> {
    let mut doc = load(file)?;
    let key = key_of(file);
    let found = doc
        .get_mut(key)
        .and_then(Item::as_array_of_tables_mut)
        .and_then(|entries| {
            entries
                .iter_mut()
                .find(|table| table.get("id").and_then(Item::as_str) == Some(id))
        });
    let Some(table) = found else {
        return Ok(false);
    };
    edit(table);
    store(file, &doc).map(|()| true)
}

/// Record a revocation on a table.
pub fn revoke(table: &mut Table, revoked: &Revoked) {
    let mut inner = Table::new();
    inner.insert("by", value(&revoked.by));
    inner.insert("date", value(&revoked.date));
    inner.insert("reason", value(&revoked.reason));
    table.insert("revoked", Item::Table(inner));
}

/// Record a reviewed target tree on a table (after confirming a stale
/// entry).
pub fn confirm(table: &mut Table, tree: &str, date: &str) {
    if let Some(recheck) = table.get_mut("recheck").and_then(Item::as_table_mut) {
        recheck.insert("target_tree", value(tree));
    }
    table.insert("date", value(date));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind) -> Entry {
        Entry {
            id: "v:19.0:contract:5f6e7d8c".to_owned(),
            change: "OCA/contract#1480".to_owned(),
            patch_ids: vec!["5f6e7d8c".to_owned()],
            source_series: "18.0".to_owned(),
            target_series: "19.0".to_owned(),
            addon: "contract".to_owned(),
            kind,
            reason: "19.0 sends through account.move.send".to_owned(),
            author: "dev@example.com".to_owned(),
            date: "2026-10-12".to_owned(),
            until: None,
            environments: None,
            recheck: Some(Recheck {
                on: "target-code-change".to_owned(),
                repo: "OCA/contract".to_owned(),
                files: vec!["contract/models/contract.py".to_owned()],
                target_tree: "9a8b".to_owned(),
            }),
            revoked: None,
        }
    }

    #[test]
    fn kinds() {
        let all = [
            Kind::NotApplicable,
            Kind::NotNeeded,
            Kind::Superseded,
            Kind::AlreadyPresent,
            Kind::Declined,
            Kind::NotWanted,
            Kind::Deferred,
        ];
        assert_eq!(
            all.map(Kind::name),
            [
                "not-applicable",
                "not-needed",
                "superseded",
                "already-present",
                "declined",
                "not-wanted",
                "deferred"
            ]
        );
        assert_eq!(all.map(Kind::default_scope)[0], Scope::Org);
        assert_eq!(Kind::Deferred.default_scope(), Scope::Project);
        assert!(Kind::Deferred.rechecks() && !Kind::Declined.rechecks());
    }

    #[test]
    fn matching_and_standing() {
        let mut verdict = entry(Kind::NotNeeded);
        let ids = vec!["5f6e7d8c".to_owned()];
        assert!(verdict.concerns("OCA/other#1", &ids, "contract", "19.0"));
        assert!(verdict.concerns("OCA/contract#1480", &[], "contract", "19.0"));
        assert!(!verdict.concerns("OCA/contract#1480", &ids, "contract", "18.0"));
        assert!(!verdict.concerns("x", &[], "contract", "19.0"));
        assert!(verdict.in_force("2026-10-13", None));
        assert_eq!(verdict.standing(Some("9a8b")), Standing::Applies);
        assert_eq!(verdict.standing(Some("ffff")), Standing::Stale);
        assert_eq!(verdict.standing(None), Standing::Applies);
        verdict.until = Some("2026-10-01".to_owned());
        assert!(!verdict.in_force("2026-10-13", None));
        verdict.until = None;
        verdict.environments = Some(vec!["uat".to_owned()]);
        assert!(verdict.in_force("2026-10-13", Some("uat")));
        assert!(!verdict.in_force("2026-10-13", Some("prod")));
        assert!(!verdict.in_force("2026-10-13", None));
        verdict.recheck = None;
        assert_eq!(verdict.standing(Some("ffff")), Standing::Applies);
    }

    /// An edit that changes nothing.
    fn untouched(_: &mut Table) {}

    #[test]
    fn files_are_appended_updated_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let file = verdict_file(root, "19.0", "contract");
        assert_eq!(read(&file).unwrap(), Vec::new());
        let first = entry(Kind::NotNeeded);
        append(&file, &first).unwrap();
        let mut second = entry(Kind::Deferred);
        second.id = "v:19.0:contract:2".to_owned();
        second.until = Some("2027-01-01".to_owned());
        second.environments = Some(vec!["uat".to_owned()]);
        second.recheck = None;
        append(&file, &second).unwrap();
        assert_eq!(read(&file).unwrap(), [first.clone(), second.clone()]);
        let before = std::fs::read_to_string(&file).unwrap();
        assert!(
            before.starts_with("[[verdict]]\nid = \"v:19.0:contract:5f6e7d8c\"\n"),
            "{before}"
        );

        let revoked = Revoked {
            by: "lead@example.com".to_owned(),
            date: "2026-10-13".to_owned(),
            reason: "wrong".to_owned(),
        };
        assert!(update(&file, &first.id, |table| revoke(table, &revoked)).unwrap());
        assert!(
            update(&file, &first.id, |table| confirm(
                table,
                "ffff",
                "2026-10-14"
            ))
            .unwrap()
        );
        assert!(update(&file, &second.id, |table| confirm(table, "x", "2026-10-14")).unwrap());
        assert!(!update(&file, "nope", untouched).unwrap());
        assert!(update(&file, &first.id, untouched).unwrap());
        let read_back = read(&file).unwrap();
        assert_eq!(read_back[0].revoked.as_ref(), Some(&revoked));
        assert_eq!(read_back[0].recheck.as_ref().unwrap().target_tree, "ffff");
        assert_eq!(read_back[1].date, "2026-10-14");

        let mutes = mutes_file(root, "abc");
        let mut mute = entry(Kind::NotWanted);
        mute.id = "m:abc:1".to_owned();
        append(&mutes, &mute).unwrap();
        assert!(
            std::fs::read_to_string(&mutes)
                .unwrap()
                .starts_with("[[mute]]\n")
        );
        std::fs::write(root.join("verdicts").join("README.md"), "x").unwrap();
        std::fs::write(root.join("verdicts").join("19.0").join("notes.txt"), "x").unwrap();
        let all = read_all(root).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].1, mute);

        let mut empty = entry(Kind::NotNeeded);
        empty.reason = " ".to_owned();
        assert_eq!(
            append(&file, &empty).unwrap_err(),
            "a verdict needs a reason"
        );
        std::fs::write(&file, "verdict = 1\n").unwrap();
        assert!(read(&file).is_err());
        assert!(read_all(root).is_err());
        append(&file, &first).unwrap();
        assert_eq!(read(&file).unwrap().len(), 1);
        std::fs::write(&file, "[[verdict\n").unwrap();
        assert!(append(&file, &first).is_err());
        assert!(update(&file, "x", untouched).is_err());
        let blocked = root.join("blocked");
        std::fs::write(&blocked, "").unwrap();
        assert!(append(&blocked.join("x.toml"), &first).is_err());
    }
}
