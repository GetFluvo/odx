//! Which addon packages go into which group directory, from `uv.lock`.

use std::collections::{BTreeMap, BTreeSet};

use odx_config::deps::normalize_name;
use odx_config::uvlock::{UvLock, UvPackage};
use odx_platform::requirements::is_odoo;

use crate::VendorError;

/// Where a package's files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemSource {
    /// A wheel from an index (or a local file), with its hash.
    Wheel {
        /// Download URL.
        url: String,
        /// `sha256:<hex>`.
        hash: String,
    },
    /// A commit of a git repository.
    Git {
        /// Repository URL.
        url: String,
        /// The commit.
        sha: String,
        /// The addon's directory in the repository.
        subdirectory: Option<String>,
    },
}

/// Repository URL, commit and subdirectory of a package from git.
pub type GitLocation = (String, String, Option<String>);

/// Where `odx.lock` locked a package from git, by distribution.
pub type GitSource<'a> = dyn Fn(&str) -> Option<GitLocation> + 'a;

/// One addon package to vendor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Dependency group (and so directory).
    pub group: String,
    /// Normalized distribution name.
    pub package: String,
    /// Resolved version.
    pub version: String,
    /// Where its files come from.
    pub source: ItemSource,
}

/// The addon packages of every group in `groups`.
///
/// Those the group declares in `uv.lock`, and the addon packages they
/// depend on that no group declares (in the first group, in name order,
/// that reaches them). `git_source` gives the locked repository, commit and
/// subdirectory of a package from git (from `odx.lock`), when known.
///
/// # Errors
///
/// When `uv.lock` has no project, misses a package, has no wheel for a
/// package from an index, or a package is in develop mode.
pub fn plan(
    lock: &UvLock,
    groups: &[String],
    git_source: &GitSource<'_>,
) -> Result<Vec<Item>, VendorError> {
    let project = lock.root().ok_or(VendorError::NoProject)?;
    let declared = |group: &str| -> Vec<String> {
        project
            .optional_dependencies
            .get(group)
            .or_else(|| project.dev_dependencies.get(group))
            .map(|deps| deps.iter().map(|dep| normalize_name(&dep.name)).collect())
            .unwrap_or_default()
    };
    let mut assigned: BTreeMap<String, String> = BTreeMap::new();
    for group in groups {
        for package in declared(group).into_iter().filter(|name| is_odoo(name)) {
            assigned.entry(package).or_insert_with(|| group.clone());
        }
    }
    let mut items = Vec::new();
    let mut seen = BTreeSet::new();
    for group in groups {
        let mut queue: Vec<String> = declared(group)
            .into_iter()
            .filter(|name| is_odoo(name) && name != "odoo")
            .collect();
        queue.reverse();
        while let Some(name) = queue.pop() {
            if assigned.get(&name).is_some_and(|owner| owner != group) || !seen.insert(name.clone())
            {
                continue;
            }
            let package = lock
                .packages_named(&name)
                .into_iter()
                .next()
                .ok_or_else(|| VendorError::MissingPackage(name.clone()))?;
            items.push(Item {
                group: group.clone(),
                package: name.clone(),
                version: package.version.clone().unwrap_or_default(),
                source: source(package, &name, git_source)?,
            });
            let mut dependencies: Vec<String> = package
                .dependencies
                .iter()
                .map(|dep| normalize_name(&dep.name))
                .filter(|dep| is_odoo(dep) && dep != "odoo")
                .collect();
            dependencies.sort();
            dependencies.reverse();
            queue.extend(dependencies);
        }
    }
    Ok(items)
}

fn source(
    package: &UvPackage,
    name: &str,
    git_source: &GitSource<'_>,
) -> Result<ItemSource, VendorError> {
    if let Some(git) = &package.source.git {
        let (url, sha, subdirectory) = git_source(name).unwrap_or_else(|| parse_git(git));
        return Ok(ItemSource::Git {
            url,
            sha,
            subdirectory,
        });
    }
    let local = &package.source;
    if local.editable.is_some() || local.directory.is_some() || local.virtual_path.is_some() {
        return Err(VendorError::Develop(name.to_owned()));
    }
    let wheel = package
        .wheels
        .iter()
        .find(|wheel| wheel.hash.is_some() && (wheel.url.is_some() || wheel.path.is_some()))
        .ok_or_else(|| VendorError::NoWheel(name.to_owned()))?;
    Ok(ItemSource::Wheel {
        url: wheel
            .url
            .clone()
            .or_else(|| wheel.path.clone())
            .unwrap_or_default(),
        hash: wheel.hash.clone().unwrap_or_default(),
    })
}

/// `<url>?subdirectory=<dir>&rev=<ref>#<commit>` from `uv.lock`.
fn parse_git(git: &str) -> GitLocation {
    let (rest, sha) = git.split_once('#').unwrap_or((git, ""));
    let (url, query) = rest.split_once('?').unwrap_or((rest, ""));
    let subdirectory = query
        .split('&')
        .find_map(|param| param.strip_prefix("subdirectory="))
        .map(|dir| dir.replace("%2F", "/"));
    (url.to_owned(), sha.to_owned(), subdirectory)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
[[package]]
name = "p"
source = { virtual = "." }

[package.optional-dependencies]
oca = [{ name = "odoo-addon-a" }, { name = "requests" }, { name = "odoo" }]
unmerged = [{ name = "odoo-addon-g" }, { name = "odoo-addon-b" }]

[package.dev-dependencies]
private = [{ name = "odoo-addon-local" }]

[[package]]
name = "odoo-addon-a"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo" }, { name = "odoo-addon-b" }, { name = "odoo-addon-c" }]
wheels = [{ url = "https://files/a.whl", hash = "sha256:aa" }]

[[package]]
name = "odoo-addon-b"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
wheels = [{ path = "b.whl", hash = "sha256:bb" }]

[[package]]
name = "odoo-addon-c"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo-addon-a" }]
wheels = [{ url = "https://files/c.whl", hash = "sha256:cc" }]

[[package]]
name = "odoo-addon-g"
version = "17.0.1.0.1"
source = { git = "https://github.com/OCA/pos.git?subdirectory=pos_g&rev=refs%2Fpull%2F1%2Fhead#0123" }

[[package]]
name = "odoo-addon-local"
version = "17.0.1.0.0"
source = { editable = "../local" }
"#;

    #[test]
    fn groups_and_their_addon_dependencies() {
        let lock = UvLock::parse(LOCK).unwrap();
        let none = |_: &str| None;
        let items = plan(&lock, &["oca".to_owned(), "unmerged".to_owned()], &none).unwrap();
        let summary: Vec<(&str, &str)> = items
            .iter()
            .map(|item| (item.group.as_str(), item.package.as_str()))
            .collect();
        // b is declared by unmerged, so it is vendored there; c only comes
        // in through a.
        assert_eq!(
            summary,
            [
                ("oca", "odoo-addon-a"),
                ("oca", "odoo-addon-c"),
                ("unmerged", "odoo-addon-g"),
                ("unmerged", "odoo-addon-b"),
            ]
        );
        assert_eq!(
            items[0].source,
            ItemSource::Wheel {
                url: "https://files/a.whl".to_owned(),
                hash: "sha256:aa".to_owned()
            }
        );
        assert_eq!(
            items[2].source,
            ItemSource::Git {
                url: "https://github.com/OCA/pos.git".to_owned(),
                sha: "0123".to_owned(),
                subdirectory: Some("pos_g".to_owned())
            }
        );
        assert_eq!(
            items[3].source,
            ItemSource::Wheel {
                url: "b.whl".to_owned(),
                hash: "sha256:bb".to_owned()
            }
        );
        // odx.lock knows where the commit was locked from.
        let locked = |name: &str| {
            (name == "odoo-addon-g")
                .then(|| ("https://up/pos.git".to_owned(), "4567".to_owned(), None))
        };
        let items = plan(&lock, &["unmerged".to_owned()], &locked).unwrap();
        assert_eq!(
            items[0].source,
            ItemSource::Git {
                url: "https://up/pos.git".to_owned(),
                sha: "4567".to_owned(),
                subdirectory: None
            }
        );
        assert_eq!(
            plan(&lock, &["private".to_owned()], &none)
                .unwrap_err()
                .to_string(),
            "odoo-addon-local: in develop mode (a local path source); restore it before vendoring"
        );
        assert_eq!(plan(&lock, &["missing".to_owned()], &none).unwrap(), []);
    }

    #[test]
    fn broken_locks() {
        let none = |_: &str| None;
        assert_eq!(
            plan(&UvLock::default(), &[], &none)
                .unwrap_err()
                .to_string(),
            "uv.lock has no project package; run `uv lock`"
        );
        let missing = UvLock::parse(
            "[[package]]\nname = \"p\"\nsource = { virtual = \".\" }\n[package.optional-dependencies]\noca = [{ name = \"odoo-addon-x\" }]\n",
        )
        .unwrap();
        assert_eq!(
            plan(&missing, &["oca".to_owned()], &none)
                .unwrap_err()
                .to_string(),
            "odoo-addon-x is not in uv.lock; run `uv lock`"
        );
        let wheelless = UvLock::parse(
            "[[package]]\nname = \"p\"\nsource = { virtual = \".\" }\n[package.optional-dependencies]\noca = [{ name = \"odoo-addon-x\" }]\n\
             [[package]]\nname = \"odoo-addon-x\"\nversion = \"1\"\nsource = { registry = \"r\" }\n",
        )
        .unwrap();
        assert_eq!(
            plan(&wheelless, &["oca".to_owned()], &none)
                .unwrap_err()
                .to_string(),
            "odoo-addon-x: uv.lock has no wheel for it"
        );
        assert_eq!(
            parse_git("https://x/y.git"),
            ("https://x/y.git".to_owned(), String::new(), None)
        );
    }
}
