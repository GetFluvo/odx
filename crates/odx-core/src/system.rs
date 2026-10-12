//! Operating-system implementations of the IO traits.
//!
//! Enabled with the `system` feature. Library crates take the traits;
//! binaries (and integration tests that need real processes) construct
//! these.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::{Clock, Env, FileSystem, Invocation, Output, ProcessRunner};

/// The real file system.
///
/// ```
/// use odx_core::system::OsFs;
/// use odx_core::FileSystem;
///
/// let dir = std::env::temp_dir().join(format!("odx-doc-{}", std::process::id()));
/// std::fs::create_dir_all(&dir).unwrap();
/// let file = dir.join("x.txt");
/// OsFs.write(&file, b"hi").unwrap();
/// assert_eq!(OsFs.read(&file).unwrap(), b"hi");
/// assert!(OsFs.exists(&file));
/// std::fs::remove_dir_all(&dir).unwrap();
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct OsFs;

impl FileSystem for OsFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// Runs real processes with standard input closed and output captured.
///
/// ```
/// use odx_core::system::OsRunner;
/// use odx_core::{Invocation, ProcessRunner};
///
/// let out = OsRunner.run(&Invocation::new("git").arg("--version")).unwrap();
/// assert!(out.success());
/// assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRunner;

impl ProcessRunner for OsRunner {
    fn run(&self, invocation: &Invocation) -> io::Result<Output> {
        let mut command = Command::new(invocation.program());
        command
            .args(invocation.arguments())
            .envs(invocation.envs().iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        if let Some(dir) = invocation.working_dir() {
            command.current_dir(dir);
        }
        let output = command.output()?;
        Ok(Output {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

/// The system clock.
///
/// ```
/// use odx_core::system::SystemClock;
/// use odx_core::Clock;
///
/// assert!(SystemClock.now() > std::time::SystemTime::UNIX_EPOCH);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// The process environment.
///
/// ```
/// use odx_core::system::OsEnv;
/// use odx_core::Env;
///
/// assert_eq!(OsEnv.var("ODX_SURELY_UNSET_VARIABLE"), None);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct OsEnv;

impl Env for OsEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_passes_directory_and_environment() {
        let dir = std::env::temp_dir();
        let out = OsRunner
            .run(
                &Invocation::new("git")
                    .args(["config", "--get", "odx.probe"])
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", "odx.probe")
                    .env("GIT_CONFIG_VALUE_0", "yes")
                    .current_dir(&dir),
            )
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "yes");
    }

    #[test]
    fn runner_reports_missing_programs() {
        let err = OsRunner
            .run(&Invocation::new("odx-surely-missing-program"))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn missing_files() {
        let path = std::env::temp_dir().join("odx-surely-missing").join("x");
        assert!(OsFs.read(&path).is_err());
        assert!(!OsFs.exists(&path));
    }
}
