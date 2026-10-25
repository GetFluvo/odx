//! `odx lock --auto-update`: which newer releases of index packages may be
//! taken without review.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;
use std::str::FromStr;

use odx_analyze::manifest::set_manifest_version;
use pep440_rs::Version;
use serde::Serialize;

/// A release file on the index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleaseFile {
    /// Download URL.
    pub url: String,
    /// File name.
    pub filename: String,
    /// `sha256:<hex>`.
    pub hash: String,
}

/// The releases of a package on the index: version → wheels (yanked ones
/// left out).
pub type Releases = BTreeMap<String, Vec<ReleaseFile>>;

/// The releases in a `/pypi/<name>/json` answer.
///
/// # Errors
///
/// When the answer is not the JSON API's.
pub fn parse_releases(json: &[u8]) -> Result<Releases, String> {
    let value: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let releases = value
        .get("releases")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "no releases in the index answer".to_owned())?;
    Ok(releases
        .iter()
        .map(|(version, files)| {
            let wheels = files
                .as_array()
                .into_iter()
                .flatten()
                .filter(|file| {
                    file.get("yanked").and_then(serde_json::Value::as_bool) != Some(true)
                })
                .filter_map(|file| {
                    let text = |key: &str| file.pointer(key).and_then(serde_json::Value::as_str);
                    match (text("/filename"), text("/url"), text("/digests/sha256")) {
                        (Some(filename), Some(url), Some(digest)) => Some(ReleaseFile {
                            url: url.to_owned(),
                            filename: filename.to_owned(),
                            hash: format!("sha256:{digest}"),
                        }),
                        _ => None,
                    }
                })
                .filter(|file| {
                    Path::new(&file.filename)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("whl"))
                })
                .collect();
            (version.clone(), wheels)
        })
        .collect())
}

/// What to do with a package's newer releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// Nothing newer in the series.
    Current,
    /// The newest release that only changes the last version segment.
    LastSegment(String),
    /// Newer releases exist, but they change more than the last segment.
    Review(String),
}

/// The update `releases` offer over `current` within the Odoo `series`
/// (`17.0`): versions that are not PEP 440, outside the series, or have no
/// wheel are ignored.
pub fn candidate(current: &str, series: &str, releases: &Releases) -> Candidate {
    let Ok(now) = Version::from_str(current) else {
        return Candidate::Current;
    };
    // Odoo versions have five segments (series, major, minor, patch);
    // OCA post-releases add a sixth. Only the fifth may change.
    let head = |version: &str| version.split('.').take(4).collect::<Vec<_>>().join(".");
    let prefix = format!("{series}.");
    let mut newer: Vec<(Version, &String)> = releases
        .iter()
        .filter(|(version, wheels)| version.starts_with(&prefix) && !wheels.is_empty())
        .filter_map(|(version, _)| Some((Version::from_str(version).ok()?, version)))
        .filter(|(parsed, _)| *parsed > now)
        .collect();
    newer.sort();
    let same_head = newer
        .iter()
        .rev()
        .find(|(_, version)| head(version) == head(current));
    match (same_head, newer.last()) {
        (Some((_, version)), _) => Candidate::LastSegment((*version).clone()),
        (None, Some((_, version))) => Candidate::Review((*version).clone()),
        (None, None) => Candidate::Current,
    }
}

/// The files of an addon wheel under `odoo/addons/<addon>/`, by path in the
/// addon, with the manifest's version blanked so a release that only bumps
/// it does not count as a change.
///
/// # Errors
///
/// When `wheel` is not a zip archive.
pub fn addon_files(wheel: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(wheel)).map_err(|e| e.to_string())?;
    // A file that cannot be read fails the comparison rather than counting
    // as removed.
    let files = (0..archive.len())
        .map(|index| {
            let mut file = archive.by_index(index).map_err(|e| e.to_string())?;
            let name = String::from_utf8_lossy(file.name_raw()).into_owned();
            let Some(path) = name
                .strip_prefix("odoo/addons/")
                .and_then(|rest| rest.split_once('/'))
                .map(|(_, path)| path.to_owned())
                .filter(|path| !path.is_empty())
            else {
                return Ok(None);
            };
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)
                .map_err(|e| format!("{name}: {e}"))?;
            if path == "__manifest__.py" {
                bytes = blank_version(&String::from_utf8_lossy(&bytes)).into_bytes();
            }
            Ok(Some((path, bytes)))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(files.into_iter().flatten().collect())
}

fn blank_version(manifest: &str) -> String {
    set_manifest_version(manifest, "").unwrap_or_else(|| manifest.to_owned())
}

/// The paths that differ between two versions of an addon (added, removed
/// or changed), sorted.
pub fn changed_paths(
    old: &BTreeMap<String, Vec<u8>>,
    new: &BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    let mut changed: Vec<String> = old
        .iter()
        .filter(|(path, bytes)| new.get(*path) != Some(bytes))
        .map(|(path, _)| path.clone())
        .chain(new.keys().filter(|path| !old.contains_key(*path)).cloned())
        .collect();
    changed.sort();
    changed
}

/// An update applied or held back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Update {
    /// Distribution.
    pub package: String,
    /// Locked version.
    pub from: String,
    /// Offered version.
    pub to: String,
    /// Why it needs review; empty when applied.
    pub reason: String,
    /// The paths that changed (all of them when applied, the ones outside
    /// the allowed paths when held back).
    pub paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn releases(versions: &[&str]) -> Releases {
        versions
            .iter()
            .map(|version| {
                (
                    (*version).to_owned(),
                    vec![ReleaseFile {
                        url: format!("https://files/{version}.whl"),
                        filename: format!("x-{version}-py3-none-any.whl"),
                        hash: "sha256:00".to_owned(),
                    }],
                )
            })
            .collect()
    }

    #[test]
    fn candidates() {
        let all = releases(&[
            "16.0.1.0.9",
            "17.0.1.2.3",
            "17.0.1.2.4",
            "17.0.1.2.5",
            "17.0.1.3.0",
            "18.0.1.0.0",
        ]);
        assert_eq!(
            candidate("17.0.1.2.3", "17.0", &all),
            Candidate::LastSegment("17.0.1.2.5".to_owned())
        );
        assert_eq!(
            candidate("17.0.1.2.5", "17.0", &all),
            Candidate::Review("17.0.1.3.0".to_owned())
        );
        assert_eq!(candidate("17.0.1.3.0", "17.0", &all), Candidate::Current);
        assert_eq!(candidate("not a version", "17.0", &all), Candidate::Current);
        let mut odd = releases(&["17.0.1.2.3.1", "17.0.x"]);
        odd.insert("17.0.1.2.9".to_owned(), Vec::new());
        // An OCA post-release, and a release after one.
        assert_eq!(
            candidate("17.0.1.2.3", "17.0", &odd),
            Candidate::LastSegment("17.0.1.2.3.1".to_owned())
        );
        let post = releases(&["17.0.1.1.8.1", "17.0.1.1.9", "17.0.1.2.0"]);
        assert_eq!(
            candidate("17.0.1.1.8.1", "17.0", &post),
            Candidate::LastSegment("17.0.1.1.9".to_owned())
        );
    }

    #[test]
    fn index_answers() {
        let json = br#"{"info": {}, "releases": {
            "17.0.1.0.0": [
                {"filename": "x-17.0.1.0.0-py3-none-any.whl", "url": "https://f/x.whl", "digests": {"sha256": "aa"}},
                {"filename": "x-17.0.1.0.0.tar.gz", "url": "https://f/x.tar.gz", "digests": {"sha256": "bb"}},
                {"filename": "y.whl", "url": "https://f/y.whl", "digests": {"sha256": "cc"}, "yanked": true},
                {"filename": "z.whl"}
            ],
            "17.0.1.0.1": "broken"
        }}"#;
        let parsed = parse_releases(json).unwrap();
        assert_eq!(
            parsed["17.0.1.0.0"],
            [ReleaseFile {
                url: "https://f/x.whl".to_owned(),
                filename: "x-17.0.1.0.0-py3-none-any.whl".to_owned(),
                hash: "sha256:aa".to_owned()
            }]
        );
        assert_eq!(parsed["17.0.1.0.1"], []);
        assert_eq!(
            parse_releases(b"{}").unwrap_err(),
            "no releases in the index answer"
        );
        assert!(parse_releases(b"no json").is_err());
    }

    fn wheel(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in files {
            writer.start_file(*name, options).unwrap();
            writer.write_all(content.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn content_changes() {
        let old = addon_files(&wheel(&[
            (
                "odoo/addons/x/__manifest__.py",
                "{\"name\": \"X\", \"version\": \"17.0.1.2.3\"}",
            ),
            ("odoo/addons/x/i18n/de.po", "msgid a"),
            ("odoo/addons/x/models/x.py", "x = 1"),
            ("odoo/addons/x/gone.py", ""),
            ("x-17.0.1.2.3.dist-info/METADATA", "Version: 17.0.1.2.3"),
            ("odoo/__init__.py", ""),
        ]))
        .unwrap();
        let new = addon_files(&wheel(&[
            (
                "odoo/addons/x/__manifest__.py",
                "{\"name\": \"X\", \"version\": \"17.0.1.2.4\"}",
            ),
            ("odoo/addons/x/i18n/de.po", "msgid b"),
            ("odoo/addons/x/models/x.py", "x = 1"),
            ("odoo/addons/x/readme/NEW.md", ""),
            ("x-17.0.1.2.4.dist-info/METADATA", "Version: 17.0.1.2.4"),
        ]))
        .unwrap();
        assert_eq!(
            changed_paths(&old, &new),
            ["gone.py", "i18n/de.po", "readme/NEW.md"]
        );
        assert_eq!(
            blank_version("{'version' : '1.0', 'x': 1}"),
            "{'version' : '', 'x': 1}"
        );
        assert_eq!(blank_version("{'name': 'x'}"), "{'name': 'x'}");
        assert!(addon_files(b"no zip").is_err());
        // A damaged file, or file header, fails the comparison.
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("odoo/addons/x/a.py", stored).unwrap();
        writer.write_all(b"print(1)\n").unwrap();
        let mut damaged = writer.finish().unwrap().into_inner();
        let at = damaged
            .windows(9)
            .position(|bytes| bytes == b"print(1)\n")
            .unwrap();
        damaged[at] = b'P';
        assert!(
            addon_files(&damaged)
                .unwrap_err()
                .starts_with("odoo/addons/x/a.py: ")
        );
        let mut headless = wheel(&[("odoo/addons/x/a.py", "")]);
        headless[0] = b'X';
        assert!(addon_files(&headless).is_err());
    }
}
