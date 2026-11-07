//! Vendoring of locked Odoo addon packages into addon directories for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `odx vendor` writes the
//! addon directories of every dependency group from the locked wheels (hash
//! checked) and commits, records what it wrote in the vendor manifest, and
//! bumps an addon's version when its content changed without one, so the
//! platform updates it.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

pub mod archive;
pub mod content;
pub mod plan;
pub mod record;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use odx_analyze::manifest::parse_manifest;
use odx_forge::Http;

use crate::archive::{extract, fetch, subdirectory_entry, wheel_entry};
use odx_analyze::manifest::set_manifest_version;

use crate::content::{bump, hash_files, read_tree, tree_hash};

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
    /// A package is installed editable (develop mode).
    #[error("{0}: in develop mode (an editable path source); restore it before vendoring")]
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
    // Everything is fetched before a directory is touched: a source that
    // cannot be had leaves the addon directories as they were.
    let mut fetched = Vec::new();
    for (group, dir) in options.group_dirs {
        for item in items.iter().filter(|item| &item.group == group) {
            fetched.push((dir, item, fetch_item(item, options, http, git_archive)?));
        }
    }
    let baselines = baselines(options, exclude);
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
    for (dir, item, got) in fetched {
        let select = |name: &str| {
            got.subdirectory.as_deref().map_or_else(
                || wheel_entry(name),
                |subdirectory| subdirectory_entry(subdirectory, name),
            )
        };
        // A kept directory is vendored as it is: nothing is left out.
        let keep_all = |_: &str| false;
        let exclude: &dyn Fn(&str) -> bool = if matches!(item.source, ItemSource::Directory { .. })
        {
            &keep_all
        } else {
            exclude
        };
        let entries = extract(&got.bytes, &got.what, &select, exclude, dir).and_then(|addons| {
            addons
                .iter()
                .map(|addon| {
                    // A kept directory is its own source: its version is
                    // whatever it says.
                    let kept = matches!(item.source, ItemSource::Directory { .. });
                    let written = Written {
                        previous: options.previous.addon(addon).filter(|_| !kept),
                        baseline: baselines.get(addon).filter(|_| !kept),
                        bump: options.bump,
                    };
                    record_addon(&dir.join(addon), item, addon, &written, &mut result)
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        for entry in entries {
            result.record.addons.push(VendoredAddon {
                source: got.source.clone(),
                wheel: got.wheel.clone(),
                ..entry
            });
        }
    }
    result
        .record
        .addons
        .sort_by(|a, b| (&a.group, &a.name).cmp(&(&b.group, &b.name)));
    Ok(result)
}

/// The archive of one item, with what the vendor manifest records of it.
struct Fetched {
    /// The wheel or `git archive` bytes.
    bytes: Vec<u8>,
    /// What the archive is called in errors.
    what: String,
    /// The addon's directory in a git archive; `None` for a wheel.
    subdirectory: Option<String>,
    /// The version, or `git <sha>`.
    source: String,
    /// The wheel's hash.
    wheel: Option<String>,
}

/// Download or archive `item`.
fn fetch_item(
    item: &Item,
    options: &Options<'_>,
    http: &dyn Http,
    git_archive: &GitArchive<'_>,
) -> Result<Fetched, VendorError> {
    Ok(match &item.source {
        ItemSource::Wheel { url, hash } => Fetched {
            bytes: fetch(http, options.wheel_cache, url, hash)?,
            what: url.rsplit('/').next().unwrap_or(url).to_owned(),
            subdirectory: None,
            source: item.version.clone(),
            wheel: Some(hash.clone()),
        },
        ItemSource::Git {
            url,
            sha,
            subdirectory,
        } => Fetched {
            bytes: git_archive(url, sha, subdirectory.as_deref()).map_err(|message| {
                VendorError::Git {
                    package: item.package.clone(),
                    message,
                }
            })?,
            what: item.package.clone(),
            subdirectory: Some(subdirectory.clone().unwrap_or_default()),
            source: format!("git {sha}"),
            wheel: None,
        },
        ItemSource::Directory { path } => {
            let name = Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            Fetched {
                bytes: zip_directory(&options.root.join(path), &name)?,
                what: item.package.clone(),
                subdirectory: Some(name),
                source: format!("path {path}"),
                wheel: None,
            }
        }
    })
}

/// A zip of `dir`, its files under `name/`.
fn zip_directory(dir: &Path, name: &str) -> Result<Vec<u8>, VendorError> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let entries = std::fs::read_dir(&current)
            .and_then(|entries| {
                entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(io_at(&current))?;
        for path in entries {
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).map_err(io_at(&path))?;
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (path, bytes) in &files {
        let relative = path.strip_prefix(dir).unwrap_or(path);
        let entry = format!("{name}/{}", relative.to_string_lossy().replace('\\', "/"));
        // Writing into memory cannot fail.
        let _ = writer.start_file(entry, options);
        let _ = std::io::Write::write_all(&mut writer, bytes);
    }
    Ok(writer
        .finish()
        .map(std::io::Cursor::into_inner)
        .unwrap_or_default())
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
    written: &Written<'_>,
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
            written,
            &Shipped {
                content: &source_content,
                neutral: &neutral_hash(&files),
                version: &source_version,
            },
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

/// What decides the version written for an addon: the previous run's
/// entry, the addon that was in the directory when there is none, and
/// whether bumping is allowed.
struct Written<'a> {
    previous: Option<&'a VendoredAddon>,
    baseline: Option<&'a Baseline>,
    bump: bool,
}

/// What the package ships: its content hash, the hash with the manifest
/// version left out, and its version.
struct Shipped<'a> {
    content: &'a str,
    neutral: &'a str,
    version: &'a str,
}

/// An addon found in a group directory that the vendor manifest does not
/// know (vendored by another tool): its version, and its content hash with
/// the manifest version left out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Baseline {
    /// The version its manifest says.
    version: String,
    /// [`neutral_hash`] of its files.
    neutral: String,
}

/// The version to write for an addon: the previous version while the
/// content is unchanged, a bump when only the content changed (recorded in
/// `result`), else the shipped version. Without a previous run, an addon
/// already in the directory counts as one, so a version another tool bumped
/// never goes down.
fn written_version(
    written: &Written<'_>,
    shipped: &Shipped<'_>,
    addon: &str,
    result: &mut Vendored,
) -> String {
    let (previous_version, unchanged, same_source) = match (written.previous, written.baseline) {
        (Some(previous), _) => (
            previous.written_version.as_str(),
            previous.source_content == shipped.content,
            previous.source_version == shipped.version,
        ),
        (None, Some(baseline)) => (
            baseline.version.as_str(),
            baseline.neutral == shipped.neutral,
            !newer(shipped.version, &baseline.version),
        ),
        (None, None) => return shipped.version.to_owned(),
    };
    if unchanged {
        return if newer(shipped.version, previous_version) {
            shipped.version.to_owned()
        } else {
            previous_version.to_owned()
        };
    }
    if !(written.bump && same_source) {
        return shipped.version.to_owned();
    }
    bump(previous_version).map_or_else(
        || {
            result.not_bumped.push(addon.to_owned());
            shipped.version.to_owned()
        },
        |next| {
            result.bumped.push(Bumped {
                addon: addon.to_owned(),
                from: shipped.version.to_owned(),
                to: next.clone(),
            });
            next
        },
    )
}

/// Whether dotted version `a` is newer than `b`, segment by segment,
/// numerically where both segments are numbers.
fn newer(a: &str, b: &str) -> bool {
    let key = |version: &str| -> Vec<(u64, String)> {
        version
            .split('.')
            .map(|part| (part.parse().unwrap_or(0), part.to_owned()))
            .collect()
    };
    key(a) > key(b)
}

/// The content hash of `files` with the manifest version left out.
fn neutral_hash(files: &[(String, Vec<u8>)]) -> String {
    let neutral: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(name, bytes)| {
            let text = (name == MANIFEST)
                .then(|| set_manifest_version(&String::from_utf8_lossy(bytes), ""))
                .flatten();
            (
                name.clone(),
                text.map_or_else(|| bytes.clone(), String::into_bytes),
            )
        })
        .collect();
    hash_files(&neutral)
}

/// The addons in the group directories that the previous vendor manifest
/// does not know, by name; files `exclude` leaves out do not count.
fn baselines(options: &Options<'_>, exclude: &dyn Fn(&str) -> bool) -> BTreeMap<String, Baseline> {
    options
        .group_dirs
        .values()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
        .filter_map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut files = read_tree(&path).unwrap_or_default();
            files.retain(|(file, _)| !exclude(file));
            let version = files
                .iter()
                .find(|(file, _)| file == MANIFEST)
                .and_then(|(_, bytes)| parse_manifest(&String::from_utf8_lossy(bytes)).ok())
                .and_then(|manifest| manifest.version)?;
            options.previous.addon(&name).is_none().then(|| {
                (
                    name,
                    Baseline {
                        version,
                        neutral: neutral_hash(&files),
                    },
                )
            })
        })
        .collect()
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
    fn a_kept_directory_survives_its_own_group_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let oca = tmp.path().join("modules/oca");
        std::fs::create_dir_all(oca.join("kept/models")).unwrap();
        std::fs::write(oca.join("kept").join(MANIFEST), "{'version': '17.0.1.0.0'}").unwrap();
        std::fs::write(oca.join("kept/models/a.py"), "a = 1\n").unwrap();
        std::fs::create_dir_all(oca.join("stale")).unwrap();
        let groups = BTreeMap::from([("oca".to_owned(), oca.clone())]);
        let previous = VendorRecord::default();
        let options = Options {
            root: tmp.path(),
            group_dirs: &groups,
            wheel_cache: tmp.path(),
            previous: &previous,
            bump: true,
        };
        let kept = Item {
            source: ItemSource::Directory {
                path: "modules/oca/kept".to_owned(),
            },
            ..item()
        };
        let no_git = |_: &str, _: &str, _: Option<&str>| Err(String::new());
        let http = archive::tests::Answers::default();
        let vendored = vendor(
            std::slice::from_ref(&kept),
            &options,
            &http,
            &no_git,
            &|_| false,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(oca.join("kept/models/a.py")).unwrap(),
            "a = 1\n"
        );
        assert!(!oca.join("stale").exists());
        assert_eq!(vendored.record.addons[0].source, "path modules/oca/kept");

        // An unreadable directory or file fails before anything is removed.
        std::fs::set_permissions(
            oca.join("kept/models/a.py"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let failed = vendor(
            std::slice::from_ref(&kept),
            &options,
            &http,
            &no_git,
            &|_| false,
        );
        std::fs::set_permissions(
            oca.join("kept/models/a.py"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(failed.is_err());
        std::fs::set_permissions(
            oca.join("kept/models"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        let failed = vendor(
            std::slice::from_ref(&kept),
            &options,
            &http,
            &no_git,
            &|_| false,
        );
        std::fs::set_permissions(
            oca.join("kept/models"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(failed.is_err());
        assert!(oca.join("kept/models/a.py").exists());
        let git = Item {
            source: ItemSource::Git {
                url: "https://forge/a.git".to_owned(),
                sha: "0".repeat(40),
                subdirectory: None,
            },
            ..item()
        };
        assert!(vendor(&[kept, git], &options, &http, &no_git, &|_| false).is_err());
        assert!(oca.join("kept/models/a.py").exists());
    }

    #[test]
    fn unreadable_or_unwritable_addons_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let fresh = Written {
            previous: None,
            baseline: None,
            bump: true,
        };
        let dangling = tmp.path().join("dangling");
        std::fs::create_dir_all(&dangling).unwrap();
        std::os::unix::fs::symlink("nowhere", dangling.join("x.py")).unwrap();
        let mut result = Vendored::default();
        assert!(record_addon(&dangling, &item(), "dangling", &fresh, &mut result).is_err());

        // A content change that needs a bump the manifest cannot take.
        let addon = tmp.path().join("a");
        std::fs::create_dir_all(&addon).unwrap();
        std::fs::write(addon.join(MANIFEST), "{'version': '1.0'}").unwrap();
        let previous = VendoredAddon {
            name: "a".to_owned(),
            source_version: "1.0".to_owned(),
            written_version: "1.0".to_owned(),
            source_content: "sha256:old".to_owned(),
            ..VendoredAddon::default()
        };
        let known = Written {
            previous: Some(&previous),
            ..fresh
        };
        std::fs::set_permissions(addon.join(MANIFEST), std::fs::Permissions::from_mode(0o444))
            .unwrap();
        let failed = record_addon(&addon, &item(), "a", &known, &mut result);
        std::fs::set_permissions(addon.join(MANIFEST), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(failed.is_err());
    }

    #[test]
    fn an_addon_another_tool_vendored_is_the_baseline() {
        let shipped = |content, version| Shipped {
            content,
            neutral: content,
            version,
        };
        let baseline = |version: &str, neutral: &str| Baseline {
            version: version.to_owned(),
            neutral: neutral.to_owned(),
        };
        let decide = |base: &Baseline, ship: &Shipped<'_>, bump: bool| {
            let written = Written {
                previous: None,
                baseline: Some(base),
                bump,
            };
            let mut result = Vendored::default();
            (
                written_version(&written, ship, "a", &mut result),
                result.bumped.len(),
            )
        };
        // Bumped by the other tool, same content: kept, never lowered.
        let base = baseline("17.0.1.0.0.1", "x");
        assert_eq!(
            decide(&base, &shipped("x", "17.0.1.0.0"), true),
            ("17.0.1.0.0.1".into(), 0)
        );
        // New content, same upstream version: bumped once more.
        assert_eq!(
            decide(&base, &shipped("y", "17.0.1.0.0"), true),
            ("17.0.1.0.0.2".into(), 1)
        );
        assert_eq!(
            decide(&base, &shipped("y", "17.0.1.0.0"), false),
            ("17.0.1.0.0".into(), 0)
        );
        // A newer upstream version wins.
        assert_eq!(
            decide(&base, &shipped("y", "17.0.2.0.0"), true),
            ("17.0.2.0.0".into(), 0)
        );
        assert_eq!(
            decide(&base, &shipped("x", "17.0.2.0.0"), true),
            ("17.0.2.0.0".into(), 0)
        );
        assert!(newer("17.0.10.0.0", "17.0.9.0.0"));
        assert!(!newer("17.0.1", "17.0.1"));
    }
}
