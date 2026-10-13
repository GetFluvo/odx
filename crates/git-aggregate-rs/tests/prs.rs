//! `show-closed-prs` and `show-all-prs` against a local forge API.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::{Read, Write};
use std::net::TcpListener;

use git_aggregate_rs::{Context, run_with};
use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

/// A GitHub Enterprise-like API with pull requests 1 (open), 2 (merged)
/// and 3 (closed).
fn forge() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 2048];
            let n = stream.read(&mut request).unwrap_or(0);
            let text = String::from_utf8_lossy(request.get(..n).unwrap_or_default()).into_owned();
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let body = match path.as_str() {
                "/api/v3/meta" => "{}",
                "/api/v3/repos/OCA/pos/pulls/1" => r#"{"state": "open", "merged": false}"#,
                "/api/v3/repos/OCA/pos/pulls/2" => r#"{"state": "closed", "merged": true}"#,
                "/api/v3/repos/OCA/pos/pulls/3" => r#"{"state": "closed", "merged": false}"#,
                _ => "",
            };
            let status = if body.is_empty() { 404 } else { 200 };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

fn run(dir: &std::path::Path, args: &[&str]) -> (Exit, String, String) {
    let env = MapEnv::default();
    let ctx = Context {
        fs: &OsFs,
        env: &env,
        dir: dir.to_path_buf(),
    };
    let mut command_line = vec!["git-aggregate-rs"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = run_with(command_line, &ctx, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn pull_request_states() {
    let api = forge();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("repos.yaml"),
        format!(
            "./pos:\n  remotes:\n    oca: {api}/OCA/pos.git\n  merges:\n    - oca 17.0\n    - oca refs/pull/1/head\n    - oca refs/pull/2/head\n    - oca refs/pull/3/head\n    - oca refs/pull/4/head\n  target: oca agg\n\
             ./other:\n  remotes:\n    x: /srv/git/other\n  merges:\n    - x 17.0\n    - x refs/pull/5/head\n  target: x agg\n"
        ),
    )
    .unwrap();
    let (exit, out, err) = run(tmp.path(), &["show-closed-prs"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!(
            "{api}/OCA/pos/pull/2 in state closed (merged)\n{api}/OCA/pos/pull/3 in state closed (not merged)\n"
        )
    );
    assert!(
        err.contains("warning: ") && err.contains("refs/pull/4/head: cannot see"),
        "{err}"
    );
    assert!(
        err.contains("/srv/git/other refs/pull/5/head: not on a forge"),
        "{err}"
    );
    let (_, out, _) = run(tmp.path(), &["show-all-prs", "-d", "./pos"]);
    assert!(
        out.starts_with(&format!(
            "{api}/OCA/pos/pull/1 in state open (not merged)\n"
        )),
        "{out}"
    );
    assert_eq!(out.lines().count(), 3, "{out}");
    let (_, out, err) = run(
        tmp.path(),
        &["show-all-prs", "-d", "./other", "--log-level", "error"],
    );
    assert_eq!((out.as_str(), err.as_str()), ("", ""));
    std::fs::write(tmp.path().join("repos.yaml"), "- not a mapping\n").unwrap();
    let (exit, _, err) = run(tmp.path(), &["show-all-prs"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.starts_with("error: repos.yaml: "), "{err}");
}

#[test]
fn a_closed_output_fails_the_run() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let api = forge();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("repos.yaml"),
        format!("./pos:\n  remotes:\n    oca: {api}/OCA/pos.git\n  merges:\n    - oca 17.0\n    - oca refs/pull/1/head\n  target: oca agg\n"),
    )
    .unwrap();
    let env = MapEnv::default();
    let ctx = Context {
        fs: &OsFs,
        env: &env,
        dir: tmp.path().to_path_buf(),
    };
    let exit = run_with(
        ["git-aggregate-rs", "show-all-prs"],
        &ctx,
        &mut Broken,
        &mut Vec::new(),
    );
    assert_eq!(exit, Exit::Failure);
    let exit = run_with(
        ["git-aggregate-rs", "show-all-prs", "-c", "nowhere.yaml"],
        &ctx,
        &mut Vec::new(),
        &mut Broken,
    );
    assert_eq!(exit, Exit::Failure);
}
