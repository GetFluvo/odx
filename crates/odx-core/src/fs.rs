use std::io;
use std::path::Path;

/// Minimal file system access used by the odx library crates.
pub trait FileSystem: std::fmt::Debug + Send + Sync {
    /// Read the whole file at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file does not exist or cannot be read.
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Create or replace the file at `path` with `contents`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be written.
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;

    /// Whether a file or directory exists at `path`.
    fn exists(&self, path: &Path) -> bool;
}

/// The text of the file at `path`, none when nothing is there.
///
/// Nothing is there when the file, or a folder on its way, is missing. A
/// file that is there but cannot be read is an error, so an edit does not
/// start over from nothing.
///
/// # Errors
///
/// When the file exists but cannot be read, or is not UTF-8.
pub fn read_if_exists(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        read => read.map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_that_are_there_or_not() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a");
        assert_eq!(read_if_exists(&file).unwrap(), None);
        std::fs::write(&file, "text").unwrap();
        assert_eq!(read_if_exists(&file).unwrap().as_deref(), Some("text"));
        // Below a file, nothing can be there.
        assert_eq!(read_if_exists(&file.join("b")).unwrap(), None);
        std::fs::write(&file, b"\xff").unwrap();
        assert!(read_if_exists(&file).is_err());
        assert!(read_if_exists(tmp.path()).is_err());
    }
}
