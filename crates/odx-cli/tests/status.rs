//! `odx status` against a local forge API. Runs the `odx` binary so git
//! can be pointed at the upstream on disk (`url.<dir>.insteadOf`) while the
//! forge API answers on localhost.

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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    /// An upstream with addon `pos_a` on 17.0 and pull request 1 changing
    /// it; the project aggregates it through the forge URL, together with
    /// pull request 2 (not locked) and 3 (not tracked).
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let up = root.join("up");
        std::fs::create_dir_all(up.join("pos_a")).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        std::fs::write(up.join("pos_a").join("x.py"), "base\n").unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        let routes: Routes = Arc::default();
        let api = forge(&routes);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let setup = Self {
            _tmp: tmp,
            root,
            up,
            project,
            api,
            routes,
        };
        setup.config("{ pr = 1 }");
        setup.route("/api/v3/meta", 200, "{}");
        let head = setup.commit_on("17.0", "x.py", "v1\n");
        git(&setup.up, &["update-ref", "refs/pull/1/head", &head]);
        setup
    }

    fn config(&self, merges: &str) {
        std::fs::write(
            self.project.join("pyproject.toml"),
            format!(
                "[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"never\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}/OCA/pos.git\"\nmerges = [{merges}]\n",
                self.api
            ),
        )
        .unwrap();
    }

    /// A commit on top of `parent` writing `content` to `pos_a/<file>`.
    fn commit_on(&self, parent: &str, file: &str, content: &str) -> String {
        git(&self.up, &["checkout", "--quiet", "--detach", parent]);
        std::fs::write(self.up.join("pos_a").join(file), content).unwrap();
        git(&self.up, &["add", "-A"]);
        git(&self.up, &["commit", "--quiet", "-m", content.trim()]);
        let head = git(&self.up, &["rev-parse", "HEAD"]);
        git(&self.up, &["checkout", "--quiet", "17.0"]);
        head
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    /// Pull request `n` as the forge reports it.
    fn pull(&self, n: u32, state: &str, merged_at: &str, head: &str) {
        self.route(
            &format!("/api/v3/repos/OCA/pos/pulls/{n}"),
            200,
            &format!(
                r#"{{"title":"[17.0][FIX] pos_a #{n}","state":"{state}","merged_at":{merged_at},"head":{{"sha":"{head}"}},"base":{{"ref":"17.0"}},"labels":[{{"name":"approved"}}]}}"#
            ),
        );
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

    fn pr_head(&self) -> String {
        git(&self.up, &["rev-parse", "refs/pull/1/head"])
    }
}

#[test]
fn heads_and_states_of_tracked_pull_requests() {
    let setup = Setup::new();
    let (code, out, err) = setup.odx(&["aggregate"]);
    assert_eq!(code, 0, "{out}{err}");
    let v1 = setup.pr_head();
    setup.pull(1, "open", "null", &v1);
    let (code, out, err) = setup.odx(&["status"]);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    assert_eq!(
        out,
        "pos #1: open, head same, labels: approved - [17.0][FIX] pos_a #1\n"
    );

    // New commits on top: first only the forge knows, then the cache too.
    let v2 = setup.commit_on(&v1, "y.py", "more\n");
    git(&setup.up, &["update-ref", "refs/pull/1/head", &v2]);
    setup.pull(1, "open", "null", &v2);
    let (_, out, _) = setup.odx(&["status"]);
    assert!(
        out.starts_with("pos #1: open, head changed, labels"),
        "{out}"
    );
    assert!(out.contains("\n  new commits upstream: review"), "{out}");
    setup.odx(&["sync"]);
    let (_, out, _) = setup.odx(&["status"]);
    assert!(
        out.starts_with("pos #1: open, head changed (+1 commits)"),
        "{out}"
    );
    // Kept by the guard rail.
    let lock = std::fs::read_to_string(setup.project.join("odx.lock")).unwrap();
    let pinned = lock.replace("status = \"merged\"", "status = \"pinned\"");
    std::fs::write(setup.project.join("odx.lock"), &pinned).unwrap();
    let (_, out, _) = setup.odx(&["status"]);
    assert!(out.contains("\n  pin kept by the guard rail"), "{out}");
    std::fs::write(setup.project.join("odx.lock"), &lock).unwrap();

    // The same change on a newer base.
    let base = setup.commit_on("17.0", "z.py", "base moved\n");
    git(&setup.up, &["update-ref", "refs/heads/17.0", &base]);
    git(&setup.up, &["checkout", "--quiet", "--force", "17.0"]);
    let rebased = setup.commit_on("17.0", "x.py", "v1\n");
    git(&setup.up, &["update-ref", "refs/pull/1/head", &rebased]);
    setup.pull(1, "open", "null", &rebased);
    setup.odx(&["sync"]);
    let (_, out, _) = setup.odx(&["status"]);
    assert!(
        out.starts_with("pos #1: open, head rebased-same-patch"),
        "{out}"
    );

    // Force-pushed to something else.
    let other = setup.commit_on("17.0", "x.py", "rewritten\n");
    git(&setup.up, &["update-ref", "refs/pull/1/head", &other]);
    setup.pull(1, "open", "null", &other);
    setup.odx(&["sync"]);
    let (_, out, _) = setup.odx(&["status"]);
    assert!(out.starts_with("pos #1: open, head orphaned"), "{out}");
    assert!(out.contains("force-pushed upstream"), "{out}");
    let lock = std::fs::read_to_string(setup.project.join("odx.lock")).unwrap();
    let pinned = lock.replace("status = \"merged\"", "status = \"pinned\"");
    std::fs::write(setup.project.join("odx.lock"), &pinned).unwrap();
    let (_, out, _) = setup.odx(&["status"]);
    assert!(out.contains("\n  pin kept by the guard rail"), "{out}");
    std::fs::write(setup.project.join("odx.lock"), &lock).unwrap();
    // Offline, the cache alone tells the same.
    let (_, out, err) = setup.odx(&["--offline", "status"]);
    assert_eq!(
        (out.as_str(), err.as_str()),
        (
            "pos #1: unknown, head orphaned\n  force-pushed upstream: review the new head\n",
            ""
        )
    );

    // Merged upstream.
    setup.pull(1, "closed", "\"2026-03-04T05:06:07Z\"", &other);
    let (code, out, _) = setup.odx(&["status"]);
    assert_eq!(code, 0);
    assert!(out.contains(", merged 2026-03-04, labels"), "{out}");
    assert!(out.contains("drop the override"), "{out}");
    setup.config("{ pr = 2 }, { pr = 1 }");
    setup.pull(2, "open", "null", &other);
    let (_, out, _) = setup.odx(&["status", "--sort-by", "status"]);
    assert!(out.starts_with("pos #1: merged"), "{out}");
    setup.config("{ pr = 1 }");
    let (code, _, err) = setup.odx(&["status", "--fail-on-merged"]);
    assert_eq!(code, 1);
    assert!(err.contains("1 pull requests are merged upstream"), "{err}");
    let (_, out, _) = setup.odx(&["--json", "status"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["pull_requests"][0]["state"], "merged");
    assert_eq!(value["pull_requests"][0]["head"], "orphaned");
}

#[test]
fn untracked_unlocked_closed_and_unreadable_pull_requests() {
    let setup = Setup::new();
    setup.odx(&["aggregate"]);
    let v1 = setup.pr_head();
    setup.config("{ pr = 1 }, { pr = 2 }, { pr = 3, track_upstream = false }");
    setup.pull(1, "closed", "null", &v1);
    setup.pull(2, "open", "null", &v1);
    // A package from a pull request, locked from a repository on no forge.
    let lock = std::fs::read_to_string(setup.project.join("odx.lock")).unwrap();
    std::fs::write(
        setup.project.join("odx.lock"),
        format!(
            "{lock}\n[packages.odoo-addon-x]\nurl = \"{}\"\nsource = {{ pr = 4 }}\nsha = \"{v1}\"\n\n[packages.odoo-addon-y]\nurl = \"{0}\"\nsource = {{ branch = \"17.0\" }}\nsha = \"{v1}\"\n",
            file_url(&setup.up)
        ),
    )
    .unwrap();
    // A local change in the aggregated working tree.
    std::fs::write(setup.project.join("vendor/pos/pos_a/x.py"), "mine\n").unwrap();
    let (code, out, err) = setup.odx(&["status", "--sort-by", "status"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "pos #1: closed, head same, labels: approved - [17.0][FIX] pos_a #1\n  closed without merge: decide whether to keep it\n\
         pos #2: open, head not locked, labels: approved - [17.0][FIX] pos_a #2\n  not locked yet: run odx lock\n\
         odoo-addon-x #4: unknown, head unknown\n\
         pos: 1 local changes in vendor/pos\n"
            .replace("vendor/pos", &Path::new("vendor").join("pos").display().to_string())
    );
    assert_eq!(
        err,
        "warning: odoo-addon-x #4: cannot read the pull request: not on a forge\n"
    );
    let (_, out, _) = setup.odx(&["status", "--sort-by", "date"]);
    assert!(out.starts_with("odoo-addon-x #4"), "{out}");
    let (_, out, _) = setup.odx(&["--json", "status"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["modified"][0]["changes"], 1);

    // One without labels.
    setup.route(
        "/api/v3/repos/OCA/pos/pulls/1",
        200,
        &format!(r#"{{"title":"t","state":"open","head":{{"sha":"{v1}"}}}}"#),
    );
    let (_, out, _) = setup.odx(&["status"]);
    assert!(out.contains("\npos #1: open, head same - t\n"), "{out}");

    // A pull request the forge does not show.
    setup.route("/api/v3/repos/OCA/pos/pulls/2", 404, "");
    let (_, out, err) = setup.odx(&["status"]);
    assert!(out.contains("pos #2: unknown, head not locked\n"), "{out}");
    assert!(
        err.contains("pos #2: cannot read the pull request: cannot see"),
        "{err}"
    );

    // Nothing to track, and an invalid configuration.
    std::fs::write(setup.project.join("odx.lock"), &lock).unwrap();
    setup.config("{ pr = 1, track_upstream = false }");
    let (_, out, _) = setup.odx(&["status"]);
    assert!(out.starts_with("no pull requests to track\n"), "{out}");
    setup.config("{ pr = 1, sha = \"abc\" }");
    let (code, _, _) = setup.odx(&["status"]);
    assert_eq!(code, 2);
}
