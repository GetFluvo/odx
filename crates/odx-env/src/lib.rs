//! Environment comparison for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). An environment is seen as
//! a set of [`Unit`]s: the project's own addons, the packages `uv.lock`
//! resolves, the repositories and the Odoo core `odx.lock` pins. [`diff`]
//! compares two such sides unit by unit; `odx env diff`, promotion pull
//! requests and the dashboard's drift section print the result.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::str::FromStr as _;

use odx_config::lock::Lockfile;
use odx_config::uvlock::{UvLock, UvPackage};

pub mod copy;
pub mod promote;
pub mod search;

/// What a unit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The Odoo core.
    Odoo,
    /// An addon committed in the project.
    Addon,
    /// A package `uv.lock` resolves.
    Package,
    /// A repository `odx.lock` aggregates.
    Repository,
}

impl Kind {
    /// Its name in JSON output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Odoo => "odoo",
            Self::Addon => "addon",
            Self::Package => "package",
            Self::Repository => "repository",
        }
    }
}

/// One thing an environment runs, and its exact pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// What it is.
    pub kind: Kind,
    /// Addon, package or repository name.
    pub name: String,
    /// Its version, when it has one.
    pub version: Option<String>,
    /// Where it comes from: `project`, `index`, `git <url>`, a URL.
    pub source: String,
    /// What identifies its content: a wheel hash, a commit, a tree.
    pub pin: String,
}

/// How a unit differs between the two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    /// The same pin on both sides.
    Identical,
    /// A newer version on the first side.
    Ahead,
    /// An older version on the first side.
    Behind,
    /// Different pins, no telling which is newer.
    Changed,
    /// From another source on each side.
    SourceChanged,
    /// Only on the first side.
    OnlyInA,
    /// Only on the second side.
    OnlyInB,
}

impl Change {
    /// Its name in JSON output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Ahead => "ahead",
            Self::Behind => "behind",
            Self::Changed => "changed",
            Self::SourceChanged => "source-changed",
            Self::OnlyInA => "only-in-a",
            Self::OnlyInB => "only-in-b",
        }
    }
}

/// One unit on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What it is.
    pub kind: Kind,
    /// Its name.
    pub name: String,
    /// How it differs.
    pub change: Change,
    /// On the first side.
    pub a: Option<Unit>,
    /// On the second side.
    pub b: Option<Unit>,
}

/// Compare two sides: every unit of either, by kind and name.
#[must_use]
pub fn diff(a: &[Unit], b: &[Unit]) -> Vec<Entry> {
    type Sides<'a> = (Option<&'a Unit>, Option<&'a Unit>);
    let mut both: BTreeMap<(Kind, &str), Sides<'_>> = BTreeMap::new();
    for unit in a {
        both.entry((unit.kind, &unit.name)).or_default().0 = Some(unit);
    }
    for unit in b {
        both.entry((unit.kind, &unit.name)).or_default().1 = Some(unit);
    }
    both.into_iter()
        .map(|((kind, name), (a, b))| Entry {
            kind,
            name: name.to_owned(),
            change: compare(a, b),
            a: a.cloned(),
            b: b.cloned(),
        })
        .collect()
}

/// How `b` differs from `a`.
#[must_use]
pub fn compare(a: Option<&Unit>, b: Option<&Unit>) -> Change {
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        (Some(_), None) => return Change::OnlyInA,
        _ => return Change::OnlyInB,
    };
    if a.source != b.source {
        return Change::SourceChanged;
    }
    if a.pin == b.pin {
        return Change::Identical;
    }
    match a
        .version
        .as_deref()
        .zip(b.version.as_deref())
        .and_then(|(a, b)| compare_versions(a, b))
    {
        Some(Ordering::Greater) => Change::Ahead,
        Some(Ordering::Less) => Change::Behind,
        _ => Change::Changed,
    }
}

/// `a` against `b` as PEP 440 versions (manifest versions are too), none
/// when either is no version.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Option<Ordering> {
    let a = pep440_rs::Version::from_str(a).ok()?;
    let b = pep440_rs::Version::from_str(b).ok()?;
    Some(a.cmp(&b))
}

/// The packages `uv.lock` resolves, the project itself left out; of a
/// package resolved more than once, the first.
#[must_use]
pub fn packages(lock: &UvLock) -> Vec<Unit> {
    let mut units: Vec<Unit> = Vec::new();
    for package in &lock.packages {
        if package.source.virtual_path.as_deref() == Some(".")
            || package.source.editable.as_deref() == Some(".")
            || units.iter().any(|unit| unit.name == package.name)
        {
            continue;
        }
        let (source, pin) = origin(package);
        units.push(Unit {
            kind: Kind::Package,
            name: package.name.clone(),
            version: package.version.clone(),
            source,
            pin,
        });
    }
    units
}

/// Where `package` comes from, and what pins its content.
fn origin(package: &UvPackage) -> (String, String) {
    let source = &package.source;
    let local = [
        &source.virtual_path,
        &source.editable,
        &source.directory,
        &source.path,
    ]
    .into_iter()
    .flatten()
    .next();
    match (&source.git, &source.url, local) {
        (Some(git), ..) => {
            let (url, commit) = git.split_once('#').unwrap_or((git, ""));
            let url = url.split_once('?').map_or(url, |(url, _)| url);
            (format!("git {url}"), commit.to_owned())
        }
        (None, Some(url), _) => (
            url.clone(),
            wheel_hash(package).unwrap_or_else(|| url.clone()),
        ),
        (None, None, Some(path)) => (format!("local {path}"), path.clone()),
        (None, None, None) => (
            "index".to_owned(),
            wheel_hash(package).unwrap_or_else(|| package.version.clone().unwrap_or_default()),
        ),
    }
}

fn wheel_hash(package: &UvPackage) -> Option<String> {
    package.wheels.iter().find_map(|wheel| wheel.hash.clone())
}

/// The repositories and the Odoo core `odx.lock` pins: a repository by the
/// tree it builds, so the same content from other commits is identical.
#[must_use]
pub fn locked(lock: &Lockfile) -> Vec<Unit> {
    let odoo = lock.odoo.iter().map(|odoo| Unit {
        kind: Kind::Odoo,
        name: "odoo".to_owned(),
        version: None,
        source: odoo.url.clone(),
        pin: odoo.rev.clone(),
    });
    let repositories = lock.repositories.iter().map(|repo| Unit {
        kind: Kind::Repository,
        name: repo.name.clone(),
        version: None,
        source: repo.url.clone(),
        pin: repo.result_tree.clone(),
    });
    odoo.chain(repositories).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(kind: Kind, name: &str, version: Option<&str>, source: &str, pin: &str) -> Unit {
        Unit {
            kind,
            name: name.to_owned(),
            version: version.map(str::to_owned),
            source: source.to_owned(),
            pin: pin.to_owned(),
        }
    }

    #[test]
    fn changes() {
        let a = [
            unit(Kind::Addon, "same", Some("19.0.1.0.0"), "project", "t1"),
            unit(Kind::Addon, "newer", Some("19.0.1.1.0"), "project", "t2"),
            unit(Kind::Addon, "older", Some("19.0.1.0.0"), "project", "t3"),
            unit(Kind::Addon, "edited", Some("19.0.1.0.0"), "project", "t4"),
            unit(Kind::Package, "moved", Some("1.0"), "index", "h1"),
            unit(Kind::Package, "mine", Some("1.0"), "index", "h2"),
            unit(Kind::Repository, "odd", None, "u", "x"),
        ];
        let b = [
            unit(Kind::Addon, "same", Some("19.0.1.0.0"), "project", "t1"),
            unit(Kind::Addon, "newer", Some("19.0.1.0.0"), "project", "t5"),
            unit(Kind::Addon, "older", Some("19.0.1.0.1"), "project", "t6"),
            unit(Kind::Addon, "edited", Some("19.0.1.0.0"), "project", "t7"),
            unit(Kind::Package, "moved", Some("1.0"), "git u", "c1"),
            unit(Kind::Package, "theirs", Some("1.0"), "index", "h3"),
            unit(Kind::Repository, "odd", None, "u", "y"),
        ];
        let entries = diff(&a, &b);
        let found: Vec<(&str, &str)> = entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.change.name()))
            .collect();
        assert_eq!(
            found,
            [
                ("edited", "changed"),
                ("newer", "ahead"),
                ("older", "behind"),
                ("same", "identical"),
                ("mine", "only-in-a"),
                ("moved", "source-changed"),
                ("theirs", "only-in-b"),
                ("odd", "changed"),
            ]
        );
        assert_eq!(
            compare_versions("1.0+odx.abc", "1.0"),
            Some(Ordering::Greater)
        );
        assert_eq!(compare_versions("x", "1.0"), None);
        assert_eq!(compare_versions("1.0", "x"), None);
        let names: Vec<&str> = [Kind::Odoo, Kind::Addon, Kind::Package, Kind::Repository]
            .map(Kind::name)
            .to_vec();
        assert_eq!(names, ["odoo", "addon", "package", "repository"]);
        let changes: Vec<&str> = [
            Change::Identical,
            Change::Ahead,
            Change::Behind,
            Change::Changed,
            Change::SourceChanged,
            Change::OnlyInA,
            Change::OnlyInB,
        ]
        .map(Change::name)
        .to_vec();
        assert_eq!(changes.len(), 7);
    }

    #[test]
    fn units_of_lockfiles() {
        let uv = UvLock::parse(
            r#"
[[package]]
name = "p"
version = "0.1"
source = { virtual = "." }

[[package]]
name = "e"
version = "0.1"
source = { editable = "." }

[[package]]
name = "odoo-addon-a"
version = "19.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
wheels = [{ url = "https://x/a.whl", hash = "sha256:aa" }]

[[package]]
name = "odoo-addon-a"
version = "19.0.0.9.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo-addon-b"
version = "19.0.1.0.0"
source = { git = "https://github.com/OCA/b?rev=abc#abc123" }

[[package]]
name = "odoo-addon-c"
version = "19.0.1.0.0"
source = { url = "https://x/c.whl" }
wheels = [{ url = "https://x/c.whl", hash = "sha256:cc" }]

[[package]]
name = "odoo-addon-d"
version = "19.0.1.0.0"
source = { url = "https://x/d.whl" }

[[package]]
name = "odoo-addon-l"
version = "19.0.1.0.0"
source = { directory = "../l" }

[[package]]
name = "nohash"
version = "2.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "g"
version = "1"
source = { git = "https://github.com/x/g" }

[[package]]
name = "noversion"
source = { registry = "https://pypi.org/simple" }
"#,
        )
        .unwrap();
        let units = packages(&uv);
        let found: Vec<(&str, &str, &str)> = units
            .iter()
            .map(|unit| (unit.name.as_str(), unit.source.as_str(), unit.pin.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("odoo-addon-a", "index", "sha256:aa"),
                ("odoo-addon-b", "git https://github.com/OCA/b", "abc123"),
                ("odoo-addon-c", "https://x/c.whl", "sha256:cc"),
                ("odoo-addon-d", "https://x/d.whl", "https://x/d.whl"),
                ("odoo-addon-l", "local ../l", "../l"),
                ("nohash", "index", "2.0"),
                ("g", "git https://github.com/x/g", ""),
                ("noversion", "index", ""),
            ]
        );
        let mut lock = Lockfile::new();
        lock.odoo = Some(odx_config::lock::OdooPin {
            url: "https://github.com/odoo/odoo".to_owned(),
            rev: "c0ffee".to_owned(),
        });
        lock.repositories.push(odx_config::lock::LockedRepository {
            name: "pos".to_owned(),
            url: "https://github.com/OCA/pos".to_owned(),
            base: "19.0".to_owned(),
            base_sha: "1111111".to_owned(),
            result_tree: "2222222".to_owned(),
            merges: Vec::new(),
        });
        let units = locked(&lock);
        let found: Vec<(Kind, &str, &str)> = units
            .iter()
            .map(|unit| (unit.kind, unit.name.as_str(), unit.pin.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                (Kind::Odoo, "odoo", "c0ffee"),
                (Kind::Repository, "pos", "2222222")
            ]
        );
    }
}
