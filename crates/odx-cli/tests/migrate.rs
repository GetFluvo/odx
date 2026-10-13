//! `odx migrate` on a clone of a repository with the addon on 18.0 and an
//! unrelated 19.0.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=Odx", "-c", "user.email=odx@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "commit",
            "--quiet",
            "--author",
            "Jane <jane@example.com>",
            "-m",
            message,
        ],
    );
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// An upstream with `my_addon` on 18.0 and an unrelated 19.0, and a clone
/// of it on 19.0.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
    write(
        &up.join("my_addon/__manifest__.py"),
        "{\"version\": \"18.0.1.0.0\"}\n",
    );
    commit(&up, "[ADD] my_addon");
    write(&up.join("my_addon/data.xml"), "<odoo/>\n");
    commit(&up, "[IMP] my_addon: data");
    git(&up, &["checkout", "--quiet", "--orphan", "19.0"]);
    git(&up, &["rm", "-r", "--quiet", "-f", "."]);
    write(&up.join(".pre-commit-config.yaml"), "repos: []\n");
    commit(&up, "[INIT] 19.0");
    git(&up, &["checkout", "--quiet", "--detach"]);
    let work = tmp.path().join("work");
    git(
        tmp.path(),
        &[
            "clone",
            "--quiet",
            "--branch",
            "19.0",
            up.to_str().unwrap(),
            "work",
        ],
    );
    git(&work, &["config", "user.name", "Me"]);
    git(&work, &["config", "user.email", "me@example.com"]);
    (tmp, up, work)
}

fn env() -> MapEnv {
    MapEnv::default()
        .with("ODX_ODL", "odx-test-no-such-program")
        .with("ODX_PRE_COMMIT", "odx-test-no-such-program")
}

fn odx(dir: &Path, env: &MapEnv, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(command_line, &OsFs, env, dir, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn migrates_an_addon() {
    let (_tmp, up, work) = fixture();
    let env = env();
    let (exit, out, err) = odx(
        &work,
        &env,
        &["migrate", "addon", "my_addon", "--to", "19.0"],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(
        out,
        "migrated on branch 19.0-mig-my_addon: 2 commit(s) of history\n  \
         [MIG] my_addon: Migration to 19.0\n\
         odoo-lint finds nothing left to do by hand\n\
         next: fix what is left, run the tests, then push the branch and open the pull request\n"
    );
    assert!(err.contains("the pre-commit fixes are left out"), "{err}");
    assert!(err.contains("install odoo-lint"), "{err}");
    assert_eq!(git(&work, &["log", "-1", "--format=%an", "HEAD~1"]), "Jane");

    // An `upstream` remote is preferred; the JSON names the branch.
    git(&work, &["checkout", "--quiet", "19.0"]);
    git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
    git(&work, &["remote", "add", "upstream", up.to_str().unwrap()]);
    git(&work, &["remote", "set-url", "origin", "/nowhere"]);
    let (exit, out, err) = odx(
        &work,
        &env,
        &[
            "--json",
            "migrate",
            "addon",
            "my_addon",
            "--to",
            "19.0",
            "--no-pre-commit",
        ],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "done");
    assert_eq!(value["branch"], "19.0-mig-my_addon");
    assert!(!err.contains("pre-commit"), "{err}");
}

#[test]
fn usage_and_refusals() {
    let (_tmp, _up, work) = fixture();
    let env = env();
    let (exit, _, err) = odx(&work, &env, &["migrate"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("odx migrate addon <addon> --to <series>"),
        "{err}"
    );
    let (exit, _, err) = odx(&work, &env, &["migrate", "addon", "my_addon", "--to", "x"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("cannot tell the series before x"), "{err}");
    let (exit, _, err) = odx(
        &work,
        &env,
        &["migrate", "addon", "nope", "--to", "19.0", "--from", "18.0"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("nope is not on origin/18.0"), "{err}");
    let (exit, _, err) = odx(&work, &env, &["migrate", "--abort"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("no migration is in progress"), "{err}");
}

#[test]
fn conflicts_continue_and_abort() {
    let (_tmp, _up, work) = fixture();
    write(&work.join("my_addon/data.xml"), "<odoo><data/></odoo>\n");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "--quiet", "-m", "data"]);
    git(&work, &["push", "--quiet", "origin", "HEAD:19.0"]);
    let env = env();
    let start = [
        "migrate", "addon", "my_addon", "--to", "19.0", "--remote", "origin",
    ];
    let (exit, out, err) = odx(&work, &env, &start);
    assert_eq!((exit, out.as_str()), (Exit::Failure, ""));
    assert!(err.contains("odx migrate --continue"), "{err}");
    let (exit, out, _) = odx(&work, &env, &["--json", "migrate", "--continue"]);
    assert_eq!(exit, Exit::Failure);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "stopped");
    let (exit, out, _) = odx(&work, &env, &["migrate", "--abort"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "aborted the migration of my_addon: deleted 19.0-mig-my_addon, back on 19.0\n"
    );
    odx(&work, &env, &start);
    let (_, out, _) = odx(&work, &env, &["--json", "migrate", "--abort"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["status"], "aborted");
    // Skipped by hand, continuing finishes.
    odx(&work, &env, &start);
    git(&work, &["am", "--skip"]);
    let (exit, out, err) = odx(&work, &env, &["migrate", "--continue"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.starts_with("migrated on branch 19.0-mig-my_addon: 1 commit(s)"),
        "{out}"
    );
}

#[cfg(unix)]
#[test]
fn findings_left_by_hand() {
    use std::os::unix::fs::PermissionsExt as _;
    let (tmp, _up, work) = fixture();
    let odl = tmp.path().join("odl");
    std::fs::write(
        &odl,
        r#"#!/bin/sh
case "$*" in
  *--fix*) exit 1;;
  *) echo '{"modules":[{"name":"my_addon","violations":[{"file_path":"my_addon/data.xml","line":3,"column":1,"code":"U1908","name":"x","message":"use odoo.fields.Domain"}]}]}'; exit 1;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = env().with("ODX_ODL", odl.display().to_string());
    let (exit, out, err) = odx(
        &work,
        &env,
        &[
            "migrate",
            "addon",
            "my_addon",
            "--to",
            "19.0",
            "--no-pre-commit",
        ],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.contains(
            "1 finding(s) left to fix by hand:\n  my_addon/data.xml:3: U1908 use odoo.fields.Domain\n"
        ),
        "{out}"
    );
    assert_eq!(err, "");
}

type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

/// A server on localhost answering from `routes` by path, 404 otherwise.
fn serve(routes: &Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::clone(routes);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend(buffer.iter().take(n)),
                }
            }
            let text = String::from_utf8_lossy(&request);
            let target = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let path = target.split('?').next().unwrap_or(&target).to_owned();
            let (status, body) = routes
                .lock()
                .unwrap()
                .get(&path)
                .cloned()
                .unwrap_or((404, String::new()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

/// The binary, with the forge URL of the remote redirected to the upstream
/// directory for git.
fn binary(work: &Path, up: &Path, api: &str, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_odx"))
        .current_dir(work)
        .args(args)
        .env("ODX_ODL", "odx-test-no-such-program")
        .env("ODX_PRE_COMMIT", "odx-test-no-such-program")
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", file_url(up)),
        )
        .env("GIT_CONFIG_VALUE_0", format!("{api}/OCA/repo.git"))
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn an_open_migration_pull_request_comes_first() {
    let (_tmp, up, work) = fixture();
    let routes: Routes = Arc::default();
    let api = serve(&routes);
    let route = |path: &str, status: u16, body: &str| {
        routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    };
    route("/api/v3/meta", 200, "{}");
    route(
        "/api/v3/repos/OCA/repo/pulls",
        200,
        r#"[{"number":7,"title":"[19.0][MIG] my_addon: Migration to 19.0","state":"open","base":{"ref":"19.0"}}]"#,
    );
    git(
        &work,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("{api}/OCA/repo.git"),
        ],
    );
    let start = ["migrate", "addon", "my_addon", "--to", "19.0"];
    let (ok, _, err) = binary(&work, &up, &api, &start);
    assert!(!ok);
    assert!(
        err.contains("OCA/repo#7 already migrates my_addon to 19.0"),
        "{err}"
    );
    // Offline or forced, the migration goes ahead.
    let (ok, _, err) = binary(
        &work,
        &up,
        &api,
        &[
            "--offline",
            start[0],
            start[1],
            start[2],
            start[3],
            start[4],
        ],
    );
    assert!(ok, "{err}");
    git(&work, &["checkout", "--quiet", "19.0"]);
    git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
    // Without such a pull request, it goes ahead too.
    route("/api/v3/repos/OCA/repo/pulls", 200, "[]");
    let (ok, _, err) = binary(&work, &up, &api, &start);
    assert!(ok, "{err}");
    git(&work, &["checkout", "--quiet", "19.0"]);
    git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
    // A forge that cannot answer is a warning.
    route("/api/v3/repos/OCA/repo/pulls", 500, "");
    let (ok, _, err) = binary(&work, &up, &api, &start);
    assert!(ok, "{err}");
    assert!(
        err.contains("warning: migrate: cannot look for an open migration pull request: "),
        "{err}"
    );
    git(&work, &["checkout", "--quiet", "19.0"]);
    git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
    let mut forced = start.to_vec();
    forced.push("--force");
    let (ok, _, err) = binary(&work, &up, &api, &forced);
    assert!(ok, "{err}");
}
