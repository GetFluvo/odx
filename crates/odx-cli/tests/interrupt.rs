//! Ctrl-C during `odx aggregate --fetch-only` leaves no git lock files.
//!
//! A `git` wrapper on `PATH` makes every fetch slow; SIGINT arrives while
//! the first fetch runs. The running fetch finishes, pending ones are
//! cancelled, and no `*.lock` file remains in the cache repositories.

#![cfg(unix)]
#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn lock_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(lock_files(&path));
        } else if path.extension().is_some_and(|e| e == "lock") {
            found.push(path.display().to_string());
        }
    }
    found
}

#[test]
fn ctrl_c_leaves_no_git_lock_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mut repos = String::new();
    for name in ["a", "b", "c"] {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet", "--initial-branch=17.0"]);
        std::fs::write(dir.join("f"), name).unwrap();
        git(&dir, &["add", "f"]);
        git(&dir, &["commit", "--quiet", "-m", name]);
        let _ = write!(
            repos,
            "[[tool.odx.repositories]]\nname = \"{name}\"\nurl = \"file://{}\"\n",
            dir.display()
        );
    }
    let project = tmp.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        format!("[tool.odx]\nodoo_version = \"17.0\"\njobs = 1\n{repos}"),
    )
    .unwrap();

    let real_git = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("git");
    let marker = tmp.path().join("fetch-started");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \"$*\" in *fetch*) touch '{}'; sleep 1;; esac\nexec {} \"$@\"\n",
            marker.display(),
            real_git.trim()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cache = tmp.path().join("cache");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let child = Command::new(env!("CARGO_BIN_EXE_odx"))
        .args(["aggregate", "--fetch-only"])
        .current_dir(&project)
        .env("PATH", path)
        .env("ODX_CACHE_DIR", &cache)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(25));
        marker.exists()
    });
    assert!(started, "the first fetch never started");
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cancelled"), "{stderr}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("fetched a: 1 refs"),
        "the running fetch finishes: {stdout}"
    );
    assert_eq!(lock_files(&cache.join("repos")), Vec::<String>::new());
}
