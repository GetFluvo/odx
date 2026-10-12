//! In-memory implementations of the IO traits, for tests.
//!
//! Enabled with the `fakes` feature. Downstream crates add `odx-core` with
//! `features = ["fakes"]` to their dev-dependencies.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use crate::{Clock, Env, FileSystem, Invocation, Output, ProcessRunner};

/// Lock a mutex, recovering the data if another test thread panicked.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A [`Clock`] that always returns the same instant.
///
/// ```
/// use odx_core::fake::FixedClock;
/// use odx_core::Clock;
/// use std::time::{Duration, SystemTime};
///
/// let at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
/// assert_eq!(FixedClock::new(at).now(), at);
/// ```
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(SystemTime);

impl FixedClock {
    /// A clock frozen at `at`.
    pub const fn new(at: SystemTime) -> Self {
        Self(at)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        self.0
    }
}

/// An [`Env`] backed by a map.
///
/// ```
/// use odx_core::fake::MapEnv;
/// use odx_core::Env;
///
/// let env = MapEnv::default().with("GITHUB_TOKEN", "t0ken");
/// assert_eq!(env.var("GITHUB_TOKEN").as_deref(), Some("t0ken"));
/// assert_eq!(env.var("GH_TOKEN"), None);
/// ```
#[derive(Debug, Clone, Default)]
pub struct MapEnv(BTreeMap<String, String>);

impl MapEnv {
    /// Add or replace a variable.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }
}

impl Env for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

/// A [`FileSystem`] held in memory, with failure injection.
///
/// ```
/// use odx_core::fake::MemoryFs;
/// use odx_core::FileSystem;
/// use std::io::ErrorKind;
/// use std::path::Path;
///
/// let fs = MemoryFs::default();
/// fs.write(Path::new("odx.lock"), b"version = 1\n").unwrap();
/// assert_eq!(fs.read(Path::new("odx.lock")).unwrap(), b"version = 1\n");
///
/// fs.fail_next(ErrorKind::PermissionDenied);
/// assert!(fs.read(Path::new("odx.lock")).is_err());
/// ```
#[derive(Debug, Default)]
pub struct MemoryFs {
    files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    failures: Mutex<VecDeque<io::ErrorKind>>,
    failing_paths: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
    failing_writes: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
}

impl MemoryFs {
    /// Make the next `read` or `write` call fail with `kind`.
    ///
    /// Calls queue up: each failing call consumes one entry.
    pub fn fail_next(&self, kind: io::ErrorKind) {
        lock(&self.failures).push_back(kind);
    }

    /// Make every `read` and `write` of `path` fail with `kind`.
    pub fn fail_on(&self, path: impl Into<PathBuf>, kind: io::ErrorKind) {
        lock(&self.failing_paths).insert(path.into(), kind);
    }

    /// Make every `write` of `path` fail with `kind`; reads still succeed.
    pub fn fail_writes_to(&self, path: impl Into<PathBuf>, kind: io::ErrorKind) {
        lock(&self.failing_writes).insert(path.into(), kind);
    }

    fn injected_failure(&self, path: &Path) -> io::Result<()> {
        if let Some(kind) = lock(&self.failing_paths).get(path) {
            return Err(io::Error::from(*kind));
        }
        lock(&self.failures)
            .pop_front()
            .map_or(Ok(()), |kind| Err(io::Error::from(kind)))
    }
}

impl FileSystem for MemoryFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.injected_failure(path)?;
        lock(&self.files)
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        self.injected_failure(path)?;
        if let Some(kind) = lock(&self.failing_writes).get(path) {
            return Err(io::Error::from(*kind));
        }
        lock(&self.files).insert(path.to_path_buf(), contents.to_vec());
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        lock(&self.files).contains_key(path)
    }
}

/// A [`ProcessRunner`] that replays queued results and records invocations.
///
/// ```
/// use odx_core::fake::ScriptedRunner;
/// use odx_core::{Invocation, Output, ProcessRunner};
///
/// let runner = ScriptedRunner::default();
/// runner.push(Ok(Output { status: Some(0), stdout: b"ok".to_vec(), ..Output::default() }));
///
/// let out = runner.run(&Invocation::new("git").arg("--version")).unwrap();
/// assert!(out.success());
/// assert_eq!(runner.invocations()[0].program(), "git");
/// ```
#[derive(Debug, Default)]
pub struct ScriptedRunner {
    results: Mutex<VecDeque<io::Result<Output>>>,
    seen: Mutex<Vec<Invocation>>,
}

impl ScriptedRunner {
    /// Queue the result of the next `run` call.
    pub fn push(&self, result: io::Result<Output>) {
        lock(&self.results).push_back(result);
    }

    /// Every invocation received so far, in order.
    pub fn invocations(&self) -> Vec<Invocation> {
        lock(&self.seen).clone()
    }
}

impl ProcessRunner for ScriptedRunner {
    /// Replays the next queued result.
    ///
    /// When nothing is queued the call fails with
    /// [`io::ErrorKind::NotFound`], as if the program were missing.
    fn run(&self, invocation: &Invocation) -> io::Result<Output> {
        lock(&self.seen).push(invocation.clone());
        lock(&self.results)
            .pop_front()
            .unwrap_or_else(|| Err(io::Error::from(io::ErrorKind::NotFound)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn memory_fs_reports_missing_files_and_existence() {
        let fs = MemoryFs::default();
        let path = Path::new("pyproject.toml");
        assert!(!fs.exists(path));
        assert_eq!(fs.read(path).unwrap_err().kind(), io::ErrorKind::NotFound);
        fs.write(path, b"[tool.odx]\n").unwrap();
        assert!(fs.exists(path));
    }

    #[test]
    fn memory_fs_write_failure_is_consumed_once() {
        let fs = MemoryFs::default();
        fs.fail_next(io::ErrorKind::StorageFull);
        let path = Path::new("odx.lock");
        assert_eq!(
            fs.write(path, b"x").unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        fs.write(path, b"x").unwrap();
        assert!(fs.exists(path));
    }

    #[test]
    fn memory_fs_fails_on_a_path_until_the_end() {
        let fs = MemoryFs::default();
        fs.fail_on("out.yaml", io::ErrorKind::ReadOnlyFilesystem);
        for _ in 0..2 {
            let err = fs.write(Path::new("out.yaml"), b"x").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::ReadOnlyFilesystem);
        }
        assert_eq!(
            fs.read(Path::new("out.yaml")).unwrap_err().kind(),
            io::ErrorKind::ReadOnlyFilesystem
        );
        fs.write(Path::new("other.yaml"), b"x").unwrap();
    }

    #[test]
    fn memory_fs_can_fail_only_writes() {
        let fs = MemoryFs::default();
        fs.write(Path::new("p.toml"), b"x").unwrap();
        fs.fail_writes_to("p.toml", io::ErrorKind::ReadOnlyFilesystem);
        assert_eq!(fs.read(Path::new("p.toml")).unwrap(), b"x");
        assert_eq!(
            fs.write(Path::new("p.toml"), b"y").unwrap_err().kind(),
            io::ErrorKind::ReadOnlyFilesystem
        );
    }

    #[test]
    fn scripted_runner_without_results_reports_missing_program() {
        let runner = ScriptedRunner::default();
        let err = runner.run(&Invocation::new("mergiraf")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(runner.invocations().len(), 1);
    }

    #[test]
    fn lock_recovers_from_poisoned_mutex() {
        let shared = Arc::new(Mutex::new(1));
        let poisoner = Arc::clone(&shared);
        let joined = thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the mutex");
        })
        .join();
        assert!(joined.is_err());
        assert_eq!(*lock(&shared), 1);
    }
}
