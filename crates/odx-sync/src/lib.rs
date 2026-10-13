//! Background prefetching, workspace registry and cache locking for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `odx sync` prefetches
//! the registered workspaces into the shared cache; this crate holds the
//! registry edits, the sync record and the cache's garbage collection.

use std::path::Path;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

pub mod schedule;

/// Register `path` under `[workspaces]` in `doc` (the user configuration),
/// with its registration date and series; an existing entry is left as it
/// is. A `workspaces` key that is no table is replaced.
pub fn register(doc: &mut DocumentMut, path: &str, date: &str, series: &str) {
    if !doc.get("workspaces").is_some_and(Item::is_table_like) {
        doc.insert("workspaces", Item::Table(Table::new()));
    }
    if let Some(table) = doc.get_mut("workspaces").and_then(Item::as_table_like_mut)
        && table.get(path).is_none()
    {
        let mut entry = toml_edit::InlineTable::new();
        entry.insert("registered", date.into());
        entry.insert("series", series.into());
        table.insert(path, value(entry));
    }
}

/// Remove the `[workspaces]` entry of `path` from `doc`; whether there was
/// one.
pub fn unregister(doc: &mut DocumentMut, path: &str) -> bool {
    doc.get_mut("workspaces")
        .and_then(Item::as_table_like_mut)
        .is_some_and(|table| table.remove(path).is_some())
}

/// Set `[sync] enabled` in `doc` (the user configuration), and `interval`
/// when given. A `sync` key that is no table is replaced.
pub fn set_sync(doc: &mut DocumentMut, enabled: bool, interval: Option<&str>) {
    let mut table = match doc.remove("sync") {
        Some(Item::Table(table)) => table,
        _ => Table::new(),
    };
    table.insert("enabled", value(enabled));
    if let Some(interval) = interval {
        table.insert("interval", value(interval));
    }
    doc.insert("sync", Item::Table(table));
}

/// A size written as a number and a unit: `"500MB"`, `"20GB"` (powers of
/// 1024; `B`, `KB`, `MB`, `GB`, `TB`).
///
/// ```
/// assert_eq!(odx_sync::parse_size("20GB"), Some(20 * 1024 * 1024 * 1024));
/// assert_eq!(odx_sync::parse_size("512"), Some(512));
/// assert_eq!(odx_sync::parse_size("big"), None);
/// ```
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let digits = text.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let unit = text
        .get(digits.len()..)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let factor: u64 = match unit.as_str() {
        "" | "B" => 1,
        "K" | "KB" => 1 << 10,
        "M" | "MB" => 1 << 20,
        "G" | "GB" => 1 << 30,
        "T" | "TB" => 1 << 40,
        _ => return None,
    };
    digits.trim().parse::<u64>().ok()?.checked_mul(factor)
}

/// A repository in the cache, as garbage collection sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedRepo {
    /// Its directory name (the URL hash).
    pub hash: String,
    /// Bytes on disk.
    pub size: u64,
    /// Seconds since the epoch it was last fetched into (its mtime).
    pub used: u64,
    /// Whether a registered workspace uses it.
    pub referenced: bool,
}

/// The repositories to remove: those no workspace uses, least recently
/// used first, until the cache (of `total` bytes) fits `max_size`; all of
/// them without a limit.
pub fn to_collect(repos: &[CachedRepo], total: u64, max_size: Option<u64>) -> Vec<&CachedRepo> {
    let mut candidates: Vec<&CachedRepo> = repos.iter().filter(|repo| !repo.referenced).collect();
    candidates.sort_by_key(|repo| (repo.used, repo.hash.clone()));
    let Some(max_size) = max_size else {
        return candidates;
    };
    let mut remaining = total;
    candidates
        .into_iter()
        .take_while(|repo| {
            let keep_going = remaining > max_size;
            remaining = remaining.saturating_sub(repo.size);
            keep_going
        })
        .collect()
}

/// What the last `odx sync` did (`last_sync.json` in the cache).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRecord {
    /// Format version.
    pub schema: u32,
    /// Seconds since the epoch the run started.
    pub started: u64,
    /// Seconds since the epoch it ended.
    pub finished: u64,
    /// Per workspace, in order.
    pub workspaces: Vec<SyncedWorkspace>,
}

/// One workspace of a sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedWorkspace {
    /// The workspace.
    pub path: String,
    /// `synced`, `failed` or `skipped`.
    pub status: String,
    /// What happened, for people.
    pub detail: String,
}

impl SyncRecord {
    /// Read `last_sync.json`, if there is a readable one.
    pub fn read(path: &Path) -> Option<Self> {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Write it to `path` through a temporary file, so readers never see
    /// a partial record.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        let temporary = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temporary, text))
            .and_then(|()| std::fs::rename(&temporary, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_edits() {
        let text = "# mine\n[cache]\ndir = \"~/c\"\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        register(&mut doc, "/w/a", "2026-10-13", "17.0");
        let registered = doc.to_string();
        assert_eq!(
            registered,
            "# mine\n[cache]\ndir = \"~/c\"\n\n[workspaces]\n\"/w/a\" = { registered = \"2026-10-13\", series = \"17.0\" }\n"
        );
        register(&mut doc, "/w/a", "2027-01-01", "18.0");
        assert_eq!(doc.to_string(), registered);
        assert!(unregister(&mut doc, "/w/a"));
        assert_eq!(
            doc.to_string(),
            "# mine\n[cache]\ndir = \"~/c\"\n\n[workspaces]\n"
        );
        assert!(!unregister(&mut doc, "/w/a"));
        assert!(!unregister(&mut DocumentMut::new(), "/w/a"));
        let mut odd: DocumentMut = "workspaces = 1\n".parse().unwrap();
        register(&mut odd, "/w", "d", "s");
        assert!(
            odd.to_string().contains("\"/w\" = { registered = \"d\""),
            "{odd}"
        );
    }

    #[test]
    fn sync_switch() {
        let mut doc: DocumentMut = "[sync]\njobs = 1 # few\n\n[cache]\n".parse().unwrap();
        set_sync(&mut doc, true, Some("15m"));
        assert_eq!(
            doc.to_string(),
            "[sync]\njobs = 1 # few\nenabled = true\ninterval = \"15m\"\n\n[cache]\n"
        );
        set_sync(&mut doc, false, None);
        assert!(
            doc.to_string().contains("enabled = false\ninterval"),
            "{doc}"
        );
        let mut odd: DocumentMut = "sync = 1\n".parse().unwrap();
        set_sync(&mut odd, true, None);
        assert_eq!(odd.to_string(), "[sync]\nenabled = true\n");
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size(" 1 kb "), Some(1024));
        assert_eq!(parse_size("2T"), Some(2 << 40));
        assert_eq!(parse_size("3B"), Some(3));
        assert_eq!(parse_size("5M"), Some(5 << 20));
        assert_eq!(parse_size("1PB"), None);
        assert_eq!(parse_size("99999999999TB"), None);
        assert_eq!(parse_size("GB"), None);
    }

    fn repo(hash: &str, size: u64, used: u64, referenced: bool) -> CachedRepo {
        CachedRepo {
            hash: hash.to_owned(),
            size,
            used,
            referenced,
        }
    }

    #[test]
    fn least_recently_used_unreferenced_repositories_go() {
        let repos = [
            repo("new", 10, 300, false),
            repo("used", 50, 100, true),
            repo("old", 10, 100, false),
            repo("older", 10, 50, false),
        ];
        let hashes =
            |list: Vec<&CachedRepo>| list.iter().map(|r| r.hash.clone()).collect::<Vec<_>>();
        assert_eq!(
            hashes(to_collect(&repos, 80, None)),
            ["older", "old", "new"]
        );
        assert_eq!(hashes(to_collect(&repos, 80, Some(65))), ["older", "old"]);
        assert_eq!(
            hashes(to_collect(&repos, 80, Some(80))),
            Vec::<String>::new()
        );
        assert_eq!(
            hashes(to_collect(&repos, 80, Some(0))),
            ["older", "old", "new"]
        );
    }

    #[test]
    fn records() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cache").join("last_sync.json");
        assert_eq!(SyncRecord::read(&path), None);
        let record = SyncRecord {
            schema: 1,
            started: 1,
            finished: 2,
            workspaces: vec![SyncedWorkspace {
                path: "/w".to_owned(),
                status: "synced".to_owned(),
                detail: "fetched 2 refs".to_owned(),
            }],
        };
        record.write(&path).unwrap();
        assert_eq!(SyncRecord::read(&path), Some(record.clone()));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "nonsense").unwrap();
        assert_eq!(SyncRecord::read(&path), None);
        let blocked = tmp.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        assert!(record.write(&blocked.join("last_sync.json")).is_err());
    }
}
