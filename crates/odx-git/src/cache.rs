use std::path::{Path, PathBuf};

use odx_core::repo_key;

/// A stable, short hash of a repository URL: FNV-1a over [`repo_key`], so
/// `https://`, `ssh://` and scp-style spellings share one cache entry.
///
/// ```
/// use odx_git::url_hash;
///
/// assert_eq!(url_hash("https://github.com/OCA/pos.git"), url_hash("git@github.com:oca/pos"));
/// assert_eq!(url_hash("https://github.com/OCA/pos.git").len(), 16);
/// ```
pub fn url_hash(url: &str) -> String {
    let hash = repo_key(url)
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// The shared cache directory.
///
/// ```text
/// <root>/repos/<urlhash>/      bare repository per upstream URL
/// <root>/repos/<urlhash>.url   the URL, for humans and `odx cache`
/// <root>/locks/<urlhash>.lock  advisory lock per repository
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// A cache rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The cache root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The bare repository for `url`.
    pub fn repo_dir(&self, url: &str) -> PathBuf {
        self.root.join("repos").join(url_hash(url))
    }

    /// The file recording when a repository was last fetched (seconds
    /// since the epoch).
    pub fn fetched_file(&self, url: &str) -> PathBuf {
        self.root
            .join("repos")
            .join(format!("{}.fetched", url_hash(url)))
    }

    /// The file recording which URL a repository belongs to.
    pub fn url_file(&self, url: &str) -> PathBuf {
        self.root
            .join("repos")
            .join(format!("{}.url", url_hash(url)))
    }

    /// The advisory lock file for `url`.
    pub fn lock_file(&self, url: &str) -> PathBuf {
        self.root
            .join("locks")
            .join(format!("{}.lock", url_hash(url)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let cache = Cache::new("/c");
        let hash = url_hash("https://github.com/OCA/pos.git");
        assert_eq!(cache.root(), Path::new("/c"));
        assert_eq!(
            cache.repo_dir("https://github.com/OCA/pos"),
            Path::new("/c/repos").join(&hash)
        );
        assert_eq!(
            cache.url_file("https://github.com/OCA/pos"),
            Path::new("/c/repos").join(format!("{hash}.url"))
        );
        assert_eq!(
            cache.lock_file("https://github.com/OCA/pos"),
            Path::new("/c/locks").join(format!("{hash}.lock"))
        );
        assert_ne!(url_hash("https://github.com/OCA/web"), hash);
    }
}
