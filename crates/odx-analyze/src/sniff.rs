//! `odx sniff`: database migration scripts that changes bring in.
//!
//! Odoo runs `pre-*.py`, `post-*.py` and `end-*.py` from
//! `<addon>/migrations/<version>/` (or `upgrades/`) when an addon's version
//! moves past `<version>`; OpenUpgrade keeps the same files under
//! `openupgrade_scripts/scripts/<addon>/<version>/`. A merge that adds one
//! must not go unnoticed: the database is only migrated when the addon
//! version changes on deploy.

use std::collections::BTreeMap;

use serde::Serialize;

/// When Odoo runs a migration script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// Before the addon's models are loaded.
    Pre,
    /// After the addon is updated.
    Post,
    /// After every addon is updated.
    End,
}

impl Phase {
    /// The phase as Odoo names it in file names.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Post => "post",
            Self::End => "end",
        }
    }
}

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    /// The file is new.
    Added,
    /// The file existed and changed.
    Modified,
}

/// A migration script that a change adds or modifies.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Script {
    /// The addon the script migrates.
    pub addon: String,
    /// The version directory: the script runs when the addon moves past it.
    pub version: String,
    /// When the script runs.
    pub phase: Phase,
    /// Path of the script in its repository.
    pub path: String,
    /// Added or modified.
    pub change: Change,
}

/// What a change does to one addon's database migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddonChange {
    /// The addon.
    pub addon: String,
    /// The manifest version before the change, if known.
    pub previous_version: Option<String>,
    /// The manifest version after the change, if known.
    pub version: Option<String>,
    /// Migration scripts added or modified, sorted by path.
    pub scripts: Vec<Script>,
}

impl AddonChange {
    /// The version changed but no migration script came with it: often
    /// fine (OCA bumps the version for every change), worth a look when
    /// the change touches data.
    pub fn bumped_without_scripts(&self) -> bool {
        self.scripts.is_empty()
            && self.version.is_some()
            && self.previous_version.is_some()
            && self.version != self.previous_version
    }
}

/// The migration script at `path`, if it is one.
///
/// Odoo's `<addon>/{migrations,upgrades}/<version>/{pre,post,end}-*.py` or
/// OpenUpgrade's `openupgrade_scripts/scripts/<addon>/<version>/…`, at any
/// depth in the repository.
pub fn script(path: &str, change: Change) -> Option<Script> {
    let mut parts = path.rsplit('/');
    let (Some(file), Some(version), Some(dir)) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    let phase = phase(file)?;
    let parent = parts.next();
    let addon = if matches!(dir, "migrations" | "upgrades") {
        parent?
    } else if (parent, parts.next()) == (Some("scripts"), Some("openupgrade_scripts")) {
        dir
    } else {
        return None;
    };
    Some(Script {
        addon: addon.to_owned(),
        version: version.to_owned(),
        phase,
        path: path.to_owned(),
        change,
    })
}

fn phase(file: &str) -> Option<Phase> {
    let stem = file.strip_suffix(".py")?;
    [Phase::Pre, Phase::Post, Phase::End]
        .into_iter()
        .find(|phase| stem.starts_with(&format!("{}-", phase.as_str())))
}

/// The `version` of a `__manifest__.py`: the string after the `"version"`
/// (or `'version'`) key, as OCA and Odoo write it.
pub fn manifest_version(text: &str) -> Option<String> {
    ["\"version\"", "'version'"].iter().find_map(|key| {
        let (_, after) = text.split_once(key)?;
        let value = after.trim_start().strip_prefix(':')?.trim_start();
        let quote = ['"', '\'']
            .into_iter()
            .find(|quote| value.starts_with(*quote))?;
        match value.splitn(3, quote).collect::<Vec<_>>().as_slice() {
            [_, version, _] => Some((*version).to_owned()),
            _ => None,
        }
    })
}

/// The addon whose manifest is at `path` (`<addon>/__manifest__.py` at any
/// depth).
pub fn manifest_addon(path: &str) -> Option<&str> {
    let dir = path.strip_suffix("/__manifest__.py")?;
    Some(dir.rsplit('/').next().unwrap_or(dir))
}

/// The `__manifest__.py` of the addon a script in Odoo's layout migrates;
/// OpenUpgrade scripts live outside their addon and have none.
pub fn manifest_path(script: &Script) -> Option<String> {
    let mut parts = script.path.rsplitn(4, '/').skip(2);
    match (parts.next(), parts.next()) {
        (Some("migrations" | "upgrades"), Some(addon)) => Some(format!("{addon}/__manifest__.py")),
        _ => None,
    }
}

/// Group `scripts` and manifest `versions` (addon → before and after) by
/// addon. Addons without scripts are kept only when their version changed.
pub fn addon_changes(
    scripts: Vec<Script>,
    versions: &BTreeMap<String, (Option<String>, Option<String>)>,
) -> Vec<AddonChange> {
    let mut by_addon: BTreeMap<String, AddonChange> = versions
        .iter()
        .map(|(addon, (previous_version, version))| {
            let change = AddonChange {
                addon: addon.clone(),
                previous_version: previous_version.clone(),
                version: version.clone(),
                scripts: Vec::new(),
            };
            (addon.clone(), change)
        })
        .collect();
    for script in scripts {
        let addon = script.addon.clone();
        by_addon
            .entry(addon.clone())
            .or_insert_with(|| AddonChange {
                addon,
                previous_version: None,
                version: None,
                scripts: Vec::new(),
            })
            .scripts
            .push(script);
    }
    by_addon
        .into_values()
        .filter(|change| !change.scripts.is_empty() || change.bumped_without_scripts())
        .map(|mut change| {
            change.scripts.sort();
            change
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_in_every_layout() {
        let pre = script("pos_x/migrations/17.0.1.1.0/pre-migrate.py", Change::Added).unwrap();
        assert_eq!(
            (pre.addon.as_str(), pre.version.as_str(), pre.phase),
            ("pos_x", "17.0.1.1.0", Phase::Pre)
        );
        let post = script("setup/x/pos_x/upgrades/1.2/post-fix.py", Change::Modified).unwrap();
        assert_eq!(
            script("a/migrations/1/pre-x.py", Change::Added)
                .unwrap()
                .addon,
            "a"
        );
        assert_eq!((post.addon.as_str(), post.phase), ("pos_x", Phase::Post));
        let end = script(
            "openupgrade_scripts/scripts/sale/17.0.1.2/end-migration.py",
            Change::Added,
        )
        .unwrap();
        assert_eq!(
            (end.addon.as_str(), end.version.as_str(), end.phase),
            ("sale", "17.0.1.2", Phase::End)
        );
        for other in [
            "pos_x/migrations/17.0.1.1.0/helpers.py",
            "pos_x/migrations/17.0.1.1.0/pre-migrate.txt",
            "pos_x/models/17.0/pre-x.py",
            "scripts/sale/17.0/pre-x.py",
            "migrations/1/pre-x.py",
            "1/pre-x.py",
        ] {
            assert_eq!(script(other, Change::Added), None, "{other}");
        }
        assert_eq!(Phase::End.as_str(), "end");
        assert_eq!(
            manifest_path(&pre).as_deref(),
            Some("pos_x/__manifest__.py")
        );
        assert_eq!(
            manifest_path(&post).as_deref(),
            Some("setup/x/pos_x/__manifest__.py")
        );
        assert_eq!(manifest_path(&end), None);
    }

    #[test]
    fn manifest_versions() {
        assert_eq!(
            manifest_version("{\n    \"name\": \"X\",\n    \"version\": \"17.0.1.2.0\",\n}"),
            Some("17.0.1.2.0".to_owned())
        );
        assert_eq!(
            manifest_version("{'version' : '1.0', }"),
            Some("1.0".to_owned())
        );
        for text in [
            "{}",
            "{\"version\" \"1\"}",
            "{\"version\": 1}",
            "{\"version\": \"1",
            "{\"version\":",
        ] {
            assert_eq!(manifest_version(text), None, "{text}");
        }
        assert_eq!(manifest_addon("a/b/__manifest__.py"), Some("b"));
        assert_eq!(manifest_addon("b/__manifest__.py"), Some("b"));
        assert_eq!(manifest_addon("__manifest__.py"), None);
        assert_eq!(manifest_addon("b/x.py"), None);
    }

    #[test]
    fn changes_by_addon() {
        let scripts = vec![
            script("b/migrations/2/post-x.py", Change::Added).unwrap(),
            script("b/migrations/2/pre-x.py", Change::Added).unwrap(),
            script("c/migrations/2/pre-x.py", Change::Modified).unwrap(),
        ];
        let versions = BTreeMap::from([
            ("a".to_owned(), (Some("1".to_owned()), Some("2".to_owned()))),
            ("b".to_owned(), (Some("1".to_owned()), Some("2".to_owned()))),
            ("d".to_owned(), (Some("1".to_owned()), Some("1".to_owned()))),
            ("e".to_owned(), (None, Some("1".to_owned()))),
        ]);
        let changes = addon_changes(scripts, &versions);
        let summary: Vec<(&str, usize, bool)> = changes
            .iter()
            .map(|c| {
                (
                    c.addon.as_str(),
                    c.scripts.len(),
                    c.bumped_without_scripts(),
                )
            })
            .collect();
        assert_eq!(summary, [("a", 0, true), ("b", 2, false), ("c", 1, false)]);
        assert_eq!(changes[1].scripts[0].phase, Phase::Pre);
        assert_eq!(changes[2].version, None);
    }
}
