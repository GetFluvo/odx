//! Provided-package sets: what a hosting platform already installs.
//!
//! Declaring one of these again in the platform requirements, or pinning it
//! differently, makes deploys fail or behave unpredictably, so odx leaves
//! them out and warns when an addon asks for a version the platform does
//! not provide.

use std::collections::BTreeMap;
use std::str::FromStr;

use pep440_rs::{Version, VersionSpecifiers};

use crate::normalize_name;

/// Odoo's own `requirements.txt` per series, as shipped with odx.
const ODOO_STANDARD: &[(&str, &str)] = &[
    (
        "16.0",
        include_str!("../data/provided/odoo-standard-16.0.txt"),
    ),
    (
        "17.0",
        include_str!("../data/provided/odoo-standard-17.0.txt"),
    ),
    (
        "18.0",
        include_str!("../data/provided/odoo-standard-18.0.txt"),
    ),
    (
        "19.0",
        include_str!("../data/provided/odoo-standard-19.0.txt"),
    ),
];

/// The named sets odx ships, for messages.
pub const NAMED_SETS: &[&str] = &["odoo-standard", "odoo-sh"];

/// Packages a platform provides, with the versions it pins (none when it
/// installs whatever is current).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvidedSet {
    /// What the set is, for reports: `odoo-standard-17.0`, a file name.
    pub label: String,
    packages: BTreeMap<String, Vec<String>>,
}

impl ProvidedSet {
    /// The named set `name` for Odoo `series`: `odoo-standard` (Odoo's
    /// `requirements.txt`) or `odoo-sh`, which is the same until the
    /// platform image's additions are collected; add those with
    /// `provided_extra`.
    pub fn named(name: &str, series: &str) -> Option<Self> {
        if !NAMED_SETS.contains(&name) {
            return None;
        }
        let (_, text) = ODOO_STANDARD.iter().find(|(known, _)| *known == series)?;
        Some(Self::parse(&format!("{name}-{series}"), text))
    }

    /// A set from requirement lines (pip constraints format): `name`,
    /// `name==version`, with or without `; markers`; comments and options
    /// are ignored.
    pub fn parse(label: &str, text: &str) -> Self {
        let mut set = Self {
            label: label.to_owned(),
            packages: BTreeMap::new(),
        };
        for line in text.lines() {
            set.add(line);
        }
        set
    }

    /// Add one requirement line (`provided_extra`).
    pub fn add(&mut self, line: &str) {
        let line = line.split('#').next().unwrap_or_default();
        let body = line.split(';').next().unwrap_or_default().trim();
        if body.is_empty() || body.starts_with('-') {
            return;
        }
        let end = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(body.len());
        let (name, rest) = body.split_at(end);
        let versions = self.packages.entry(normalize_name(name)).or_default();
        if let Some(version) = rest.trim().strip_prefix("==") {
            versions.push(version.trim().to_owned());
        }
    }

    /// Remove a package (`provided_remove`).
    pub fn remove(&mut self, name: &str) {
        self.packages.remove(&normalize_name(name));
    }

    /// Whether the platform provides `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(&normalize_name(name))
    }

    /// The versions the platform pins for `name` (one per Python version
    /// it supports), empty when it pins none.
    pub fn versions(&self, name: &str) -> &[String] {
        self.packages
            .get(&normalize_name(name))
            .map_or(&[], Vec::as_slice)
    }

    /// Whether `specifier` (`>=2.31,<3`) excludes every version the
    /// platform pins for `name`: then the addon that asks for it cannot
    /// work on the platform. Unpinned packages and unparsable specifiers
    /// never conflict.
    pub fn excludes(&self, name: &str, specifier: &str) -> bool {
        let Ok(specifiers) = VersionSpecifiers::from_str(specifier) else {
            return false;
        };
        let versions: Vec<Version> = self
            .versions(name)
            .iter()
            .filter_map(|version| Version::from_str(version).ok())
            .collect();
        !versions.is_empty() && !versions.iter().any(|version| specifiers.contains(version))
    }

    /// Number of packages in the set.
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_sets_per_series() {
        for series in ["16.0", "17.0", "18.0", "19.0"] {
            let set = ProvidedSet::named("odoo-standard", series).unwrap();
            assert!(set.contains("lxml"), "{series}");
            assert!(set.contains("psycopg2"), "{series}");
            assert!(!set.contains("odoo-addon-web-responsive"));
            assert!(set.len() > 40, "{series}");
        }
        let sh = ProvidedSet::named("odoo-sh", "17.0").unwrap();
        assert_eq!(sh.label, "odoo-sh-17.0");
        assert_eq!(ProvidedSet::named("odoo-standard", "12.0"), None);
        assert_eq!(ProvidedSet::named("nope", "17.0"), None);
    }

    #[test]
    fn versions_and_conflicts() {
        let mut set = ProvidedSet::parse(
            "file",
            "# comment\n-c other.txt\n\nBabel==2.9.1 ; python_version < '3.11'\n\
             babel==2.10.3 ; python_version >= '3.11'\npytz  # whatever is current\n",
        );
        assert_eq!(set.versions("babel"), ["2.9.1", "2.10.3"]);
        assert_eq!(set.versions("pytz"), [] as [String; 0]);
        assert_eq!(set.versions("requests"), [] as [String; 0]);
        assert!(!set.excludes("babel", ">=2.10"));
        assert!(set.excludes("babel", ">=2.11"));
        assert!(!set.excludes("pytz", "==2020.1"));
        assert!(!set.excludes("babel", "not a specifier"));
        set.add("requests==2.31.0");
        assert!(set.contains("Requests"));
        set.remove("REQUESTS");
        assert!(!set.contains("requests"));
        assert!(!set.is_empty());
        assert!(ProvidedSet::default().is_empty());
    }
}
