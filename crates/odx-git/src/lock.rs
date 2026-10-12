use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::GitError;

/// An exclusive advisory lock on a cache repository, released when dropped
/// or when the process dies, so a crashed odx never leaves a stale lock.
#[derive(Debug)]
pub struct RepoLock {
    _file: File,
}

impl RepoLock {
    /// Take the lock at `path`, retrying until `timeout` while another
    /// process holds it.
    ///
    /// # Errors
    ///
    /// [`GitError::Locked`] after `timeout`, [`GitError::Io`] when the lock
    /// file cannot be created or locked.
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self, GitError> {
        fs::create_dir_all(path.with_file_name(""))?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(e) => retry_or_fail(e, Instant::now() >= deadline, path)?,
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Whether a failed lock attempt should be retried: only when another
/// process holds the lock and the deadline has not passed.
fn retry_or_fail(error: TryLockError, expired: bool, path: &Path) -> Result<(), GitError> {
    match (error, expired) {
        (TryLockError::WouldBlock, false) => Ok(()),
        (TryLockError::WouldBlock, true) => Err(GitError::Locked(path.display().to_string())),
        (TryLockError::Error(e), _) => Err(GitError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn exclusive_with_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks").join("x.lock");
        let held = RepoLock::acquire(&path, Duration::ZERO).unwrap();
        let err = RepoLock::acquire(&path, Duration::from_millis(120)).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{} is locked by another odx process", path.display())
        );
        drop(held);
        // Another test's child process may briefly share the descriptor
        // between fork and exec, so allow a short wait after the release.
        assert!(RepoLock::acquire(&path, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn classification() {
        let path = Path::new("x.lock");
        assert!(retry_or_fail(TryLockError::WouldBlock, false, path).is_ok());
        let locked = retry_or_fail(TryLockError::WouldBlock, true, path).unwrap_err();
        assert_eq!(
            locked.to_string(),
            "x.lock is locked by another odx process"
        );
        let other = TryLockError::Error(io::Error::from(io::ErrorKind::Unsupported));
        let err = retry_or_fail(other, false, path).unwrap_err();
        assert!(err.to_string().starts_with("cannot run git"), "{err}");
    }

    #[test]
    fn lock_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, b"").unwrap();
        assert!(RepoLock::acquire(&blocker.join("x.lock"), Duration::ZERO).is_err());
        assert!(RepoLock::acquire(&dir.path().join("sub").join(""), Duration::ZERO).is_err());
    }
}
