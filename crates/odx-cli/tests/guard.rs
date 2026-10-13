//! The force-push guard rail of `odx lock --update`, against a local forge
//! API. Runs the `odx` binary so git can be pointed at the upstream on disk
//! (`url.<dir>.insteadOf`) while the forge API answers on localhost.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A forge API on localhost answering from `routes` (exact paths, query
/// strings ignored), 404 otherwise.
fn forge(routes: &Routes) -> String {
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
            let target = text.split_whitespace().nth(1).unwrap_or("/");
            let path = target.split('?').next().unwrap_or(target).to_owned();
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

struct Setup {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    up: PathBuf,
    project: PathBuf,
    api: String,
    routes: Routes,
}

impl Setup {
    /// An upstream with addons `pos_a` and `pos_b` on 17.0 and pull
    /// request 1 changing `pos_a`; the project aggregates it through the
    /// forge URL.
    fn new(policy: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        for addon in ["pos_a", "pos_b"] {
            std::fs::create_dir_all(up.join(addon)).unwrap();
            std::fs::write(up.join(addon).join("x.py"), addon).unwrap();
        }
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        let routes: Routes = Arc::default();
        let api = forge(&routes);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            format!(
                "[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"never\"\n{policy}\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{api}/OCA/pos.git\"\nmerges = [{{ pr = 1 }}]\n"
            ),
        )
        .unwrap();
        let setup = Self {
            _tmp: tmp,
            root,
            up,
            project,
            api,
            routes,
        };
        setup.push_pr("v1");
        setup
    }

    /// Pull request 1 now changes `pos_a` to `content`; returns its head.
    fn push_pr(&self, content: &str) -> String {
        git(&self.up, &["checkout", "--quiet", "--detach", "17.0"]);
        std::fs::write(self.up.join("pos_a").join("x.py"), content).unwrap();
        git(&self.up, &["commit", "--quiet", "-am", content]);
        let head = git(&self.up, &["rev-parse", "HEAD"]);
        git(&self.up, &["update-ref", "refs/pull/1/head", &head]);
        git(&self.up, &["checkout", "--quiet", "17.0"]);
        head
    }

    fn base(&self) -> String {
        git(&self.up, &["rev-parse", "17.0"])
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    fn odx(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_odx"))
            .current_dir(&self.project)
            .args(args)
            .env("ODX_CACHE_DIR", self.root.join("cache"))
            .env("ODX_CONFIG_DIR", self.root.join("config"))
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.{}.insteadOf", file_url(&self.up)),
            )
            .env("GIT_CONFIG_VALUE_0", format!("{}/OCA/pos.git", self.api))
            .env_remove("CI")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn lock(&self) -> String {
        std::fs::read_to_string(self.project.join("odx.lock")).unwrap()
    }
}

/// GitHub Enterprise: check runs plus commit statuses per commit.
fn github(setup: &Setup) {
    setup.route("/api/v3/meta", 200, "{}");
}

fn checks(setup: &Setup, sha: &str, runs: &str) {
    setup.route(
        &format!("/api/v3/repos/OCA/pos/commits/{sha}/check-runs"),
        200,
        &format!("{{\"check_runs\":[{runs}]}}"),
    );
    setup.route(
        &format!("/api/v3/repos/OCA/pos/commits/{sha}/status"),
        200,
        "{\"statuses\":[]}",
    );
}

const FAILED: &str = r#"{"name":"test pos_b","status":"completed","conclusion":"failure"}"#;
const PASSED: &str = r#"{"name":"test pos_b","status":"completed","conclusion":"success"}"#;
const RUNNING: &str = r#"{"name":"test pos_b","status":"in_progress"}"#;

#[test]
fn moved_heads_follow_the_policy() {
    let setup = Setup::new("");
    github(&setup);
    let (code, out, err) = setup.odx(&["lock"]);
    assert_eq!(code, 0, "{out}{err}");
    let v1 = git(&setup.up, &["rev-parse", "refs/pull/1/head"]);
    assert!(setup.lock().contains(&v1));

    // A red head whose failure the base shares is kept, and why is told.
    let v2 = setup.push_pr("v2");
    checks(&setup, &v2, FAILED);
    checks(&setup, &setup.base(), FAILED);
    let (code, out, err) = setup.odx(&["lock", "--update"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("is red, likely unrelated (base also failing)"),
        "{out}"
    );
    let lock = setup.lock();
    assert!(lock.contains(&format!("sha = \"{v1}\"")), "{lock}");
    assert!(lock.contains("status = \"pinned\""), "{lock}");
    assert!(
        lock.contains(&format!("upstream_head = \"{v2}\"")),
        "{lock}"
    );
    assert!(lock.contains("upstream_head_ci = \"red\""), "{lock}");
    assert!(
        lock.contains("upstream_head_classification = \"base-also-failing\""),
        "{lock}"
    );
    let since = lock
        .lines()
        .find(|line| line.starts_with("pinned_since"))
        .unwrap()
        .to_owned();

    // Failing checks that name only another addon.
    checks(&setup, &setup.base(), PASSED);
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(
        out.contains("is red, likely unrelated (other addon)"),
        "{out}"
    );
    let (_, out, _) = setup.odx(&["--json", "lock", "--update"]);
    assert!(out.contains("\"classification\":\"other-addon\""), "{out}");
    assert!(setup.lock().contains(&since), "the pin keeps its date");

    // Still running: kept, checked again next time.
    let v3 = setup.push_pr("v3");
    checks(&setup, &v3, RUNNING);
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(out.contains("is pending"), "{out}");

    // Adopted deliberately, and on green.
    let (code, out, _) = setup.odx(&["lock", "--update", "--follow", "pos#1"]);
    assert_eq!(code, 0);
    assert!(out.contains("updated pos #1"), "{out}");
    assert!(!setup.lock().contains("pinned"), "{}", setup.lock());
    let v4 = setup.push_pr("v4");
    checks(&setup, &v4, PASSED);
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(out.contains("updated pos #1"), "{out}");
    assert!(setup.lock().contains(&v4));
    // Nothing moved: nothing to decide.
    let lock = setup.lock();
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(!out.contains("updated") && !out.contains("kept"), "{out}");
    assert_eq!(setup.lock(), lock);

    // An API that cannot be read keeps the locked commit.
    let v5 = setup.push_pr("v5");
    setup.route(
        &format!("/api/v3/repos/OCA/pos/commits/{v5}/check-runs"),
        500,
        "",
    );
    let (code, out, err) = setup.odx(&["lock", "--update"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("cannot read the CI state"), "{err}");
    assert!(out.contains("is pending"), "{out}");
    assert!(setup.lock().contains(&format!("sha = \"{v4}\"")));

    // No checks at all: nothing to wait for.
    checks(&setup, &v5, "");
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(out.contains("updated pos #1"), "{out}");
}

#[test]
fn unknown_forges_and_unreachable_repositories() {
    // No forge API answers: the CI state cannot be read.
    let setup = Setup::new("");
    assert_eq!(setup.odx(&["lock"]).0, 0);
    setup.push_pr("v2");
    let (code, out, err) = setup.odx(&["lock", "--update"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("does not look like GitHub"), "{err}");
    assert!(out.contains("is pending"), "{out}");

    // A repository that cannot be fetched is reported, the others locked.
    let text = std::fs::read_to_string(setup.project.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.project.join("pyproject.toml"),
        format!("{text}[[tool.odx.repositories]]\nname = \"gone\"\nurl = \"file:///nonexistent/odx/gone\"\n"),
    )
    .unwrap();
    let (code, _, err) = setup.odx(&["lock", "--update"]);
    assert_eq!(code, 1);
    assert!(err.contains("gone: git fetch failed"), "{err}");
}

#[test]
fn pin_policy_and_status_only_forges() {
    let setup = Setup::new("on_head_change = \"pin\"");
    assert_eq!(setup.odx(&["lock"]).0, 0);
    setup.push_pr("v2");
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(out.contains("is newer (on_head_change = \"pin\")"), "{out}");

    // Gitea and Forgejo only have commit statuses: plain red.
    let setup = Setup::new("");
    setup.route("/api/v1/version", 200, "{\"version\":\"1\"}");
    assert_eq!(setup.odx(&["lock"]).0, 0);
    let v2 = setup.push_pr("v2");
    setup.route(
        &format!("/api/v1/repos/OCA/pos/commits/{v2}/status"),
        200,
        r#"{"statuses":[{"context":"test pos_b","status":"failure"}]}"#,
    );
    let (_, out, _) = setup.odx(&["lock", "--update"]);
    assert!(
        out.contains(": upstream head ") && out.contains(" is red\n"),
        "{out}"
    );
    assert!(
        setup
            .lock()
            .contains("upstream_head_classification = \"red\"")
    );
}
