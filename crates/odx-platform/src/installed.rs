//! Installed distributions: what a virtual environment (or any
//! `site-packages`) holds, and whether it is what the locked wheels hold.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::normalize_name;

/// One line of a `RECORD` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEntry {
    /// Path relative to `site-packages`.
    pub path: String,
    /// `sha256=<urlsafe base64>`, absent for `RECORD` itself and for files
    /// the installer generated.
    pub hash: Option<String>,
}

/// An installed distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distribution {
    /// Normalized name.
    pub name: String,
    /// Version from `METADATA`.
    pub version: String,
    /// Its `*.dist-info` directory.
    pub dist_info: PathBuf,
    /// Its `RECORD`.
    pub record: Vec<RecordEntry>,
    /// Wheel tags from `WHEEL` (`py3-none-any`).
    pub tags: Vec<String>,
    /// The commit of a distribution installed from git (`direct_url.json`).
    pub commit: Option<String>,
}

/// The `site-packages` of `dir`: `dir` itself when it holds `*.dist-info`
/// directories, else `lib/python*/site-packages` or `Lib/site-packages`
/// of a virtual environment.
pub fn site_packages(dir: &Path) -> Option<PathBuf> {
    if dist_infos(dir).next().is_some() {
        return Some(dir.to_path_buf());
    }
    let windows = dir.join("Lib").join("site-packages");
    if windows.is_dir() {
        return Some(windows);
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir.join("lib"))
        .map_or_else(
            |_| Vec::new(),
            |entries| entries.flatten().map(|e| e.path()).collect(),
        )
        .into_iter()
        .map(|python| python.join("site-packages"))
        .filter(|site| site.is_dir())
        .collect();
    candidates.sort();
    candidates.pop()
}

fn dist_infos(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .map_or_else(
            |_| Vec::new(),
            |entries| entries.flatten().map(|e| e.path()).collect(),
        )
        .into_iter()
        .filter(|path| {
            path.is_dir()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "dist-info")
        })
}

/// Every distribution installed in `site`, sorted by name.
pub fn scan(site: &Path) -> Vec<Distribution> {
    let mut found: Vec<Distribution> = dist_infos(site)
        .filter_map(|dist_info| {
            let metadata = std::fs::read_to_string(dist_info.join("METADATA")).ok()?;
            let field = |key: &str| {
                metadata
                    .lines()
                    .take_while(|line| !line.is_empty())
                    .find_map(|line| line.strip_prefix(key))
                    .map(|value| value.trim().to_owned())
            };
            let name = normalize_name(&field("Name:")?);
            let version = field("Version:")?;
            let record = parse_record(
                &std::fs::read_to_string(dist_info.join("RECORD")).unwrap_or_default(),
            );
            let tags = std::fs::read_to_string(dist_info.join("WHEEL"))
                .unwrap_or_default()
                .lines()
                .filter_map(|line| line.strip_prefix("Tag:"))
                .map(|tag| tag.trim().to_owned())
                .collect();
            let commit = std::fs::read_to_string(dist_info.join("direct_url.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .pointer("/vcs_info/commit_id")?
                        .as_str()
                        .map(str::to_owned)
                });
            Some(Distribution {
                name,
                version,
                dist_info,
                record,
                tags,
                commit,
            })
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The entries of a `RECORD` file (CSV: path, hash, size).
pub fn parse_record(text: &str) -> Vec<RecordEntry> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = csv_fields(line);
            let mut fields = fields.into_iter();
            let path = fields.next().unwrap_or_default();
            let hash = fields.next().filter(|hash| !hash.is_empty());
            RecordEntry { path, hash }
        })
        .collect()
}

/// The fields of one CSV line, with `"`-quoted fields unquoted.
fn csv_fields(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                fields
                    .last_mut()
                    .into_iter()
                    .for_each(|field| field.push('"'));
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(String::new()),
            other => fields
                .last_mut()
                .into_iter()
                .for_each(|field| field.push(other)),
        }
    }
    fields
}

/// `sha256=<urlsafe base64 without padding>` of `bytes`, as `RECORD`
/// writes it.
pub fn record_hash(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let digest = Sha256::digest(bytes);
    let mut text = String::from("sha256=");
    for chunk in digest.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0_u32, |bits, (i, byte)| {
            bits | u32::from(*byte) << (16 - 8 * i)
        });
        let digits = chunk.len() + 1;
        for i in 0..digits {
            let index = (bits >> (18 - 6 * i)) & 0x3f;
            text.extend(ALPHABET.get(index as usize).map(|c| char::from(*c)));
        }
    }
    text
}

/// The files of `distribution` that differ from its `RECORD` (changed or
/// missing), relative to `site`.
pub fn modified_files(site: &Path, distribution: &Distribution) -> Vec<String> {
    distribution
        .record
        .iter()
        .filter_map(|entry| {
            let expected = entry.hash.as_ref()?;
            let actual = std::fs::read(site.join(&entry.path))
                .map(|bytes| record_hash(&bytes))
                .unwrap_or_default();
            (&actual != expected).then(|| entry.path.clone())
        })
        .collect()
}

/// The `RECORD` inside a wheel.
///
/// # Errors
///
/// When the wheel is not a zip archive or has no `RECORD`.
pub fn wheel_record(wheel: &[u8]) -> Result<Vec<RecordEntry>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(wheel)).map_err(|e| e.to_string())?;
    (0..archive.len())
        .find_map(|index| {
            let mut file = archive
                .by_index(index)
                .ok()
                .filter(|file| file.name_raw().ends_with(b".dist-info/RECORD"))?;
            let mut text = String::new();
            file.read_to_string(&mut text)
                .ok()
                .map(|_| parse_record(&text))
        })
        .ok_or_else(|| "the wheel has no readable RECORD".to_owned())
}

/// The files of the locked wheel (`wheel`, its `RECORD`) that are installed
/// with other content or not at all, according to the installed `RECORD`.
/// Files the wheel installs elsewhere (`*.data/`) are not compared.
pub fn substituted(wheel: &[RecordEntry], installed: &[RecordEntry]) -> Vec<String> {
    wheel
        .iter()
        .filter(|entry| entry.hash.is_some() && !entry.path.contains(".data/"))
        .filter(|entry| {
            !installed
                .iter()
                .any(|other| other.path == entry.path && other.hash == entry.hash)
        })
        .map(|entry| entry.path.clone())
        .collect()
}

/// The tags of a wheel file name (`name-1.0-py3-none-any.whl` →
/// `py3-none-any`), expanded over compressed tag sets (`py2.py3`).
pub fn wheel_tags(file_name: &str) -> Vec<String> {
    let stem = file_name.rsplit('/').next().unwrap_or(file_name);
    let stem = stem.strip_suffix(".whl").unwrap_or(stem);
    let parts: Vec<&str> = stem.split('-').collect();
    let [.., python, abi, platform] = parts.as_slice() else {
        return Vec::new();
    };
    let mut tags = Vec::new();
    for python in python.split('.') {
        for abi in abi.split('.') {
            for platform in platform.split('.') {
                tags.push(format!("{python}-{abi}-{platform}"));
            }
        }
    }
    tags
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn record_hashes_match_pip() {
        // `python -c "import hashlib,base64;print(base64.urlsafe_b64encode(hashlib.sha256(b'x = 1\n').digest()).rstrip(b'='))"`
        assert_eq!(
            record_hash(b"x = 1\n"),
            "sha256=nia_NpkRxFwkPGhBR7I_yeHc_PJX0pmhxjIBam_NM_Q"
        );
        assert_eq!(record_hash(b"").len(), 7 + 43);
    }

    #[test]
    fn records() {
        let entries = parse_record(
            "odoo/addons/a/x.py,sha256=abc,6\n\"odoo/addons/a/b,c.py\",sha256=def,1\n\"q\"\"uote.py\",,\na.dist-info/RECORD,,\n\n",
        );
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[1].path, "odoo/addons/a/b,c.py");
        assert_eq!(entries[2].path, "q\"uote.py");
        assert_eq!(entries[3].hash, None);
        let installed = vec![
            RecordEntry {
                path: "a.py".to_owned(),
                hash: Some("h1".to_owned()),
            },
            RecordEntry {
                path: "b.py".to_owned(),
                hash: Some("other".to_owned()),
            },
        ];
        let wheel = vec![
            RecordEntry {
                path: "a.py".to_owned(),
                hash: Some("h1".to_owned()),
            },
            RecordEntry {
                path: "b.py".to_owned(),
                hash: Some("h2".to_owned()),
            },
            RecordEntry {
                path: "c.py".to_owned(),
                hash: Some("h3".to_owned()),
            },
            RecordEntry {
                path: "x.data/scripts/x".to_owned(),
                hash: Some("h4".to_owned()),
            },
            RecordEntry {
                path: "x.dist-info/RECORD".to_owned(),
                hash: None,
            },
        ];
        assert_eq!(substituted(&wheel, &installed), ["b.py", "c.py"]);
    }

    #[test]
    fn tags() {
        assert_eq!(
            wheel_tags("https://x/a-1.0-py3-none-any.whl"),
            ["py3-none-any"]
        );
        assert_eq!(
            wheel_tags("a-1.0-1-py2.py3-none-any.whl"),
            ["py2-none-any", "py3-none-any"]
        );
        assert_eq!(wheel_tags("broken.whl"), Vec::<String>::new());
    }

    #[test]
    fn scans_site_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let venv = tmp.path().join("venv");
        let site = venv.join("lib").join("python3.12").join("site-packages");
        let dist = site.join("odoo_addon_a-1.0.dist-info");
        write(&site.join("odoo/addons/a/x.py"), "x = 1\n");
        write(
            &dist.join("METADATA"),
            "Metadata-Version: 2.1\nName: odoo-addon-a\nVersion: 1.0\n\nName: not this\n",
        );
        write(
            &dist.join("RECORD"),
            &format!(
                "odoo/addons/a/x.py,{},6\nodoo/addons/a/gone.py,sha256=x,1\nodoo_addon_a-1.0.dist-info/RECORD,,\n",
                record_hash(b"x = 1\n")
            ),
        );
        write(
            &dist.join("WHEEL"),
            "Wheel-Version: 1.0\nTag: py3-none-any\n",
        );
        write(
            &dist.join("direct_url.json"),
            "{\"url\": \"https://x\", \"vcs_info\": {\"vcs\": \"git\", \"commit_id\": \"0123\"}}",
        );
        // Not a distribution: no METADATA, or no version.
        std::fs::create_dir_all(site.join("broken-1.0.dist-info")).unwrap();
        write(
            &site.join("noversion-1.dist-info/METADATA"),
            "Name: noversion\n",
        );
        write(&site.join("file.dist-info"), "");
        write(&site.join("noname-1.dist-info/METADATA"), "Version: 1\n");

        assert_eq!(site_packages(&venv), Some(site.clone()));
        assert_eq!(site_packages(&site), Some(site.clone()));
        assert_eq!(site_packages(tmp.path()), None);
        let windows = tmp.path().join("win");
        std::fs::create_dir_all(windows.join("Lib").join("site-packages")).unwrap();
        assert_eq!(
            site_packages(&windows),
            Some(windows.join("Lib").join("site-packages"))
        );

        let found = scan(&site);
        assert_eq!(found.len(), 1);
        let a = &found[0];
        assert_eq!(
            (a.name.as_str(), a.version.as_str()),
            ("odoo-addon-a", "1.0")
        );
        assert_eq!(a.tags, ["py3-none-any"]);
        assert_eq!(a.commit.as_deref(), Some("0123"));
        assert_eq!(modified_files(&site, a), ["odoo/addons/a/gone.py"]);
        write(&site.join("odoo/addons/a/x.py"), "x = 2\n");
        assert_eq!(
            modified_files(&site, a),
            ["odoo/addons/a/x.py", "odoo/addons/a/gone.py"]
        );
    }

    #[test]
    fn records_inside_wheels() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        writer
            .start_file("a-1.0.dist-info/RECORD", options)
            .unwrap();
        writer.write_all(b"a.py,sha256=x,1\n").unwrap();
        let wheel = writer.finish().unwrap().into_inner();
        assert_eq!(wheel_record(&wheel).unwrap()[0].path, "a.py");
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file("a.py", options).unwrap();
        let empty = writer.finish().unwrap().into_inner();
        assert_eq!(
            wheel_record(&empty).unwrap_err(),
            "the wheel has no readable RECORD"
        );
        assert!(wheel_record(b"no zip").is_err());
    }
}
