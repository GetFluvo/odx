//! Content hashes of addon directories and manifest version bumps.

use std::fmt::Write as _;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

/// `sha256:<hex>` over every file under `dir`.
///
/// The relative paths (with `/`) in sorted order, each followed by its
/// length and bytes, so a renamed, added, removed or changed file changes
/// the hash and nothing else (timestamps, permissions) does.
///
/// # Errors
///
/// When `dir` or a file under it cannot be read.
pub fn tree_hash(dir: &Path) -> io::Result<String> {
    read_tree(dir).map(|files| hash_files(&files))
}

/// Every file under `dir` with its bytes, by relative path (with `/`),
/// sorted.
///
/// # Errors
///
/// When `dir` or a file under it cannot be read.
pub fn read_tree(dir: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut names = Vec::new();
    collect(dir, dir, &mut names)?;
    names.sort();
    names
        .into_iter()
        .map(|name| std::fs::read(dir.join(&name)).map(|bytes| (name, bytes)))
        .collect()
}

/// The hash [`tree_hash`] computes, of files already read.
pub fn hash_files(files: &[(String, Vec<u8>)]) -> String {
    let mut hasher = Sha256::new();
    for (name, bytes) in files {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(bytes.len().to_le_bytes());
        hasher.update(bytes);
    }
    format!("sha256:{}", hex(&hasher.finalize()))
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, files)?;
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            files.push(relative.display().to_string().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// `version` with its last segment incremented (`17.0.1.2.0` →
/// `17.0.1.2.1`); `None` when the last segment is not a number.
pub fn bump(version: &str) -> Option<String> {
    let (head, last) = version.rsplit_once('.').unwrap_or(("", version));
    let next = last.parse::<u64>().ok()?.checked_add(1)?;
    Some(if head.is_empty() {
        next.to_string()
    } else {
        format!("{head}.{next}")
    })
}

/// `text` (a `__manifest__.py`) with the string after the `version` key
/// replaced by `version`; `None` when there is no such string.
pub fn set_manifest_version(text: &str, version: &str) -> Option<String> {
    ["\"version\"", "'version'"].iter().find_map(|key| {
        let (before, after) = text.split_once(key)?;
        let (gap, value) = after.split_once(':')?;
        let (lead, quoted) = value.split_at(value.find(['"', '\''])?);
        let quote = if quoted.starts_with('"') { '"' } else { '\'' };
        let (_, inner) = quoted.split_at(1);
        let (_, rest) = inner.split_once(quote)?;
        Some(format!(
            "{before}{key}{gap}:{lead}{quote}{version}{quote}{rest}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_follow_content_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir_all(dir.join("models")).unwrap();
        std::fs::write(dir.join("__manifest__.py"), "{}").unwrap();
        std::fs::write(dir.join("models").join("x.py"), "x").unwrap();
        let first = tree_hash(&dir).unwrap();
        assert!(first.starts_with("sha256:"));
        assert_eq!(first.len(), 7 + 64);
        assert_eq!(tree_hash(&dir).unwrap(), first);
        std::fs::write(dir.join("models").join("x.py"), "y").unwrap();
        let changed = tree_hash(&dir).unwrap();
        assert_ne!(changed, first);
        std::fs::rename(
            dir.join("models").join("x.py"),
            dir.join("models").join("z.py"),
        )
        .unwrap();
        assert_ne!(tree_hash(&dir).unwrap(), changed);
        assert!(tree_hash(&tmp.path().join("missing")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_content_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dangling = tmp.path().join("dangling");
        std::fs::create_dir_all(&dangling).unwrap();
        std::os::unix::fs::symlink("nowhere", dangling.join("link")).unwrap();
        assert!(tree_hash(&dangling).is_err());

        let locked = tmp.path().join("locked");
        std::fs::create_dir_all(locked.join("sub")).unwrap();
        std::fs::set_permissions(locked.join("sub"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let result = tree_hash(&locked);
        std::fs::set_permissions(locked.join("sub"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn version_bumps() {
        assert_eq!(bump("17.0.1.2.0").as_deref(), Some("17.0.1.2.1"));
        assert_eq!(bump("9").as_deref(), Some("10"));
        assert_eq!(bump("17.0.1.2.x"), None);
        assert_eq!(bump(&format!("1.{}", u64::MAX)), None);
        assert_eq!(hex(&[0, 255]), "00ff");
    }

    #[test]
    fn manifest_versions_are_replaced_in_place() {
        let text = "{\n    \"name\": \"X\",\n    \"version\": \"17.0.1.2.0\",  # bumped\n}\n";
        assert_eq!(
            set_manifest_version(text, "17.0.1.2.1").unwrap(),
            "{\n    \"name\": \"X\",\n    \"version\": \"17.0.1.2.1\",  # bumped\n}\n"
        );
        assert_eq!(
            set_manifest_version("{'version' : '1.0'}", "1.1").unwrap(),
            "{'version' : '1.1'}"
        );
        assert_eq!(set_manifest_version("{'name': 'x'}", "1"), None);
        assert_eq!(set_manifest_version("{'version'", "1"), None);
        assert_eq!(set_manifest_version("{'version': 1}", "1"), None);
        assert_eq!(set_manifest_version("{'version': '1", "1"), None);
    }
}
