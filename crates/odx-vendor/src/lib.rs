//! Vendoring of locked Odoo addon packages into addon directories for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `odx vendor` writes the
//! addon directories of every dependency group from the locked wheels (hash
//! checked) and commits, records what it wrote in the vendor manifest, and
//! bumps an addon's version when its content changed without one, so the
//! platform updates it.

pub mod archive;
pub mod content;
pub mod plan;
pub mod record;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use odx_analyze::manifest::parse_manifest;
use odx_forge::Http;

use crate::archive::{extract, fetch, subdirectory_entry, wheel_entry};
use crate::content::{bump, hash_files, read_tree, set_manifest_version, tree_hash};

const MANIFEST: &str = "__manifest__.py";
use crate::plan::{Item, ItemSource};
use crate::record::{VERSION, VendorRecord, VendoredAddon};

/// Why vendoring failed.
#[derive(Debug, thiserror::Error)]
pub enum VendorError {
    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// A wheel could not be downloaded.
    #[error("cannot download {url}: {message}")]
    Download {
        /// The URL.
        url: String,
        /// What went wrong.
        message: String,
    },
    /// A wheel's hash differs from `uv.lock`.
    #[error("{url}: hash mismatch: expected {expected}, got {actual}")]
    Hash {
        /// The URL.
        url: String,
        /// The hash in `uv.lock`.
        expected: String,
        /// The hash of what arrived.
        actual: String,
    },
    /// A wheel or archive is broken or unsafe.
    #[error("{what}: {message}")]
    Archive {
        /// The wheel or archive.
        what: String,
        /// What is wrong.
        message: String,
    },
    /// `uv.lock` has no project.
    #[error("uv.lock has no project package; run `uv lock`")]
    NoProject,
    /// A declared package is not in `uv.lock`.
    #[error("{0} is not in uv.lock; run `uv lock`")]
    MissingPackage(String),
    /// A package from an index has no wheel in `uv.lock`.
    #[error("{0}: uv.lock has no wheel for it")]
    NoWheel(String),
    /// A package points at a local directory.
    #[error("{0}: in develop mode (a local path source); restore it before vendoring")]
    Develop(String),
    /// A package from git could not be exported.
    #[error("{package}: {message}")]
    Git {
        /// The package.
        package: String,
        /// What went wrong.
        message: String,
    },
    /// A group directory odx must not replace.
    #[error("{}: odx vendor replaces this directory, so it must be inside the project and hold no project files", .0.display())]
    UnsafeDirectory(PathBuf),
}

/// A zip of commit `sha` of repository `url`, limited to a subdirectory.
pub type GitArchive<'a> = dyn Fn(&str, &str, Option<&str>) -> Result<Vec<u8>, String> + 'a;

/// What vendoring needs besides the plan.
#[derive(Debug)]
pub struct Options<'a> {
    /// The project root.
    pub root: &'a Path,
    /// Group → directory (absolute).
    pub group_dirs: &'a BTreeMap<String, PathBuf>,
    /// Wheel cache directory.
    pub wheel_cache: &'a Path,
    /// The previous vendor manifest.
    pub previous: &'a VendorRecord,
    /// Bump the version of addons whose content changed alone.
    pub bump: bool,
}

/// A version bump `odx vendor` made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bumped {
    /// The addon.
    pub addon: String,
    /// The version shipped.
    pub from: String,
    /// The version written.
    pub to: String,
}

/// What `odx vendor` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vendored {
    /// The new vendor manifest.
    pub record: VendorRecord,
    /// Versions bumped because content changed alone.
    pub bumped: Vec<Bumped>,
    /// Addons whose content changed alone but whose version cannot be
    /// bumped (no numeric last segment).
    pub not_bumped: Vec<String>,
}

/// Write every group directory from `items`.
///
/// The directory is replaced with the addons of its packages: wheels from
/// `http` (or the cache), git packages from `git_archive` (a zip of the
/// commit's subdirectory).
///
/// # Errors
///
/// When a group directory is unsafe to replace, a wheel or commit cannot
/// be fetched, or a file cannot be written.
pub fn vendor(
    items: &[Item],
    options: &Options<'_>,
    http: &dyn Http,
    git_archive: &GitArchive<'_>,
    exclude: &dyn Fn(&str) -> bool,
) -> Result<Vendored, VendorError> {
    let root = normalize(options.root);
    for dir in options.group_dirs.values() {
        let dir = normalize(dir);
        let inside = dir.starts_with(&root) && dir != root;
        let owned = ["pyproject.toml", ".git"]
            .iter()
            .all(|name| !dir.join(name).exists());
        if !inside || !owned {
            return Err(VendorError::UnsafeDirectory(dir));
        }
    }
    for dir in options.group_dirs.values() {
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(io_at(dir))?;
        }
        std::fs::create_dir_all(dir).map_err(io_at(dir))?;
    }
    let mut result = Vendored {
        record: VendorRecord {
            version: VERSION,
            addons: Vec::new(),
        },
        ..Vendored::default()
    };
    for (group, dir) in options.group_dirs {
        for item in items.iter().filter(|item| &item.group == group) {
            let (bytes, what, subdirectory, source, wheel) = match &item.source {
                ItemSource::Wheel { url, hash } => (
                    fetch(http, options.wheel_cache, url, hash)?,
                    url.rsplit('/').next().unwrap_or(url).to_owned(),
                    None,
                    item.version.clone(),
                    Some(hash.clone()),
                ),
                ItemSource::Git {
                    url,
                    sha,
                    subdirectory,
                } => (
                    git_archive(url, sha, subdirectory.as_deref()).map_err(|message| {
                        VendorError::Git {
                            package: item.package.clone(),
                            message,
                        }
                    })?,
                    item.package.clone(),
                    Some(subdirectory.clone().unwrap_or_default()),
                    format!("git {sha}"),
                    None,
                ),
            };
            let select = |name: &str| {
                subdirectory.as_deref().map_or_else(
                    || wheel_entry(name),
                    |subdirectory| subdirectory_entry(subdirectory, name),
                )
            };
            let entries = extract(&bytes, &what, &select, exclude, dir).and_then(|addons| {
                addons
                    .iter()
                    .map(|addon| record_addon(&dir.join(addon), item, addon, options, &mut result))
                    .collect::<Result<Vec<_>, _>>()
            })?;
            for entry in entries {
                result.record.addons.push(VendoredAddon {
                    source: source.clone(),
                    wheel: wheel.clone(),
                    ..entry
                });
            }
        }
    }
    result
        .record
        .addons
        .sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));
    Ok(result)
}

/// Turns an IO failure at `path` into a [`VendorError`].
fn io_at(path: &Path) -> impl Fn(std::io::Error) -> VendorError + Copy + '_ {
    move |source| VendorError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// `path` with `.` and `..` resolved without touching the file system, so
/// `<root>/../x` is not mistaken for a directory inside `root`.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for part in path.components() {
        if part == Component::ParentDir {
            normal.pop();
        } else {
            normal.push(part);
        }
    }
    normal
}

/// Hash the written addon, bump its version when only its content changed
/// since the previous run, and describe it.
fn record_addon(
    dir: &Path,
    item: &Item,
    addon: &str,
    options: &Options<'_>,
    result: &mut Vendored,
) -> Result<VendoredAddon, VendorError> {
    let io = io_at(dir);
    read_tree(dir).map_err(io).and_then(|mut files| {
        let manifest = files
            .iter()
            .find(|(name, _)| name == MANIFEST)
            .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();
        let source_version = parse_manifest(&manifest)
            .ok()
            .and_then(|parsed| parsed.version)
            .unwrap_or_default();
        let source_content = hash_files(&files);
        let written_version = written_version(
            options.previous.addon(addon),
            &source_content,
            &source_version,
            options.bump,
            addon,
            result,
        );
        let rewrite = (written_version != source_version)
            .then(|| set_manifest_version(&manifest, &written_version))
            .flatten();
        rewrite
            .map_or(Ok(()), |text| {
                std::fs::write(dir.join(MANIFEST), &text).map(|()| {
                    for (name, bytes) in &mut files {
                        if name == MANIFEST {
                            bytes.clone_from(&text.as_bytes().to_vec());
                        }
                    }
                })
            })
            .map_err(io)
            .map(|()| VendoredAddon {
                name: addon.to_owned(),
                group: item.group.clone(),
                package: item.package.clone(),
                source: String::new(),
                wheel: None,
                source_version,
                written_version,
                source_content,
                tree: hash_files(&files),
            })
    })
}

/// The version to write for an addon with `source_content` and
/// `source_version`, given what the previous run wrote: the previous
/// version while the content is unchanged, a bump when only the content
/// changed (recorded in `result`), else the shipped version.
fn written_version(
    previous: Option<&VendoredAddon>,
    source_content: &str,
    source_version: &str,
    bump_allowed: bool,
    addon: &str,
    result: &mut Vendored,
) -> String {
    match previous {
        Some(previous) if previous.source_content == source_content => {
            previous.written_version.clone()
        }
        Some(previous) if bump_allowed && previous.source_version == source_version => {
            if let Some(next) = bump(&previous.written_version) {
                result.bumped.push(Bumped {
                    addon: addon.to_owned(),
                    from: source_version.to_owned(),
                    to: next.clone(),
                });
                next
            } else {
                result.not_bumped.push(addon.to_owned());
                source_version.to_owned()
            }
        }
        _ => source_version.to_owned(),
    }
}

/// The addons under the group directories that differ from `record`:
/// changed, missing, or not vendored by odx. Each problem names the
/// directory relative to `root`.
pub fn check(
    record: &VendorRecord,
    group_dirs: &BTreeMap<String, PathBuf>,
    root: &Path,
) -> Vec<String> {
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace('\\', "/")
    };
    let mut problems = Vec::new();
    for (group, dir) in group_dirs {
        let expected: Vec<&VendoredAddon> = record
            .addons
            .iter()
            .filter(|addon| &addon.group == group)
            .collect();
        for addon in &expected {
            let path = dir.join(&addon.name);
            match tree_hash(&path) {
                Ok(tree) if tree == addon.tree => {}
                Ok(_) => problems.push(format!("{}: changed since `odx vendor`", relative(&path))),
                Err(_) => problems.push(format!("{}: missing", relative(&path))),
            }
        }
        let present = std::fs::read_dir(dir)
            .map_or_else(|_| Vec::new(), |entries| entries.flatten().collect());
        let mut extra: Vec<String> = present
            .iter()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !expected.iter().any(|addon| &addon.name == name))
            .map(|name| relative(&dir.join(name)))
            .collect();
        extra.sort();
        problems.extend(
            extra
                .into_iter()
                .map(|path| format!("{path}: not vendored by odx")),
        );
    }
    problems
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn item() -> Item {
        Item {
            group: "oca".to_owned(),
            package: "odoo-addon-a".to_owned(),
            version: "1".to_owned(),
            source: ItemSource::Wheel {
                url: String::new(),
                hash: String::new(),
            },
        }
    }

    #[test]
    fn unreadable_or_unwritable_addons_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let groups = BTreeMap::new();
        let previous = VendorRecord::default();
        let options = Options {
            root: tmp.path(),
            group_dirs: &groups,
            wheel_cache: tmp.path(),
            previous: &previous,
            bump: true,
        };
        let dangling = tmp.path().join("dangling");
        std::fs::create_dir_all(&dangling).unwrap();
        std::os::unix::fs::symlink("nowhere", dangling.join("x.py")).unwrap();
        let mut result = Vendored::default();
        assert!(record_addon(&dangling, &item(), "dangling", &options, &mut result).is_err());

        // A content change that needs a bump the manifest cannot take.
        let addon = tmp.path().join("a");
        std::fs::create_dir_all(&addon).unwrap();
        std::fs::write(addon.join(MANIFEST), "{'version': '1.0'}").unwrap();
        let previous = VendorRecord {
            version: VERSION,
            addons: vec![VendoredAddon {
                name: "a".to_owned(),
                source_version: "1.0".to_owned(),
                written_version: "1.0".to_owned(),
                source_content: "sha256:old".to_owned(),
                ..VendoredAddon::default()
            }],
        };
        let options = Options {
            previous: &previous,
            ..options
        };
        std::fs::set_permissions(addon.join(MANIFEST), std::fs::Permissions::from_mode(0o444))
            .unwrap();
        let failed = record_addon(&addon, &item(), "a", &options, &mut result);
        std::fs::set_permissions(addon.join(MANIFEST), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(failed.is_err());
    }
}
