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
    for series in ["x", "x.0"] {
        let (exit, _, err) = odx(
            &work,
            &env,
            &["migrate", "addon", "my_addon", "--to", series],
        );
        assert_eq!(exit, Exit::Usage);
        assert!(
            err.contains(&format!("cannot tell the series before {series};")),
            "{err}"
        );
    }
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
    // Own addons belong to a project.
    let (exit, _, err) = odx(&work, &env, &["migrate", "own", "--to", "19.0"]);
    assert_eq!(exit, Exit::Usage, "{err}");
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

/// The binary, with the forge URLs of the upstream and of the fork
/// (`fork` next to `up`) redirected to their directories for git, and
/// SSH-style URLs on the forge's host to its HTTP ones.
fn binary(work: &Path, up: &Path, api: &str, args: &[&str]) -> (bool, String, String) {
    binary_with(work, up, api, "odx-test-no-such-program", args)
}

fn binary_with(
    work: &Path,
    up: &Path,
    api: &str,
    odl: &str,
    args: &[&str],
) -> (bool, String, String) {
    let fork = up.with_file_name("fork");
    let out = Command::new(env!("CARGO_BIN_EXE_odx"))
        .current_dir(work)
        .args(args)
        .env("ODX_ODL", odl)
        .env("ODX_PRE_COMMIT", "odx-test-no-such-program")
        .env("GIT_CONFIG_COUNT", "4")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", file_url(up)),
        )
        .env("GIT_CONFIG_VALUE_0", format!("{api}/OCA/repo.git"))
        .env(
            "GIT_CONFIG_KEY_1",
            format!("url.{}.insteadOf", file_url(&fork)),
        )
        .env("GIT_CONFIG_VALUE_1", format!("{api}/me/repo.git"))
        .env("GIT_CONFIG_KEY_2", format!("url.{api}/.insteadOf"))
        .env("GIT_CONFIG_VALUE_2", "git@127.0.0.1:")
        .env(
            "GIT_CONFIG_KEY_3",
            format!("url.{}.insteadOf", file_url(&fork)),
        )
        .env("GIT_CONFIG_VALUE_3", "git@127.0.0.1:me/repo.git")
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

/// The fixture with its remote on a local forge that may be pushed to,
/// and an empty bare repository for the user's fork.
struct Forge {
    tmp: tempfile::TempDir,
    up: PathBuf,
    work: PathBuf,
    api: String,
    routes: Routes,
}

impl Forge {
    fn new() -> Self {
        let (tmp, up, work) = fixture();
        git(tmp.path(), &["init", "--quiet", "--bare", "fork"]);
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
        forge.route("GET /api/v3/repos/OCA/repo/pulls", 200, "[]");
        forge.route(
            "GET /api/v3/repos/OCA/repo",
            200,
            r#"{"permissions":{"push":true}}"#,
        );
        forge.route(
            "POST /api/v3/repos/OCA/repo/pulls",
            201,
            &format!(
                r#"{{"number":9,"html_url":"{}/OCA/repo/pull/9"}}"#,
                forge.api
            ),
        );
        forge.route("GET /api/v3/user", 200, r#"{"login":"me"}"#);
        forge.route(
            "POST /api/v3/repos/OCA/repo/forks",
            202,
            &format!(r#"{{"html_url":"{}/me/repo"}}"#, forge.api),
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

    fn odx(&self, args: &[&str]) -> (bool, String, String) {
        binary(&self.work, &self.up, &self.api, args)
    }

    #[cfg(unix)]
    fn odx_with(&self, odl: &str, args: &[&str]) -> (bool, String, String) {
        binary_with(&self.work, &self.up, &self.api, odl, args)
    }

    /// Without push permission on the remote: through the user's fork.
    fn no_push(&self) {
        self.route(
            "GET /api/v3/repos/OCA/repo",
            200,
            r#"{"permissions":{"push":false}}"#,
        );
    }
}

#[test]
fn pull_requests_from_the_branch_or_a_fork() {
    let forge = Forge::new();
    let api = &forge.api;
    // Migrated and proposed in one go, to the repository itself.
    let (ok, out, err) = forge.odx(&["migrate", "addon", "my_addon", "--to", "19.0", "--open-pr"]);
    assert!(ok, "{err}");
    assert!(
        out.ends_with(&format!(
            "pushed to origin\nopened {api}/OCA/repo/pull/9 as a draft\n"
        )),
        "{out}"
    );
    git(
        &forge.up,
        &["rev-parse", "--verify", "--quiet", "19.0-mig-my_addon"],
    );

    // Later, ready for review, from a fork the user does not have yet.
    forge.no_push();
    let (ok, out, err) = forge.odx(&["--json", "migrate", "pr", "--ready"]);
    assert!(ok, "{err}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["pushed_to"], format!("{api}/me/repo.git"));
    assert_eq!(
        (value["number"].as_u64(), &value["draft"]),
        (Some(9), &false.into())
    );
    git(
        &forge.tmp.path().join("fork"),
        &["rev-parse", "--verify", "--quiet", "19.0-mig-my_addon"],
    );

    // An SSH remote: the fork is pushed to over SSH too.
    git(
        &forge.work,
        &["remote", "set-url", "origin", "git@127.0.0.1:OCA/repo.git"],
    );
    let (ok, out, err) = forge.odx(&["migrate", "pr"]);
    assert!(ok, "{err}");
    assert!(
        out.starts_with("pushed to git@127.0.0.1:me/repo.git\n"),
        "{out}"
    );

    // A remote of the user's choice.
    git(
        &forge.work,
        &["remote", "add", "mine", &format!("{api}/me/repo.git")],
    );
    let (ok, out, err) = forge.odx(&["migrate", "pr", "--push-remote", "mine", "--ready"]);
    assert!(ok, "{err}");
    assert_eq!(
        out,
        format!("pushed to mine\nopened {api}/OCA/repo/pull/9\n")
    );
}

#[test]
fn pull_request_failures() {
    let forge = Forge::new();
    let (ok, _, err) = forge.odx(&["migrate", "addon", "my_addon", "--to", "19.0"]);
    assert!(ok, "{err}");
    let failure = |expected: &str, args: &[&str]| {
        let (ok, _, err) = forge.odx(args);
        assert!(!ok);
        assert!(err.contains(expected), "{err}");
    };
    // A remote off the forges.
    git(&forge.work, &["remote", "add", "local", "/nowhere"]);
    failure(
        "the remote local is not on a forge",
        &["migrate", "pr", "--push-remote", "local"],
    );
    // A refused pull request.
    forge.route("POST /api/v3/repos/OCA/repo/pulls", 422, "{}");
    failure("answered HTTP 422", &["migrate", "pr"]);
    // A fork that cannot be pushed to, or made.
    forge.no_push();
    forge.route(
        "POST /api/v3/repos/OCA/repo/forks",
        202,
        &format!(r#"{{"html_url":"{}/me/missing"}}"#, forge.api),
    );
    failure("git push failed", &["migrate", "pr"]);
    forge.route("GET /api/v3/user", 401, "");
    failure("needs a token", &["migrate", "pr"]);
    // A forge that does not answer, or is none.
    forge.route("GET /api/v3/repos/OCA/repo", 500, "");
    failure("answered HTTP 500", &["migrate", "pr"]);
    forge.route("/api/v3/meta", 404, "");
    failure("does not look like GitHub", &["migrate", "pr"]);
}

#[test]
fn pull_requests_need_a_migration_branch_on_a_forge() {
    let (_tmp, _up, work) = fixture();
    let env = env();
    let (exit, _, err) = odx(&work, &env, &["migrate", "pr"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("\"19.0\" is no migration branch"), "{err}");
    git(&work, &["checkout", "--quiet", "-b", "19.0-mig-x"]);
    let (exit, _, err) = odx(&work, &env, &["migrate", "pr"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("the remote origin is not on a forge"), "{err}");
    // With --open-pr, the migration is kept when the pull request fails.
    git(&work, &["checkout", "--quiet", "19.0"]);
    let (exit, out, err) = odx(
        &work,
        &env,
        &["migrate", "addon", "my_addon", "--to", "19.0", "--open-pr"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.starts_with("migrated on branch 19.0-mig-my_addon"),
        "{out}"
    );
    assert!(err.contains("the remote origin is not on a forge"), "{err}");
}

#[cfg(unix)]
#[test]
fn pull_requests_list_the_findings() {
    use std::os::unix::fs::PermissionsExt as _;
    let forge = Forge::new();
    let odl = forge.tmp.path().join("odl");
    std::fs::write(
        &odl,
        "#!/bin/sh\necho '{\"modules\":[{\"violations\":[{\"file_path\":\"my_addon/data.xml\",\"line\":3,\"code\":\"U1908\",\"message\":\"by hand\"}]}]}'\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let odl = odl.display().to_string();
    let (ok, out, err) = forge.odx_with(
        &odl,
        &["migrate", "addon", "my_addon", "--to", "19.0", "--open-pr"],
    );
    assert!(ok, "{err}");
    assert!(out.contains("1 finding(s) left to fix by hand"), "{out}");
}
