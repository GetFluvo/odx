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
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    assert!(out.contains("\ntook "), "{out}");
    let untimed: String = out
        .split_inclusive('\n')
        .filter(|line| !line.starts_with("took "))
        .collect();
    assert_eq!(
        untimed,
        "migrated on branch 19.0-mig-my_addon from origin/18.0: 2 commit(s) of history\n  \
         [MIG] my_addon: Migration to 19.0\n\
         odoo-lint finds nothing left to do by hand\n\
         next: fix what is left and run the tests; then `odx migrate pr` pushes the branch and opens a draft pull request\n"
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
        out.starts_with("migrated on branch 19.0-mig-my_addon from origin/18.0: 1 commit(s)"),
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
    binary_with(work, up, api, &[], args)
}

fn binary_with(
    work: &Path,
    up: &Path,
    api: &str,
    env: &[(&str, &str)],
    args: &[&str],
) -> (bool, String, String) {
    let fork = up.with_file_name("fork");
    let out = Command::new(env!("CARGO_BIN_EXE_odx"))
        .current_dir(work)
        .args(args)
        .env("ODX_ODL", "odx-test-no-such-program")
        .envs(env.iter().copied())
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
    // The plan names it.
    let (ok, out, err) = binary(&work, &up, &api, &["migrate", "plan", "--to", "19.0"]);
    assert!(ok, "{err}");
    assert!(
        out.contains(
            "open migration pull request: my_addon #7; review, test and contribute there\n"
        ),
        "{out}"
    );
    let (_, out, _) = binary(
        &work,
        &up,
        &api,
        &["--json", "migrate", "plan", "--to", "19.0"],
    );
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["pull_requests"][0]["number"], 7);
    let (ok, out, _) = binary(
        &work,
        &up,
        &api,
        &["--offline", "migrate", "plan", "--to", "19.0"],
    );
    assert!(ok);
    assert!(
        out.contains("to migrate, dependencies first (1):\n  my_addon\n"),
        "{out}"
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
    let (ok, _, err) = binary(&work, &up, &api, &["migrate", "plan", "--to", "19.0"]);
    assert!(ok, "{err}");
    assert!(
        err.contains("warning: migrate: cannot look for open migration pull requests: "),
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
        binary_with(&self.work, &self.up, &self.api, &[("ODX_ODL", odl)], args)
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

#[test]
fn administrative_commits_are_squashed_unless_asked() {
    let (_tmp, up, work) = fixture();
    git(&up, &["checkout", "--quiet", "18.0"]);
    write(&up.join("my_addon/README.rst"), "readme\n");
    git(&up, &["add", "-A"]);
    git(
        &up,
        &[
            "commit",
            "--quiet",
            "--author",
            "OCA-git-bot <oca-git-bot@odoo-community.org>",
            "-m",
            "[BOT] post-merge updates",
        ],
    );
    git(&up, &["checkout", "--quiet", "--detach"]);
    let env = env();
    let start = [
        "migrate",
        "addon",
        "my_addon",
        "--to",
        "19.0",
        "--no-pre-commit",
    ];
    let again = |extra: &[&str]| {
        git(&work, &["checkout", "--quiet", "19.0"]);
        git(&work, &["branch", "-D", "19.0-mig-my_addon"]);
        let mut args = start.to_vec();
        args.extend_from_slice(extra);
        odx(&work, &env, &args)
    };
    let (exit, out, err) = odx(&work, &env, &start);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.starts_with(
            "migrated on branch 19.0-mig-my_addon from origin/18.0: 2 commit(s) of history (1 administrative commit(s) squashed into them)\n"
        ),
        "{out}"
    );
    let (_, out, _) = again(&["--no-squash"]);
    assert!(
        out.starts_with(
            "migrated on branch 19.0-mig-my_addon from origin/18.0: 3 commit(s) of history\n"
        ),
        "{out}"
    );
    // A project may name other bots, or none.
    write(
        &work.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"19.0\"\n\n[tool.odx.port]\nsquash_authors = []\nsquash_subjects = []\n",
    );
    let (_, out, _) = again(&[]);
    assert!(
        out.starts_with(
            "migrated on branch 19.0-mig-my_addon from origin/18.0: 3 commit(s) of history\n"
        ),
        "{out}"
    );
}

#[test]
fn plans_of_a_plain_addons_repository() {
    let (_tmp, up, work) = fixture();
    // On 18.0 another addon that depends on my_addon, and one 19.0 has.
    git(&up, &["checkout", "--quiet", "18.0"]);
    write(
        &up.join("a_dep/__manifest__.py"),
        "{'depends': ['base', 'my_addon']}\n",
    );
    write(&up.join("done/__manifest__.py"), "{}\n");
    write(&up.join("docs/index.md"), "not an addon\n");
    commit(&up, "[ADD] more");
    git(&up, &["checkout", "--quiet", "19.0"]);
    write(&up.join("done/__manifest__.py"), "{}\n");
    commit(&up, "[MIG] done");
    git(&up, &["checkout", "--quiet", "--detach"]);
    let env = env();
    let (exit, out, err) = odx(&work, &env, &["migrate", "plan", "--to", "19.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(
        out,
        "migration plan origin/18.0 -> origin/19.0\n\
         already on 19.0 (1): done\n\
         to migrate, dependencies first (2):\n  my_addon\n  a_dep\n\
         next: `odx migrate addon <addon> --to 19.0 --from 18.0 --remote origin`\n"
    );
    let (_, out, _) = odx(&work, &env, &["--json", "migrate", "plan", "--to", "19.0"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        value["to_migrate"],
        serde_json::json!(["my_addon", "a_dep"])
    );
    assert_eq!(value["source"], "origin/18.0");
    for (args, exit, expected) in [
        (
            &["migrate", "plan"][..],
            Exit::Usage,
            "name the target series with --to",
        ),
        (
            &["migrate", "plan", "--to", "x"],
            Exit::Usage,
            "cannot tell the series before x",
        ),
        (
            &["migrate", "plan", "--to", "19.0", "--remote", "nowhere"],
            Exit::Failure,
            "cannot fetch from nowhere",
        ),
    ] {
        let (code, _, err) = odx(&work, &env, args);
        assert_eq!(code, exit, "{err}");
        assert!(err.contains(expected), "{err}");
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario, step by step"
)]
fn dependencies_wait_and_get_pinned() {
    let forge = Forge::new();
    let api = forge.api.clone();
    // my_addon depends on addons with and without an open migration pull
    // request, one released for 19.0, and ones odx cannot follow.
    git(&forge.up, &["checkout", "--quiet", "18.0"]);
    write(
        &forge.up.join("my_addon/__manifest__.py"),
        "{'version': '18.0.1.0.0', 'depends': ['base', 'dep_x', 'dep_y', 'dep_released', 'dep_gone', 'dep_odd', 'dep_far']}\n",
    );
    commit(&forge.up, "[IMP] my_addon: depends");
    git(&forge.up, &["checkout", "--quiet", "--detach"]);
    let index = forge.tmp.path().join("index");
    let release = |dist: &str, version: &str, home: &str| {
        write(
            &index.join(dist).join("json"),
            &format!("{{\"releases\": {{\"{version}\": []}}}}"),
        );
        write(
            &index.join(dist).join(version).join("json"),
            &format!("{{\"info\": {{\"home_page\": \"{home}\"}}}}"),
        );
    };
    let deps = format!("{api}/OCA/deps");
    release("odoo-addon-dep-x", "18.0.1.0.0", &deps);
    release("odoo-addon-dep-y", "18.0.1.0.0", &deps);
    release("odoo-addon-dep-released", "19.0.1.0.0", &deps);
    release("odoo-addon-dep-odd", "18.0.1.0.0", "not a url");
    release(
        "odoo-addon-dep-far",
        "18.0.1.0.0",
        "http://127.0.0.1:1/OCA/far",
    );
    forge.route(
        "GET /api/v3/repos/OCA/deps/pulls",
        200,
        r#"[{"number":31,"title":"[19.0][MIG] dep_x: Migration to 19.0","state":"open","base":{"ref":"19.0"}}]"#,
    );
    let pypi = file_url(&index);
    let env = [("ODX_PYPI_URL", pypi.as_str())];
    let run = |args: &[&str]| binary_with(&forge.work, &forge.up, &api, &env, args);
    let waits = "  waits on dep_x: open migration pull request OCA/deps#31\n  \
                 waits on dep_y: not on 19.0, and no open migration pull request found\n";
    // The plan says it, offline it cannot tell.
    let (ok, out, err) = run(&["migrate", "plan", "--to", "19.0"]);
    assert!(ok, "{err}");
    let indented = waits.replace("  waits", "    waits");
    assert!(out.contains(&format!("  my_addon\n{indented}")), "{out}");
    let (_, out, _) = run(&["--offline", "migrate", "plan", "--to", "19.0"]);
    assert!(!out.contains("waits on"), "{out}");
    let (ok, out, err) = run(&[
        "migrate",
        "addon",
        "my_addon",
        "--to",
        "19.0",
        "--no-pre-commit",
    ]);
    assert!(ok, "{err}");
    assert!(
        out.contains(&format!("5 dependencies are not on 19.0 yet:\n{waits}")),
        "{out}"
    );
    assert!(out.contains("`odx migrate pin` pins them"), "{out}");
    // Pinned next to what test-requirements.txt has.
    write(&forge.work.join("test-requirements.txt"), "requests\n");
    git(&forge.work, &["add", "-A"]);
    git(&forge.work, &["commit", "--quiet", "-m", "requests"]);
    let pin =
        format!("odoo-addon-dep_x @ git+{api}/OCA/deps.git@refs/pull/31/head#subdirectory=dep_x");
    let (ok, out, err) = run(&["migrate", "pin"]);
    assert!(ok, "{err}");
    assert!(out.contains(&format!("  {pin}\n")), "{out}");
    assert!(out.contains("stays red"), "{out}");
    assert_eq!(
        git(&forge.work, &["log", "-1", "--format=%s"]),
        "[DON'T MERGE] test-requirements.txt"
    );
    let file = std::fs::read_to_string(forge.work.join("test-requirements.txt")).unwrap();
    assert_eq!(file, format!("requests\n{pin}\n"));
    // Again: nothing changes, no commit.
    let (ok, out, _) = run(&["--json", "migrate", "pin"]);
    assert!(ok);
    assert!(out.contains("\"pinned\":["), "{out}");
    assert_eq!(
        git(
            &forge.work,
            &["rev-list", "--count", "--grep", "DON'T MERGE", "HEAD"]
        ),
        "1"
    );
    // The pull request depends on it.
    let (ok, _, err) = run(&["migrate", "pr"]);
    assert!(ok, "{err}");
    // A pin git cannot commit, and one it cannot write.
    let lock = forge.work.join(".git/index.lock");
    git(&forge.work, &["reset", "--quiet", "--hard", "HEAD~1"]);
    std::fs::write(&lock, "").unwrap();
    let (ok, _, err) = run(&["migrate", "pin"]);
    assert!(!ok);
    assert!(err.contains("git add failed"), "{err}");
    std::fs::remove_file(&lock).unwrap();
    // Pinned right away, from a fresh migration.
    let fresh = |extra: &[&str]| {
        git(&forge.work, &["checkout", "--quiet", "--force", "19.0"]);
        git(&forge.work, &["branch", "-D", "19.0-mig-my_addon"]);
        let _ = std::fs::remove_dir_all(forge.work.join("test-requirements.txt"));
        let mut args = vec![
            "migrate",
            "addon",
            "my_addon",
            "--to",
            "19.0",
            "--no-pre-commit",
            "--force",
        ];
        args.extend_from_slice(extra);
        run(&args)
    };
    let (ok, out, err) = fresh(&["--pin-unreleased"]);
    assert!(ok, "{err}");
    assert!(
        out.contains(&format!(
            "pinned in a commit `[DON'T MERGE] test-requirements.txt`:\n  {pin}\n"
        )),
        "{out}"
    );
    let (ok, out, _) = fresh(&["--json", "--pin-unreleased"]);
    assert!(ok);
    let value: serde_json::Value =
        serde_json::from_str(out.lines().last().unwrap_or_default()).unwrap_or_default();
    assert_eq!(value["waits_on"][0]["pull_request"]["number"], 31);
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    git(&forge.work, &["branch", "-D", "19.0-mig-my_addon"]);
    std::fs::create_dir_all(forge.work.join("test-requirements.txt")).unwrap();
    let (ok, _, err) = run(&[
        "migrate",
        "addon",
        "my_addon",
        "--to",
        "19.0",
        "--no-pre-commit",
        "--force",
        "--pin-unreleased",
    ]);
    assert!(!ok);
    assert!(err.contains("test-requirements.txt"), "{err}");
    let (ok, _, err) = run(&["migrate", "pin"]);
    assert!(!ok);
    assert!(err.contains("test-requirements.txt"), "{err}");
    // Without an open pull request there is nothing to pin, nor to propose.
    forge.route("GET /api/v3/repos/OCA/deps/pulls", 200, "[]");
    let (ok, out, err) = fresh(&[]);
    assert!(ok, "{err}");
    assert!(!out.contains("`odx migrate pin`"), "{out}");
    let (ok, out, _) = run(&["migrate", "pin"]);
    assert!(ok);
    assert!(out.starts_with("nothing to pin"), "{out}");
    // Only on a migration branch.
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    let (ok, _, err) = run(&["migrate", "pin"]);
    assert!(!ok);
    assert!(err.contains("is no migration branch"), "{err}");
}

/// A commit by Jane on 18.0 of `up`, touching `my_addon`.
fn upstream_fix(up: &Path, file: &str, text: &str, subject: &str) {
    git(up, &["checkout", "--quiet", "18.0"]);
    write(&up.join("my_addon").join(file), text);
    git(up, &["add", "-A"]);
    git(
        up,
        &[
            "commit",
            "--quiet",
            "--author",
            "Jane <jane@example.com>",
            "-m",
            subject,
        ],
    );
    git(up, &["checkout", "--quiet", "--detach"]);
}

#[test]
fn migrations_are_brought_up_to_date() {
    let (_tmp, up, work) = fixture();
    let env = env();
    let (exit, _, err) = odx(
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
    upstream_fix(&up, "b.py", "b\n", "[FIX] my_addon: b");
    let (exit, out, err) = odx(
        &work,
        &env,
        &["migrate", "update", "--no-pre-commit", "--no-squash"],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.starts_with("updated 19.0-mig-my_addon from origin/18.0: 1 new commit(s)\n"),
        "{out}"
    );
    assert!(out.contains("--push"), "{out}");
    assert_eq!(
        git(&work, &["log", "-1", "--format=%an|%s"]),
        "Jane|[FIX] my_addon: b"
    );
    let (_, out, _) = odx(&work, &env, &["migrate", "update"]);
    assert!(
        out.starts_with("19.0-mig-my_addon is up to date with origin/18.0\n"),
        "{out}"
    );
    let (_, out, _) = odx(
        &work,
        &env,
        &[
            "--json", "migrate", "update", "--from", "18.0", "--remote", "origin",
        ],
    );
    assert!(out.contains("\"status\":\"updated\""), "{out}");
    // Pushing needs to know where.
    let (exit, _, err) = odx(&work, &env, &["migrate", "update", "--push"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("does not know where"), "{err}");
    // A push that fails says what to do instead.
    for (key, value) in [("PushTo", "/nowhere"), ("PushBranch", "theirs")] {
        let key = format!("branch.19.0-mig-my_addon.odxMigrate{key}");
        git(&work, &["config", &key, value]);
    }
    let (exit, _, err) = odx(&work, &env, &["migrate", "update", "--push"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("open a pull request against their branch"),
        "{err}"
    );
    // A conflict stops; continuing reports the update.
    std::fs::write(work.join("my_addon/b.py"), "b, 19.0\n").unwrap();
    git(&work, &["commit", "--quiet", "-am", "[IMP] my_addon: 19.0"]);
    upstream_fix(&up, "b.py", "b, fixed\n", "[FIX] my_addon: b again");
    let (exit, _, err) = odx(&work, &env, &["migrate", "update"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx migrate --continue"), "{err}");
    std::fs::write(work.join("my_addon/b.py"), "b, 19.0, fixed\n").unwrap();
    git(&work, &["add", "-A"]);
    let (exit, out, err) = odx(&work, &env, &["migrate", "--continue"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.starts_with("updated 19.0-mig-my_addon from origin/18.0: 1 new commit(s)"),
        "{out}"
    );
    // Elsewhere: not a migration branch, or one odx cannot place.
    git(
        &work,
        &["checkout", "--quiet", "-b", "19.0-mig-other", "origin/19.0"],
    );
    let (exit, _, err) = odx(&work, &env, &["migrate", "update"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("--since"), "{err}");
    git(&work, &["checkout", "--quiet", "19.0"]);
    let (exit, _, err) = odx(&work, &env, &["migrate", "update"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("is no migration branch"), "{err}");
}

#[test]
fn someone_elses_migration_pull_request() {
    let forge = Forge::new();
    let api = forge.api.clone();
    let run = |args: &[&str]| forge.odx(args);
    // Someone's migration, as pull request 7 from their fork.
    let (ok, _, err) = run(&[
        "migrate",
        "addon",
        "my_addon",
        "--to",
        "19.0",
        "--no-pre-commit",
        "--force",
    ]);
    assert!(ok, "{err}");
    git(
        &forge.work,
        &[
            "push",
            "--quiet",
            &file_url(&forge.up),
            "19.0-mig-my_addon:refs/pull/7/head",
        ],
    );
    git(
        &forge.work,
        &[
            "push",
            "--quiet",
            &file_url(&forge.tmp.path().join("fork")),
            "19.0-mig-my_addon:theirs",
        ],
    );
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    git(&forge.work, &["branch", "-D", "19.0-mig-my_addon"]);
    forge.route(
        "GET /api/v3/repos/OCA/repo/pulls/7",
        200,
        &format!(
            r#"{{"number":7,"title":"[19.0][MIG] my_addon: Migration to 19.0","state":"open","base":{{"ref":"19.0"}},"head":{{"sha":"x","ref":"theirs","repo":{{"html_url":"{api}/me/repo"}}}},"maintainer_can_modify":true}}"#
        ),
    );
    upstream_fix(&forge.up, "b.py", "b\n", "[FIX] my_addon: b");
    let (ok, out, err) = run(&[
        "migrate",
        "update",
        "OCA/repo#7",
        "--push",
        "--no-pre-commit",
    ]);
    assert!(ok, "{err}");
    assert!(
        out.starts_with("updated 19.0-mig-my_addon from origin/18.0: 1 new commit(s)\n"),
        "{out}"
    );
    assert!(
        out.ends_with(&format!("pushed to {api}/me/repo.git theirs\n")),
        "{out}"
    );
    assert_eq!(
        git(
            &forge.tmp.path().join("fork"),
            &["log", "-1", "--format=%s", "theirs"]
        ),
        "[FIX] my_addon: b"
    );
    // The branch is here now; other refusals.
    let (ok, _, err) = run(&["migrate", "update", "OCA/repo#7"]);
    assert!(!ok);
    assert!(err.contains("exists here"), "{err}");
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    for (args, expected) in [
        (
            &["migrate", "update", "repo#7"][..],
            "<owner>/<repo>#<number>",
        ),
        (
            &["migrate", "update", "OCA/elsewhere#7"],
            "no remote of this clone points at OCA/elsewhere",
        ),
        (&["migrate", "update", "OCA/repo#8"], "cannot see"),
    ] {
        let (ok, _, err) = run(args);
        assert!(!ok);
        assert!(err.contains(expected), "{err}");
    }
    forge.route(
        "GET /api/v3/repos/OCA/repo/pulls/8",
        200,
        r#"{"number":8,"title":"[19.0][IMP] my_addon","state":"open","base":{"ref":"19.0"}}"#,
    );
    let (ok, _, err) = run(&["migrate", "update", "OCA/repo#8"]);
    assert!(!ok);
    assert!(err.contains("names no `[MIG] <addon>`"), "{err}");
}

#[test]
fn migration_pull_requests_odx_cannot_update() {
    let forge = Forge::new();
    let run = |args: &[&str]| forge.odx(args);
    // A head that cannot be fetched; one from a repository the forge no
    // longer shows, checked out but not pushed anywhere.
    git(
        &forge.work,
        &[
            "push",
            "--quiet",
            &file_url(&forge.up),
            "19.0:refs/pull/10/head",
        ],
    );
    for number in [9, 10] {
        forge.route(
            &format!("GET /api/v3/repos/OCA/repo/pulls/{number}"),
            200,
            &format!(
                r#"{{"number":{number},"title":"[19.0][MIG] addon_{number}","state":"open","base":{{"ref":"19.0"}}}}"#
            ),
        );
    }
    let (ok, _, err) = run(&["migrate", "update", "OCA/repo#9"]);
    assert!(!ok);
    assert!(err.contains("cannot fetch"), "{err}");
    let (ok, _, err) = run(&["migrate", "update", "OCA/repo#10"]);
    assert!(!ok);
    assert!(err.contains("--since"), "{err}");
    assert_eq!(
        git(&forge.work, &["branch", "--show-current"]),
        "19.0-mig-addon_10"
    );
    git(&forge.work, &["checkout", "--quiet", "19.0"]);
    git(&forge.work, &["remote", "add", "local", "/nowhere"]);
    let (ok, _, err) = run(&["migrate", "update", "OCA/repo#7", "--remote", "local"]);
    assert!(!ok);
    assert!(err.contains("not on a forge"), "{err}");
}

#[test]
fn updates_leave_bots_out_and_bring_target_fixes() {
    use std::os::unix::fs::PermissionsExt as _;
    let (tmp, up, work) = fixture();
    let odl = tmp.path().join("odl");
    std::fs::write(
        &odl,
        r#"#!/bin/sh
case "$*" in
  *--bump-version*) exit 0;;
  *--fix*) echo fixed >> my_addon/fixed.py; exit 0;;
  *) echo '{"modules":[{"name":"my_addon","violations":[{"file_path":"my_addon/data.xml","line":3,"column":1,"code":"U1908","name":"x","message":"use odoo.fields.Domain"}]}]}'; exit 1;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&odl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = env().with("ODX_ODL", odl.display().to_string());
    let (exit, _, err) = odx(
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
    upstream_fix(&up, "b.py", "b\n", "[FIX] my_addon: b");
    git(&up, &["checkout", "--quiet", "18.0"]);
    for (email, text, subject) in [
        ("nl@example.com", "a\n", "Translated using Weblate (Dutch)"),
        ("oca-ci@odoo-community.org", "", "[UPD] Update my_addon.pot"),
        (
            "nl@example.com",
            "a, b\n",
            "Translated using Weblate (Dutch)",
        ),
    ] {
        let file = if text.is_empty() {
            "i18n/my_addon.pot"
        } else {
            "i18n/nl.po"
        };
        write(
            &up.join("my_addon").join(file),
            &format!("{text}{subject}\n"),
        );
        git(&up, &["add", "-A"]);
        let author = format!("Someone <{email}>");
        git(
            &up,
            &["commit", "--quiet", "--author", &author, "-m", subject],
        );
    }
    git(&up, &["checkout", "--quiet", "--detach"]);
    let (exit, out, err) = odx(&work, &env, &["migrate", "update", "--no-pre-commit"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.starts_with(
            "updated 19.0-mig-my_addon from origin/18.0: 2 new commit(s) (1 administrative commit(s) squashed into them)\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("  [FW] my_addon: 19.0 fixes for the changes from 18.0"),
        "{out}"
    );
    assert!(
        out.contains("  left by hand: my_addon/data.xml:3: U1908"),
        "{out}"
    );
    let subjects = git(&work, &["log", "-4", "--format=%s"]);
    assert!(!subjects.contains("[UPD]"), "{subjects}");
}
