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
