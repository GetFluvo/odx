//! Background prefetching, workspace registry and cache locking for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `odx sync` prefetches
//! the registered workspaces into the shared cache; this crate holds the
//! registry edits, the sync record and the cache's garbage collection.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// A foreground odx command, visible to `odx sync` while it lives: a file
/// under `active/` in the cache, locked for as long as the command runs, so
/// a file nobody holds is left over from a command that died.
#[derive(Debug)]
pub struct Presence {
    path: PathBuf,
    file: Option<File>,
}

/// Tells presences of one process apart.
static PRESENCES: AtomicUsize = AtomicUsize::new(0);

impl Presence {
    /// Announce a foreground command in `cache`; `None` when the file
    /// cannot be created (the command runs anyway).
    pub fn enter(cache: &Path) -> Option<Self> {
        let dir = cache.join("active");
        let name = format!(
            "{}-{}",
            std::process::id(),
            PRESENCES.fetch_add(1, Ordering::Relaxed)
        );
        let path = dir.join(&name);
        // Locked under a hidden name first, which `busy` passes over: a
        // file it saw before the lock would look left over and be removed.
        let hidden = dir.join(format!(".{name}"));
        let file = fs::create_dir_all(&dir)
            .and_then(|()| File::create(&hidden))
            .and_then(|file| file.lock().map(|()| file))
            .and_then(|file| fs::rename(&hidden, &path).map(|()| file));
        let Ok(file) = file else {
            let _ = fs::remove_file(&hidden);
            return None;
        };
        Some(Self {
            path,
            file: Some(file),
        })
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        // Unlock first: a file still open cannot be removed everywhere.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// Whether a foreground odx command is running on `cache`; presence files
/// of commands that died are removed on the way.
pub fn busy(cache: &Path) -> bool {
    let mut live = false;
    for entry in fs::read_dir(cache.join("active"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if File::open(&path).is_ok_and(|file| file.try_lock().is_ok()) {
            let _ = fs::remove_file(&path);
        } else {
            live = true;
        }
    }
    live
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
        fs::read(path)
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
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&temporary, text))
            .and_then(|()| fs::rename(&temporary, path))
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
    fn presences() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        assert!(!busy(&cache));
        let first = Presence::enter(&cache).unwrap();
        let second = Presence::enter(&cache).unwrap();
        assert_ne!(first.path, second.path);
        assert!(busy(&cache));
        drop(first);
        assert!(busy(&cache));
        drop(second);
        assert!(!busy(&cache));
        assert_eq!(fs::read_dir(cache.join("active")).unwrap().count(), 0);
        // Left over from a command that died: nobody holds it.
        fs::write(cache.join("active").join("1-0"), "").unwrap();
        assert!(!busy(&cache));
        assert!(!cache.join("active").join("1-0").exists());
        // One being entered, not locked yet, is left alone.
        fs::write(cache.join("active").join(".1-1"), "").unwrap();
        assert!(!busy(&cache));
        assert!(cache.join("active").join(".1-1").exists());
        // No cache to announce in.
        let blocked = tmp.path().join("file");
        fs::write(&blocked, "").unwrap();
        assert!(Presence::enter(&blocked).is_none());
        // A presence that cannot take its name leaves nothing behind.
        let taken = tmp.path().join("taken");
        let next = PRESENCES.load(Ordering::Relaxed);
        fs::create_dir_all(
            taken
                .join("active")
                .join(format!("{}-{next}", std::process::id()))
                .join("x"),
        )
        .unwrap();
        assert!(Presence::enter(&taken).is_none());
        assert!(
            !taken
                .join("active")
                .join(format!(".{}-{next}", std::process::id()))
                .exists()
        );
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
        fs::write(&path, "nonsense").unwrap();
        assert_eq!(SyncRecord::read(&path), None);
        let blocked = tmp.path().join("file");
        fs::write(&blocked, "").unwrap();
        assert!(record.write(&blocked.join("last_sync.json")).is_err());
    }
}
