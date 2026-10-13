//! Fetching over the `git://` protocol from a local `git daemon`.
//!
//! Skipped (with a message) where `git daemon` is not installed locally;
//! on CI (`CI` set) a missing `git daemon` fails the test instead.

#![cfg(unix)]
#![expect(clippy::expect_used, reason = "test helpers should fail loudly")]

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use odx_core::system::OsRunner;
use odx_git::{Cache, FetchPlan, Forge, Source, SystemGit, fetch};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn fetches_over_the_git_protocol() {
    let available = Command::new("git")
        .args(["daemon", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !available {
        assert!(
            std::env::var_os("CI").is_none(),
            "`git daemon` must be available on CI so this test really runs"
        );
        eprintln!("skipped: `git daemon` is not installed");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("pos");
    std::fs::create_dir_all(&dir).expect("mkdir");
    git(&dir, &["init", "--quiet", "--initial-branch=17.0"]);
    std::fs::write(dir.join("a.txt"), "a").expect("write");
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "--quiet", "-m", "a"]);
    let base = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", "refs/pull/1/head", &base]);

    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    let _daemon = Daemon(
        Command::new("git")
            .args([
                "daemon",
                "--export-all",
                "--reuseaddr",
                "--listen=127.0.0.1",
            ])
            .arg(format!("--port={port}"))
            .arg(format!("--base-path={}", tmp.path().display()))
            .arg(tmp.path())
            .stderr(Stdio::null())
            .spawn()
            .expect("git daemon starts"),
    );
    let ready = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    assert!(ready, "git daemon did not start");

    let plan = FetchPlan {
        url: format!("git://127.0.0.1:{port}/pos"),
        forge: Forge::Github,
        sources: vec![(None, Source::Branch("17.0".into())), (None, Source::Pr(1))],
        mirror: None,
        fallback: None,
        clone: None,
        max_age: None,
    };
    let backend = SystemGit::new(Arc::new(OsRunner));
    let fetched = fetch(
        &Cache::new(tmp.path().join("cache")),
        &backend,
        &plan,
        Duration::ZERO,
    )
    .expect("fetch over git://");
    assert_eq!(fetched[0].sha, base);
    assert_eq!(fetched[1].sha, base);
}
