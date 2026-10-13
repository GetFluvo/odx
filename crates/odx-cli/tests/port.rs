//! `odx port` on a clone of a repository whose 18.0 has a merged fix that
//! 19.0 lacks.

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

fn commit(dir: &Path, author: &str, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(
        dir,
        &[
            "commit",
            "--quiet",
            "--author",
            &format!("{author} <{}@example.com>", author.to_lowercase()),
            "-m",
            message,
        ],
    );
    git(dir, &["rev-parse", "HEAD"])
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// An upstream whose 19.0 has `my_addon` migrated from 18.0, and whose
/// 18.0 then merged pull request #5 (a fix by Jane) and a translation
/// update; a clone of it on 19.0. Returns the fix too.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
    write(&up.join("my_addon/__manifest__.py"), "{}\n");
    write(&up.join("my_addon/data.xml"), "<odoo/>\n");
    commit(&up, "Joe", "[ADD] my_addon");
    git(&up, &["checkout", "--quiet", "-b", "19.0"]);
    write(
        &up.join("my_addon/__manifest__.py"),
        "{\"version\": \"19.0.1.0.0\"}\n",
    );
    write(
        &up.join(".oca/oca-port/blacklist/my_addon.json"),
        "{\"pull_requests\": {\"OCA/repo#99\": \"not needed\"}}\n",
    );
    commit(&up, "Joe", "[MIG] my_addon: Migration to 19.0");
    git(&up, &["checkout", "--quiet", "-b", "fix", "18.0"]);
    write(&up.join("my_addon/data.xml"), "<odoo><data/></odoo>\n");
    let fix = commit(&up, "Jane", "[FIX] my_addon: data");
    git(&up, &["checkout", "--quiet", "18.0"]);
    git(
        &up,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "-m",
            "Merge PR #5 into 18.0",
            "fix",
        ],
    );
    git(&up, &["update-ref", "refs/pull/5/head", &fix]);
    write(&up.join("my_addon/i18n/nl.po"), "msgid \"\"\n");
    commit(&up, "Weblate", "Translated using Weblate (Dutch)");
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
    (tmp, up, work, fix)
}

fn env() -> MapEnv {
    MapEnv::default().with("ODX_ODL", "odx-test-no-such-program")
}

fn odx(dir: &Path, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(command_line, &OsFs, &env(), dir, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

const ADDON: &[&str] = &[
    "port", "addon", "my_addon", "--from", "18.0", "--to", "19.0",
];

#[test]
fn ports_what_an_addon_lacks() {
    let (_tmp, _up, work, _fix) = fixture();
    let (exit, out, err) = odx(&work, ADDON);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("port: "), "{out}");
    assert!(out.contains("#5 [FIX] my_addon: data\n"), "{out}");
    assert!(out.contains("translations: "), "{out}");
    assert!(
        out.contains(
            "ported on branch 19.0-port-my_addon: 1 commit(s) cherry-picked\n\
             odoo-lint finds nothing left to do by hand\n"
        ),
        "{out}"
    );
    assert!(err.contains("install odoo-lint"), "{err}");
    assert_eq!(git(&work, &["log", "-1", "--format=%an"]), "Jane");

    // Once 19.0 has it, there is nothing left to port.
    git(
        &work,
        &["push", "--quiet", "origin", "19.0-port-my_addon:19.0"],
    );
    git(&work, &["checkout", "--quiet", "19.0"]);
    git(&work, &["branch", "-D", "19.0-port-my_addon"]);
    let (exit, out, _) = odx(&work, ADDON);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.ends_with("nothing of my_addon on 18.0 to port to 19.0\n"),
        "{out}"
    );
    let mut json = vec!["--json"];
    json.extend_from_slice(ADDON);
    let (_, out, _) = odx(&work, &json);
    assert_eq!(out, "{\"status\":\"nothing to port\"}\n");
}

#[test]
fn conflicts_continue_and_abort() {
    let (_tmp, _up, work, _fix) = fixture();
    write(&work.join("my_addon/data.xml"), "<odoo><record/></odoo>\n");
    git(&work, &["commit", "--quiet", "-am", "record"]);
    git(&work, &["push", "--quiet", "origin", "HEAD:19.0"]);
    let (exit, _, err) = odx(&work, ADDON);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx port --continue"), "{err}");
    let (exit, out, _) = odx(&work, &["--json", "port", "--continue"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("\"status\":\"stopped\""), "{out}");
    let (exit, out, _) = odx(&work, &["port", "--abort"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "aborted the port: deleted 19.0-port-my_addon, back on 19.0\n"
    );
    let (exit, _, err) = odx(&work, &["port", "--abort"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("no port is in progress"), "{err}");
    odx(&work, ADDON);
    let (_, out, _) = odx(&work, &["--json", "port", "--abort"]);
    assert!(out.contains("\"status\":\"aborted\""), "{out}");
    // Resolved, continuing finishes.
    odx(&work, ADDON);
    write(
        &work.join("my_addon/data.xml"),
        "<odoo><data/><record/></odoo>\n",
    );
    git(&work, &["add", "-A"]);
    let (exit, out, err) = odx(&work, &["--json", "port", "--continue"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("\"picked\":1"), "{out}");
}

#[test]
fn usage_and_refusals() {
    let (_tmp, _up, work, _fix) = fixture();
    let refused = |args: &[&str], exit: Exit, expected: &str| {
        let (code, _, err) = odx(&work, args);
        assert_eq!(code, exit, "{err}");
        assert!(err.contains(expected), "{err}");
    };
    refused(&["port"], Exit::Usage, "name what to port");
    refused(
        &["port", "sweep", "--to", "19.0", "--from", "18.0"],
        Exit::Usage,
        "not implemented yet",
    );
    refused(
        &["port", "pr", "x#1", "--to", "19.0"],
        Exit::Usage,
        "<owner>/<repo>#<number>",
    );
    refused(
        &["port", "pr", "OCA/repo#5", "--to", "19.0"],
        Exit::Usage,
        "no remote of this clone points at OCA/repo",
    );
    refused(
        &[
            "port",
            "pr",
            "OCA/repo#5",
            "--to",
            "19.0",
            "--remote",
            "origin",
        ],
        Exit::Failure,
        "the remote origin is not on a forge",
    );
    let mut nowhere = ADDON.to_vec();
    nowhere.extend_from_slice(&["--remote", "nowhere"]);
    refused(&nowhere, Exit::Failure, "cannot fetch from nowhere");
    write(&work.join("my_addon/__manifest__.py"), "changed\n");
    refused(ADDON, Exit::Failure, "uncommitted changes");
    git(&work, &["checkout", "--quiet", "--", "."]);
    // The project's classification settings apply.
    write(
        &work.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"19.0\"\n\n[tool.odx.port]\nmigration_commit_patterns = [\"(\"]\n",
    );
    refused(ADDON, Exit::Usage, "migration_commit_patterns");
    std::fs::remove_file(work.join("pyproject.toml")).unwrap();
    // A pull request wanted from a remote off the forges: the port is kept.
    let mut open = ADDON.to_vec();
    open.push("--open-pr");
    let (exit, out, err) = odx(&work, &open);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("ported on branch"), "{out}");
    assert!(err.contains("the remote origin is not on a forge"), "{err}");
}

#[test]
fn blacklist_entries() {
    let (tmp, _up, work, _fix) = fixture();
    // A clone of a local repository names it after its last two directories.
    let key = format!("{}/up#5", tmp.path().file_name().unwrap().to_string_lossy());
    let (exit, out, err) = odx(
        &work,
        &[
            "port",
            "blacklist",
            "my_addon",
            &key,
            "--reason",
            "not for 19.0",
        ],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(out, "[FW] my_addon: blacklist 1 port suggestion(s)\n");
    let (exit, _, _) = odx(
        &work,
        &[
            "port",
            "blacklist",
            "my_addon",
            "orphaned",
            "--reason",
            "migration commits",
        ],
    );
    assert_eq!(exit, Exit::Success);
    let text = std::fs::read_to_string(work.join(".oca/oca-port/blacklist/my_addon.json")).unwrap();
    assert!(
        text.contains(&format!("\"{key}\": \"not for 19.0\"")),
        "{text}"
    );
    assert!(
        text.contains("\"orphaned_commits\": \"migration commits\""),
        "{text}"
    );
    // Once on the target branch, the port skips what the blacklist names.
    git(&work, &["push", "--quiet", "origin", "HEAD:19.0"]);
    let (_, out, _) = odx(
        &work,
        &[
            "port", "addon", "my_addon", "--from", "18.0", "--to", "19.0",
        ],
    );
    assert!(out.contains("blacklisted: "), "{out}");
    assert!(out.contains("nothing of my_addon"), "{out}");
    for (args, exit, expected) in [
        (
            ["my_addon", "OCA/repo#5", "--reason", " "],
            Exit::Usage,
            "needs a reason",
        ),
        (
            ["my_addon", "repo", "--reason", "r"],
            Exit::Usage,
            "or `orphaned`",
        ),
        (
            ["my/addon", "OCA/repo#5", "--reason", "r"],
            Exit::Failure,
            "blacklist",
        ),
    ] {
        let mut argv = vec!["port", "blacklist"];
        argv.extend_from_slice(&args);
        let (code, _, err) = odx(&work, &argv);
        assert_eq!(code, exit, "{err}");
        assert!(err.contains(expected), "{err}");
    }
}

type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

/// A server on localhost answering from `routes` by method and path, or
/// path alone, 404 otherwise.
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
            let mut words = text.split_whitespace();
            let method = words.next().unwrap_or("GET").to_owned();
            let target = words.next().unwrap_or("/").to_owned();
            let path = target.split('?').next().unwrap_or(&target).to_owned();
            let (status, body) = {
                let routes = routes.lock().unwrap();
                routes
                    .get(&format!("{method} {path}"))
                    .or_else(|| routes.get(&path))
                    .cloned()
                    .unwrap_or((404, String::new()))
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

/// The clone with its remote on a local forge that knows pull request #5.
struct Forge {
    tmp: tempfile::TempDir,
    up: PathBuf,
    work: PathBuf,
    api: String,
    routes: Routes,
}

impl Forge {
    fn new() -> Self {
        let (tmp, up, work, fix) = fixture();
        let routes: Routes = Arc::default();
        let api = serve(&routes);
        let forge = Self {
            tmp,
            up,
            work,
            api,
            routes,
        };
        forge.route("/api/v3/meta", 200, "{}");
        forge.route(
            "/api/v3/repos/OCA/repo/pulls/5",
            200,
            r#"{"number":5,"title":"[FIX] my_addon: data","state":"closed","merged_at":"2024-03-01","base":{"ref":"18.0"},"head":{"sha":"x"}}"#,
        );
        forge.route(
            "/api/v3/repos/OCA/repo/pulls/5/commits",
            200,
            &format!(r#"[{{"sha":"{fix}"}}]"#),
        );
        forge.route(
            "GET /api/v3/repos/OCA/repo",
            200,
            r#"{"permissions":{"push":true}}"#,
        );
        forge.route(
            "POST /api/v3/repos/OCA/repo/pulls",
            201,
            &format!(
                r#"{{"number":6,"html_url":"{}/OCA/repo/pull/6"}}"#,
                forge.api
            ),
        );
        let url = format!("{}/OCA/repo.git", forge.api);
        git(&forge.work, &["remote", "set-url", "origin", &url]);
        forge
    }

    fn route(&self, key: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(key.to_owned(), (status, body.to_owned()));
    }

    /// The binary, with the remote's forge URL redirected to the upstream
    /// directory for git.
    fn odx(&self, odl: &str, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_odx"))
            .current_dir(&self.work)
            .args(args)
            .env("ODX_ODL", odl)
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.{}.insteadOf", file_url(&self.up)),
            )
            .env("GIT_CONFIG_VALUE_0", format!("{}/OCA/repo.git", self.api))
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

const PR: &[&str] = &["port", "pr", "OCA/repo#5", "--to", "19.0"];
const NO_ODL: &str = "odx-test-no-such-program";

#[test]
fn ports_a_pull_request_and_proposes_it() {
    let forge = Forge::new();
    let mut args = PR.to_vec();
    args.push("--open-pr");
    let (ok, out, err) = forge.odx(NO_ODL, &args);
    assert!(ok, "{err}");
    assert!(
        out.starts_with("ported on branch 19.0-port-5: 1 commit(s) cherry-picked\n"),
        "{out}"
    );
    assert!(
        out.ends_with(&format!(
            "pushed to origin\nopened {}/OCA/repo/pull/6 as a draft\n",
            forge.api
        )),
        "{out}"
    );
    git(
        &forge.up,
        &["rev-parse", "--verify", "--quiet", "19.0-port-5"],
    );
    // In JSON, again on a fresh branch name.
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    git(&forge.work, &["branch", "-D", "19.0-port-5"]);
    git(&forge.up, &["branch", "-D", "19.0-port-5"]);
    let mut json = vec!["--json"];
    json.extend_from_slice(&args);
    json.extend_from_slice(&["--push-remote", "origin", "--no-migrate"]);
    let (ok, out, err) = forge.odx(NO_ODL, &json);
    assert!(ok, "{err}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["pull_request"]["number"], 6);
    assert_eq!(value["warnings"], serde_json::json!([]));
}

#[test]
fn pull_request_failures() {
    let forge = Forge::new();
    forge.route("/api/v3/repos/OCA/repo/pulls/5/commits", 500, "");
    let (ok, _, err) = forge.odx(NO_ODL, PR);
    assert!(!ok);
    assert!(err.contains("answered HTTP 500"), "{err}");
    // A head the remote does not have.
    forge.route(
        "/api/v3/repos/OCA/repo/pulls/9",
        200,
        r#"{"number":9,"title":"t","state":"open","base":{"ref":"18.0"}}"#,
    );
    forge.route("/api/v3/repos/OCA/repo/pulls/9/commits", 200, "[]");
    let (ok, _, err) = forge.odx(NO_ODL, &["port", "pr", "OCA/repo#9", "--to", "19.0"]);
    assert!(!ok);
    assert!(err.contains("cannot fetch from origin"), "{err}");
    // A pull request the forge does not know, a forge that is none.
    let (ok, _, err) = forge.odx(NO_ODL, &["port", "pr", "OCA/repo#7", "--to", "19.0"]);
    assert!(!ok);
    assert!(err.contains("cannot see"), "{err}");
    // Commits the clone cannot find.
    forge.route(
        "/api/v3/repos/OCA/repo/pulls/5/commits",
        200,
        r#"[{"sha":"0123456789012345678901234567890123456789"}]"#,
    );
    let (ok, _, err) = forge.odx(NO_ODL, PR);
    assert!(!ok);
    assert!(err.contains("cannot read the commits"), "{err}");
    forge.route("/api/v3/meta", 404, "");
    let (ok, _, err) = forge.odx(NO_ODL, PR);
    assert!(!ok);
    assert!(err.contains("does not look like GitHub"), "{err}");
}

#[test]
fn gitlab_merge_requests() {
    let forge = Forge::new();
    let fix = git(&forge.up, &["rev-parse", "refs/pull/5/head"]);
    git(
        &forge.up,
        &["update-ref", "refs/merge-requests/5/head", &fix],
    );
    forge.route("/api/v3/meta", 404, "");
    forge.route("/api/v4/version", 200, "{}");
    forge.route(
        "/api/v4/projects/OCA%2Frepo/merge_requests/5",
        200,
        r#"{"iid":5,"title":"[FIX] my_addon: data","state":"merged","target_branch":"18.0","sha":"x"}"#,
    );
    forge.route(
        "/api/v4/projects/OCA%2Frepo/merge_requests/5/commits",
        200,
        &format!(r#"[{{"id":"{fix}"}}]"#),
    );
    let (ok, out, err) = forge.odx(NO_ODL, &["port", "pr", "OCA/repo!5", "--to", "19.0"]);
    assert!(!ok);
    assert!(err.contains("<owner>/<repo>#<number>"), "{err}{out}");
    let (ok, out, err) = forge.odx(NO_ODL, PR);
    assert!(ok, "{err}");
    assert!(
        out.starts_with("ported on branch 19.0-port-5: 1 commit(s)"),
        "{out}"
    );
}

#[cfg(unix)]
#[test]
fn findings_go_to_the_description() {
    use std::os::unix::fs::PermissionsExt as _;
    let forge = Forge::new();
    let odl = forge.tmp.path().join("odl");
    std::fs::write(
        &odl,
        "#!/bin/sh\ncase \"$*\" in\n  *--fix*) echo '<!-- fixed -->' >> my_addon/data.xml; exit 1;;\n  *) echo '{\"modules\":[{\"violations\":[{\"file_path\":\"my_addon/data.xml\",\"line\":1,\"code\":\"U1\",\"message\":\"m\"}]}]}';;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut args = PR.to_vec();
    args.push("--open-pr");
    let (ok, out, err) = forge.odx(&odl.display().to_string(), &args);
    assert!(ok, "{err}");
    assert!(
        out.contains(
            "  [FW] my_addon: apply 19.0 migration fixes\n\
             1 finding(s) left to fix by hand:\n  my_addon/data.xml:1: U1 m\n"
        ),
        "{out}"
    );
}
