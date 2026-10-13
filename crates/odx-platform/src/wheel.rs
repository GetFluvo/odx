//! Giving a built wheel odx's version and making it byte-reproducible.

use std::fmt::Write as _;
use std::io::{Cursor, Read, Write as _};

use crate::installed::record_hash;

/// A wheel after [`retag`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retagged {
    /// `<distribution>-<version>-<tag>.whl`.
    pub file_name: String,
    /// The version written.
    pub version: String,
    /// The wheel.
    pub bytes: Vec<u8>,
}

/// `wheel` with version `<its version>+<local>`, byte-reproducible.
///
/// The `.dist-info` directory is renamed, `METADATA` and `RECORD`
/// rewritten, and every entry written with a fixed timestamp and
/// permissions, so the same input always gives the same bytes.
///
/// # Errors
///
/// When `wheel` is not a zip archive or has no `.dist-info` with a
/// `METADATA` version.
pub fn retag(wheel: &[u8], local: &str) -> Result<Retagged, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(wheel)).map_err(|e| e.to_string())?;
    let entries: Vec<(String, Vec<u8>)> = (0..archive.len())
        .filter_map(|index| {
            archive.by_index(index).ok().and_then(|mut file| {
                let name = String::from_utf8_lossy(file.name_raw()).into_owned();
                let mut bytes = Vec::new();
                (!file.is_dir() && file.read_to_end(&mut bytes).is_ok()).then_some((name, bytes))
            })
        })
        .collect();
    let (dist_info, metadata) = entries
        .iter()
        .find_map(|(name, bytes)| {
            name.strip_suffix("/METADATA")
                .filter(|dir| dir.ends_with(".dist-info") && !dir.contains('/'))
                .map(|dir| (dir.to_owned(), String::from_utf8_lossy(bytes).into_owned()))
        })
        .ok_or_else(|| "the wheel has no .dist-info/METADATA".to_owned())?;
    let old = metadata
        .lines()
        .find_map(|line| line.strip_prefix("Version:"))
        .map(str::trim)
        .ok_or_else(|| format!("{dist_info}/METADATA has no version"))?
        .to_owned();
    let version = format!("{}+{local}", old.split('+').next().unwrap_or(&old));
    let distribution = dist_info
        .strip_suffix(".dist-info")
        .and_then(|stem| stem.rsplit_once('-'))
        .map_or(dist_info.as_str(), |(name, _)| name)
        .to_owned();
    let new_dir = format!("{distribution}-{version}.dist-info");
    let tag = entries
        .iter()
        .find(|(name, _)| *name == format!("{dist_info}/WHEEL"))
        .and_then(|(_, bytes)| {
            String::from_utf8_lossy(bytes)
                .lines()
                .find_map(|line| line.strip_prefix("Tag:").map(|tag| tag.trim().to_owned()))
        })
        .unwrap_or_else(|| "py3-none-any".to_owned());
    let record_name = format!("{new_dir}/RECORD");
    let mut files: Vec<(String, Vec<u8>)> = entries
        .into_iter()
        .filter(|(name, _)| *name != format!("{dist_info}/RECORD"))
        .map(|(name, bytes)| {
            let renamed = name
                .strip_prefix(&format!("{dist_info}/"))
                .map_or_else(|| name.clone(), |rest| format!("{new_dir}/{rest}"));
            let bytes = if renamed == format!("{new_dir}/METADATA") {
                metadata
                    .lines()
                    .map(|line| {
                        if line.starts_with("Version:") {
                            format!("Version: {version}\n")
                        } else {
                            format!("{line}\n")
                        }
                    })
                    .collect::<String>()
                    .into_bytes()
            } else {
                bytes
            };
            (renamed, bytes)
        })
        .collect();
    // Package files first, metadata last, each group in name order.
    files.sort_by(|(a, _), (b, _)| (a.starts_with(&new_dir), a).cmp(&(b.starts_with(&new_dir), b)));
    let mut record = String::new();
    for (name, bytes) in &files {
        let _ = writeln!(record, "{name},{},{}", record_hash(bytes), bytes.len());
    }
    let _ = writeln!(record, "{record_name},,");
    files.push((record_name, record.into_bytes()));
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (name, bytes) in &files {
        // Writing into memory cannot fail.
        let _ = writer.start_file(name.as_str(), options);
        let _ = writer.write_all(bytes);
    }
    let bytes = writer.finish().map(Cursor::into_inner).unwrap_or_default();
    Ok(Retagged {
        file_name: format!("{distribution}-{version}-{tag}.whl"),
        version,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in files {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn read(wheel: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(wheel)).unwrap();
        let mut text = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    #[test]
    fn retagged_wheels_are_reproducible() {
        let built = wheel(&[
            ("odoo/", ""),
            ("odoo/addons/a/__manifest__.py", "{}"),
            (
                "odoo_addon_a-17.0.1.0.0.dist-info/METADATA",
                "Metadata-Version: 2.1\nName: odoo-addon-a\nVersion: 17.0.1.0.0\n",
            ),
            (
                "odoo_addon_a-17.0.1.0.0.dist-info/WHEEL",
                "Wheel-Version: 1.0\nTag: py3-none-any\n",
            ),
            ("odoo_addon_a-17.0.1.0.0.dist-info/RECORD", "stale"),
        ]);
        let retagged = retag(&built, "odx.0123456789ab").unwrap();
        assert_eq!(retagged.version, "17.0.1.0.0+odx.0123456789ab");
        assert_eq!(
            retagged.file_name,
            "odoo_addon_a-17.0.1.0.0+odx.0123456789ab-py3-none-any.whl"
        );
        let dir = "odoo_addon_a-17.0.1.0.0+odx.0123456789ab.dist-info";
        assert!(
            read(&retagged.bytes, &format!("{dir}/METADATA"))
                .contains("Version: 17.0.1.0.0+odx.0123456789ab\n")
        );
        let record = read(&retagged.bytes, &format!("{dir}/RECORD"));
        assert!(
            record.starts_with(&format!(
                "odoo/addons/a/__manifest__.py,{},2\n",
                record_hash(b"{}")
            )),
            "{record}"
        );
        assert!(record.ends_with(&format!("{dir}/RECORD,,\n")), "{record}");
        // The same input, the same bytes; retagging again replaces the label.
        assert_eq!(retag(&built, "odx.0123456789ab").unwrap(), retagged);
        let again = retag(&retagged.bytes, "odx.fedcba987654").unwrap();
        assert_eq!(again.version, "17.0.1.0.0+odx.fedcba987654");
    }

    #[test]
    fn broken_wheels() {
        assert!(retag(b"no zip", "x").is_err());
        assert_eq!(
            retag(&wheel(&[("a.py", "")]), "x").unwrap_err(),
            "the wheel has no .dist-info/METADATA"
        );
        assert_eq!(
            retag(&wheel(&[("a-1.dist-info/METADATA", "Name: a\n")]), "x").unwrap_err(),
            "a-1.dist-info/METADATA has no version"
        );
        // Without a WHEEL file the pure-Python tag is assumed.
        let plain = retag(&wheel(&[("a-1.dist-info/METADATA", "Version: 1\n")]), "x").unwrap();
        assert_eq!(plain.file_name, "a-1+x-py3-none-any.whl");
    }
}
