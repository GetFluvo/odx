//! The project's addon packages that come from git: `[tool.odx.packages]`
//! and git direct references in the configured dependency groups.

use std::collections::BTreeMap;

use toml_edit::DocumentMut;

use crate::OdxConfig;
use crate::Problem;
use crate::deps::{normalize_name, read_group};
use crate::lock::{LockSource, LockedPackage};
use odx_core::repo_key;

/// Where a package is declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageOrigin {
    /// `[tool.odx.packages]`; `odx lock` writes its `[tool.uv.sources]`
    /// entry.
    Table,
    /// A git direct reference in this group; `odx lock` rewrites it.
    DirectReference(String),
}

/// A package to lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    /// Distribution name as written.
    pub dist: String,
    /// The dependency group it belongs to, if known.
    pub group: Option<String>,
    /// The upstream repository URL.
    pub url: String,
    /// What to resolve in it.
    pub source: LockSource,
    /// Subdirectory of the addon in the repository.
    pub subdirectory: Option<String>,
    /// Where it is declared.
    pub origin: PackageOrigin,
}

fn source(
    pr: Option<u32>,
    sha: Option<&str>,
    branch: Option<&str>,
    tag: Option<&str>,
) -> LockSource {
    LockSource {
        pr,
        sha: sha.map(str::to_owned),
        branch: branch.map(str::to_owned),
        remote: None,
        tag: tag.map(str::to_owned),
    }
}

/// The source a direct reference's `@<ref>` stands for: `refs/pull/N/head`
/// (or `pull/N/head`) is a pull request, 40 or 64 hexadecimal digits a
/// commit, anything else a branch or tag name.
fn reference_source(reference: &str) -> LockSource {
    let pr = reference
        .trim_start_matches("refs/")
        .strip_prefix("pull/")
        .and_then(|rest| rest.strip_suffix("/head"))
        .and_then(|n| n.parse().ok());
    let hex =
        matches!(reference.len(), 40 | 64) && reference.bytes().all(|b| b.is_ascii_hexdigit());
    match (pr, hex) {
        (Some(pr), _) => source(Some(pr), None, None, None),
        (None, true) => source(None, Some(reference), None, None),
        (None, false) => source(None, None, Some(reference), None),
    }
}

/// A URL uv accepts after `git+`: scp-like `user@host:path` becomes
/// `ssh://user@host/path`.
///
/// ```
/// use odx_config::packages::uv_git_url;
///
/// assert_eq!(uv_git_url("git@forge:odoo/vault.git"), "ssh://git@forge/odoo/vault.git");
/// assert_eq!(uv_git_url("https://forge/odoo/vault.git"), "https://forge/odoo/vault.git");
/// ```
pub fn uv_git_url(url: &str) -> String {
    match url.split_once(':') {
        Some((authority, path)) if !url.contains("://") && authority.contains('@') => {
            format!("ssh://{authority}/{}", path.trim_start_matches('/'))
        }
        _ => url.to_owned(),
    }
}

/// The git packages of the project in `doc`.
///
/// `[tool.odx.packages]` come first, then git direct references in the
/// groups of `[tool.odx.groups]`, by group. A direct reference that already points at the vault
/// (`<mirror>@odx/<sha>`) stands for the source `locked` records for it.
/// Direct references without a ref are reported and skipped.
pub fn package_specs(
    doc: &DocumentMut,
    config: &OdxConfig,
    locked: &BTreeMap<String, LockedPackage>,
) -> (Vec<PackageSpec>, Vec<Problem>) {
    let default_group = config
        .groups
        .as_ref()
        .and_then(|groups| groups.default.clone());
    let mut specs: Vec<PackageSpec> = config
        .packages
        .iter()
        .map(|(dist, package)| PackageSpec {
            dist: dist.clone(),
            group: package.group.clone().or_else(|| default_group.clone()),
            url: package.url.clone().unwrap_or_else(|| {
                format!(
                    "https://github.com/{}.git",
                    package.repo.as_deref().unwrap_or_default()
                )
            }),
            source: source(
                package.pr,
                package.rev.as_deref(),
                package.branch.as_deref(),
                package.tag.as_deref(),
            ),
            subdirectory: package.subdirectory.clone(),
            origin: PackageOrigin::Table,
        })
        .collect();
    let mirror = config.mirror.as_ref().map(|mirror| repo_key(&mirror.url));
    let mut problems = Vec::new();
    let names = config.groups.iter().flat_map(|groups| groups.dirs.keys());
    for group in names.filter_map(|name| read_group(doc, name)) {
        let direct = group
            .requirements
            .iter()
            .filter_map(|req| req.direct.as_ref().map(|direct| (req, direct)))
            .filter(|(_, direct)| direct.raw.starts_with("git+"));
        for (req, direct) in direct {
            let Some(reference) = direct.reference.as_deref() else {
                problems.push(Problem {
                    key: format!("{}: {}", group.name, req.name),
                    message: "a git direct reference needs a branch, tag or commit after `@`"
                        .to_owned(),
                });
                continue;
            };
            let vaulted = reference
                .strip_prefix("odx/")
                .filter(|_| mirror.as_deref() == Some(repo_key(&direct.repository).as_str()))
                .and_then(|sha| {
                    locked
                        .iter()
                        .find(|(name, entry)| {
                            normalize_name(name) == normalize_name(&req.name) && entry.sha == sha
                        })
                        .map(|(_, entry)| entry)
                });
            let (url, source) = vaulted.map_or_else(
                || (direct.repository.clone(), reference_source(reference)),
                |entry| (entry.url.clone(), entry.source.clone()),
            );
            specs.push(PackageSpec {
                dist: req.name.clone(),
                group: Some(group.name.clone()),
                url,
                source,
                subdirectory: direct.subdirectory.clone(),
                origin: PackageOrigin::DirectReference(group.name.clone()),
            });
        }
    }
    (specs, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::LockedPackage;
    use std::path::Path;

    const PYPROJECT: &str = r#"
[project]
name = "client"
version = "1"

[project.optional-dependencies]
oca = ["odoo-addon-web_responsive==17.0.1.2.0"]
oca_unmerged = [
  "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
  "odoo-addon-pos_fix @ git+https://github.com/OCA/pos.git@17.0-fix#subdirectory=pos_fix",
  "odoo-addon-pos_pinned @ git+https://github.com/OCA/pos.git@0123456789abcdef0123456789abcdef01234567#subdirectory=pos_pinned",
  "odoo-addon-pos_vaulted @ git+ssh://git@forge.example/odoo/vault.git@odx/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa#subdirectory=pos_vaulted",
  "odoo-addon-pos_unvaulted @ git+https://forge.example/odoo/vault.git@odx/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "odoo-addon-pos_norev @ git+https://github.com/OCA/pos.git#subdirectory=pos_norev",
  "odoo-addon-pos_archive @ https://example.com/pos.zip",
]

[tool.odx]
odoo_version = "17.0"

[tool.odx.mirror]
url = "git@forge.example:odoo/vault.git"
ref_style = "tags"

[tool.odx.groups]
oca = "modules/oca"
oca_unmerged = "modules/oca_unmerged"
missing = "modules/missing"
default = "oca"

[tool.odx.packages]
odoo-addon-private = { url = "https://forge.example/odoo/private.git", branch = "17.0", subdirectory = "private" }
odoo-addon-gh = { repo = "OCA/server-tools", tag = "v1", group = "oca_unmerged" }
odoo-addon-rev = { repo = "OCA/web", rev = "abc", group = "oca" }
odoo-addon-pr = { repo = "OCA/web", pr = 7, group = "oca" }
"#;

    #[test]
    #[expect(clippy::too_many_lines, reason = "one fixture, every kind of package")]
    fn packages_from_the_table_and_direct_references() {
        let config = crate::parse_pyproject(PYPROJECT, Path::new("pyproject.toml"))
            .unwrap()
            .unwrap()
            .config;
        let doc: DocumentMut = PYPROJECT.parse().unwrap();
        let mut locked = BTreeMap::new();
        locked.insert(
            "odoo_addon.pos-vaulted".to_owned(),
            LockedPackage {
                group: Some("oca_unmerged".into()),
                url: "https://github.com/OCA/pos.git".into(),
                source: source(Some(99), None, None, None),
                subdirectory: Some("pos_vaulted".into()),
                sha: "a".repeat(40),
                mirror_ref: Some("refs/tags/odx/aaaa".into()),
            },
        );
        let (specs, problems) = package_specs(&doc, &config, &locked);
        let summary: Vec<(&str, Option<&str>, &str, String)> = specs
            .iter()
            .map(|spec| {
                (
                    spec.dist.as_str(),
                    spec.group.as_deref(),
                    spec.url.as_str(),
                    format!(
                        "{:?}",
                        (
                            spec.source.pr,
                            &spec.source.sha,
                            &spec.source.branch,
                            &spec.source.tag
                        )
                    ),
                )
            })
            .collect();
        let fmt = |pr: Option<u32>, sha: Option<&str>, branch: Option<&str>, tag: Option<&str>| {
            format!(
                "{:?}",
                (
                    pr,
                    sha.map(str::to_owned),
                    branch.map(str::to_owned),
                    tag.map(str::to_owned)
                )
            )
        };
        assert_eq!(
            summary,
            [
                (
                    "odoo-addon-gh",
                    Some("oca_unmerged"),
                    "https://github.com/OCA/server-tools.git",
                    fmt(None, None, None, Some("v1"))
                ),
                (
                    "odoo-addon-pr",
                    Some("oca"),
                    "https://github.com/OCA/web.git",
                    fmt(Some(7), None, None, None)
                ),
                (
                    "odoo-addon-private",
                    Some("oca"),
                    "https://forge.example/odoo/private.git",
                    fmt(None, None, Some("17.0"), None)
                ),
                (
                    "odoo-addon-rev",
                    Some("oca"),
                    "https://github.com/OCA/web.git",
                    fmt(None, Some("abc"), None, None)
                ),
                (
                    "odoo-addon-pos_vouchers",
                    Some("oca_unmerged"),
                    "https://github.com/OCA/pos.git",
                    fmt(Some(45), None, None, None)
                ),
                (
                    "odoo-addon-pos_fix",
                    Some("oca_unmerged"),
                    "https://github.com/OCA/pos.git",
                    fmt(None, None, Some("17.0-fix"), None)
                ),
                (
                    "odoo-addon-pos_pinned",
                    Some("oca_unmerged"),
                    "https://github.com/OCA/pos.git",
                    fmt(
                        None,
                        Some("0123456789abcdef0123456789abcdef01234567"),
                        None,
                        None
                    )
                ),
                (
                    "odoo-addon-pos_vaulted",
                    Some("oca_unmerged"),
                    "https://github.com/OCA/pos.git",
                    fmt(Some(99), None, None, None)
                ),
                (
                    "odoo-addon-pos_unvaulted",
                    Some("oca_unmerged"),
                    "https://forge.example/odoo/vault.git",
                    fmt(
                        None,
                        None,
                        Some("odx/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                        None
                    )
                ),
            ]
        );
        assert_eq!(specs[4].subdirectory.as_deref(), Some("pos_vouchers"));
        assert_eq!(
            specs[4].origin,
            PackageOrigin::DirectReference("oca_unmerged".into())
        );
        assert_eq!(specs[0].origin, PackageOrigin::Table);
        assert_eq!(
            problems.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "oca_unmerged: odoo-addon-pos_norev: a git direct reference needs a branch, tag or commit after `@`"
            ]
        );
    }

    #[test]
    fn references_and_urls() {
        assert_eq!(reference_source("pull/3/head").pr, Some(3));
        assert_eq!(
            reference_source("refs/pull/x/head").branch.as_deref(),
            Some("refs/pull/x/head")
        );
        assert_eq!(reference_source(&"f".repeat(64)).sha, Some("f".repeat(64)));
        assert_eq!(uv_git_url("ssh://git@forge/a.git"), "ssh://git@forge/a.git");
        assert_eq!(uv_git_url("/srv/vault.git"), "/srv/vault.git");
        let config =
            crate::parse_pyproject("[tool.odx]\nodoo_version = \"17.0\"\n", Path::new("p.toml"))
                .unwrap()
                .unwrap()
                .config;
        assert_eq!(
            package_specs(&DocumentMut::new(), &config, &BTreeMap::new()),
            (Vec::new(), Vec::new())
        );
    }
}
