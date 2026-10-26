//! `odx dashboard` and `odx dashboard act` on a project aggregating a
//! repository behind a local forge API, with an index update. Runs the
//! `odx` binary so git can be pointed at the upstream on disk.

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
        .env("GIT_AUTHOR_NAME", "Jane")
        .env("GIT_AUTHOR_EMAIL", "jane@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn change(dir: &Path, files: &[(&str, &str)], message: &str) {
    for (file, content) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
}

fn merged(dir: &Path, series: &str, n: u32, files: &[(&str, &str)], message: &str) {
    git(dir, &["checkout", "--quiet", "-b", &format!("pr{n}")]);
    change(dir, files, message);
    git(dir, &["checkout", "--quiet", series]);
    git(
        dir,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            &format!("pr{n}"),
            "-m",
            &format!("Merge PR #{n} into {series}"),
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

/// A forge API on localhost answering from `routes` (paths without the
/// query string), 404 otherwise.
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
    /// `OCA/pos` with pull request 2 merged on 16.0 (missing on 17.0),
    /// pull request 3 there too but blacklisted on 17.0, pull request 9 on
    /// 18.0, and pull request 1 open on 17.0, which the project merges.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let up = root.join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=16.0"]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '16'}")],
            "[MIG] pos_x: 16.0",
        );
        merged(&up, "16.0", 2, &[("pos_x/b.py", "b\n")], "[FIX] pos_x: b");
        merged(&up, "16.0", 3, &[("pos_x/c.py", "c\n")], "[IMP] pos_x: c");
        git(&up, &["checkout", "--quiet", "--orphan", "18.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '18'}")],
            "[MIG] pos_x: 18.0",
        );
        merged(&up, "18.0", 9, &[("pos_x/d.py", "d\n")], "[IMP] pos_x: d");
        git(&up, &["checkout", "--quiet", "--orphan", "17.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '17'}")],
            "[MIG] pos_x: 17.0",
        );
        change(
            &up,
            &[(
                ".oca/oca-port/blacklist/pos_x.json",
                "{\"pull_requests\": {\"OCA/pos#3\": \"not on 17.0\"}}",
            )],
            "[BL] pos_x",
        );
        git(&up, &["checkout", "--quiet", "-b", "pr1"]);
        change(&up, &[("pos_x/e.py", "e\n")], "[FIX] pos_x: e");
        let head = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/1/head", &head]);
        git(&up, &["checkout", "--quiet", "17.0"]);
        // An index with a patch release of an addon package.
        let index = root.join("index");
        std::fs::create_dir_all(index.join("odoo-addon-web-q")).unwrap();
        std::fs::write(
            index.join("odoo-addon-web-q").join("json"),
            r#"{"releases": {"17.0.2.0.0": [{"filename": "odoo_addon_web_q-17.0.2.0.0-py3-none-any.whl", "url": "file:///nowhere/q.whl", "digests": {"sha256": "22"}}]}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(index.join("odoo-addon-web-z")).unwrap();
        std::fs::write(
            index.join("odoo-addon-web-z").join("json"),
            r#"{"releases": {
                "17.0.1.0.0": [{"filename": "odoo_addon_web_z-17.0.1.0.0-py3-none-any.whl", "url": "file:///nowhere/a.whl", "digests": {"sha256": "00"}}],
                "17.0.1.0.1": [{"filename": "odoo_addon_web_z-17.0.1.0.1-py3-none-any.whl", "url": "file:///nowhere/b.whl", "digests": {"sha256": "11"}}]
            }}"#,
        )
        .unwrap();
        let routes: Routes = Arc::default();
        let api = forge(&routes);
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("pyproject.toml"),
            format!(
                "[project]\nname = \"p\"\nversion = \"1\"\ndependencies = [\"odoo-addon-web-z\"]\n\n[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"never\"\n\n[tool.odx.port]\ndefault_from = [\"16.0\", \"18.0\"]\n\n[tool.odx.environments.env.prod]\n\n[tool.odx.environments.env.next]\nseries = \"18.0\"\n\n[tool.odx.lock.auto_update]\ncontent_check = false\n\n[tool.odx.cache]\nttl = {{ pr_state = \"30m\" }}\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{api}/OCA/pos.git\"\naddons = [\"pos_x\"]\nmerges = [{{ pr = 1 }}]\n"
            ),
        )
        .unwrap();
        std::fs::write(
            project.join("uv.lock"),
            "version = 1\n\n[[package]]\nname = \"odoo-addon-web-z\"\nversion = \"17.0.1.0.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n\n[[package]]\nname = \"odoo-addon-web-q\"\nversion = \"17.0.1.0.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n",
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
        setup.route("/api/v3/meta", 200, "{}");
        setup.pull("open");
        setup.route(
            "/api/v3/repos/OCA/pos/pulls",
            200,
            r#"[{"number": 5, "title": "[17.0][IMP] pos_x: new", "state": "open", "base": {"ref": "17.0"}},
               {"number": 1, "title": "[17.0][FIX] pos_x: e", "state": "open", "base": {"ref": "17.0"}},
               {"number": 6, "title": "[17.0][IMP] web: other", "state": "open", "base": {"ref": "17.0"}}]"#,
        );
        setup
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (status, body.to_owned()));
    }

    /// Pull request 1 in `state`.
    fn pull(&self, state: &str) {
        let head = git(&self.up, &["rev-parse", "refs/pull/1/head"]);
        let merged_at = if state == "merged" {
            "\"2026-10-01T00:00:00Z\""
        } else {
            "null"
        };
        let state = if state == "merged" { "closed" } else { state };
        self.route(
            "/api/v3/repos/OCA/pos/pulls/1",
            200,
            &format!(
                r#"{{"title": "[17.0][FIX] pos_x: e", "state": "{state}", "merged_at": {merged_at}, "head": {{"sha": "{head}"}}}}"#
            ),
        );
    }

    fn odx(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_odx"))
            .current_dir(&self.project)
            .args(args)
            .env("ODX_CACHE_DIR", self.root.join("cache"))
            .env("ODX_CONFIG_DIR", self.root.join("config"))
            .env("ODX_PYPI_URL", file_url(&self.root.join("index")))
            .env("ODX_UV", "odx-test-no-such-program")
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

    fn pyproject(&self) -> String {
        std::fs::read_to_string(self.project.join("pyproject.toml")).unwrap()
    }
}

#[test]
fn sections_and_renderers() {
    let setup = Setup::new();
    let (code, out, err) = setup.odx(&["aggregate"]);
    assert_eq!(code, 0, "{out}{err}");
    let (code, out, err) = setup.odx(&["dashboard"]);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    for line in [
        "Dashboard for 17.0\n",
        "Pinned pull requests (1)\n  pr:OCA/pos#1  open, head same  pos #1 - [17.0][FIX] pos_x: e  drop\n",
        "New upstream pull requests (1)\n  new:OCA/pos#5  open  [17.0][IMP] pos_x: new  include\n",
        "Forward-port suggestions (1)\n  fwd:OCA/pos#2->17.0  from 16.0  pos_x: [FIX] pos_x: b  port,mute\n",
        "Backport candidates (1)\n  bwd:OCA/pos#9->17.0  from 18.0  pos_x: [IMP] pos_x: d  port,mute\n",
        "Index updates (2)\n  pkg:odoo-addon-web-z@17.0.1.0.1  safe    odoo-addon-web-z 17.0.1.0.0 -> 17.0.1.0.1  follow\n",
        "Suppressed (1)\n  fwd:OCA/pos#3->17.0  from 16.0  pos_x: [IMP] pos_x: c\n    blacklisted upstream: not on 17.0\n",
    ] {
        assert!(out.contains(line), "{line}\n---\n{out}");
    }
    assert!(out.contains("Dashboard for 18.0\n"), "{out}");
    assert!(
        out.contains("  pkg:odoo-addon-web-q@17.0.2.0.0  review"),
        "{out}"
    );
    // The second time, the kept dashboard answers.
    let (_, out, _) = setup.odx(&["dashboard", "--section", "pinned"]);
    assert!(
        out.ends_with("data as of 0 min ago; --fresh rebuilds it\n"),
        "{out}"
    );
    let (_, out, _) = setup.odx(&["dashboard", "--format", "markdown", "--checkboxes"]);
    assert!(
        out.contains("  - [ ] mute <!-- odx:mute fwd:OCA/pos#2->17.0 -->\n"),
        "{out}"
    );
    let (_, out, _) = setup.odx(&["--json", "dashboard", "--section", "forward-port"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value[0]["sections"].as_array().unwrap().len(), 1);
    assert_eq!(
        value[0]["sections"][0]["entries"][0]["id"],
        "fwd:OCA/pos#2->17.0"
    );
    // `[tool.odx.dashboard] sections` when --section is not given.
    let config = std::fs::read_to_string(setup.project.join("pyproject.toml")).unwrap();
    std::fs::write(
        setup.project.join("pyproject.toml"),
        format!("{config}\n[tool.odx.dashboard]\nsections = [\"backport\"]\n"),
    )
    .unwrap();
    let (_, out, _) = setup.odx(&["--json", "dashboard"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value[0]["sections"].as_array().unwrap().len(), 1);
    assert_eq!(value[0]["sections"][0]["kind"], "backport");
    let (_, out, _) = setup.odx(&["--json", "dashboard", "--section", "pinned"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value[0]["sections"][0]["kind"], "pinned");
    std::fs::write(setup.project.join("pyproject.toml"), config).unwrap();
    let (code, _, err) = setup.odx(&["dashboard", "--section", "nope"]);
    assert_eq!(code, 2);
    assert!(err.contains("no section \"nope\""), "{err}");
    let (code, _, err) = setup.odx(&["dashboard", "--format", "tui"]);
    assert_eq!(code, 2);
    if cfg!(feature = "tui") {
        assert!(err.contains("needs a terminal"), "{err}");
    } else {
        assert!(err.contains("`tui` feature"), "{err}");
    }
    // A package locked from a pull request is pinned too.
    let lock = std::fs::read_to_string(setup.project.join("odx.lock")).unwrap();
    std::fs::write(
        setup.project.join("odx.lock"),
        format!(
            "{lock}\n[packages.odoo-addon-pos-y]\nurl = \"{}/OCA/pos.git\"\nsource = {{ pr = 7 }}\nsha = \"{}\"\n",
            setup.api,
            "0".repeat(40)
        ),
    )
    .unwrap();
    let (_, out, _) = setup.odx(&["--fresh", "dashboard", "--section", "pinned"]);
    assert!(out.contains("  pr:OCA/pos#7  unknown"), "{out}");
    // Merged upstream: the override can go.
    setup.pull("merged");
    let (_, out, _) = setup.odx(&["--fresh", "dashboard", "--section", "droppable"]);
    assert!(
        out.contains("Overrides that can be dropped (1)\n  pr:OCA/pos#1  merged"),
        "{out}"
    );
    // A host that answers like no forge has no new pull requests.
    setup.route("/api/v3/meta", 404, "");
    let (_, out, err) = setup.odx(&["--fresh", "dashboard", "--section", "new-upstream"]);
    assert!(!out.contains("new:"), "{out}");
    assert!(err.contains("new upstream pos: "), "{err}");
    let (code, _, _) = setup.odx(&["-C", "..", "dashboard"]);
    assert_eq!(code, 2);
    // Without uv.lock the index section is a warning.
    std::fs::remove_file(setup.project.join("uv.lock")).unwrap();
    std::fs::remove_dir_all(setup.root.join("cache").join("snapshots")).unwrap();
    let (code, _, err) = setup.odx(&["--offline", "dashboard"]);
    assert_eq!(code, 0);
    assert!(err.contains("warning: auto-update: "), "{err}");
}

#[test]
fn ports_that_conflict() {
    let setup = Setup::new();
    setup.odx(&["aggregate"]);
    setup.odx(&["dashboard"]);
    // 17.0 changes the same file meanwhile: the cherry-pick stops.
    git(&setup.up, &["checkout", "--quiet", "17.0"]);
    change(&setup.up, &[("pos_x/b.py", "other\n")], "[IMP] pos_x: b");
    let port = ["dashboard", "act", "fwd:OCA/pos#2->17.0", "--port"];
    let (code, _, err) = setup.odx(&port);
    assert_eq!(code, 1);
    assert!(
        err.contains("cache/ports/OCA-pos:\n") || err.contains("ports"),
        "{err}"
    );
    assert!(err.contains("odx port --continue"), "{err}");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario, step by step"
)]
fn actions() {
    let setup = Setup::new();
    setup.odx(&["aggregate"]);
    for (args, message) in [
        (
            &["dashboard", "act", "nope", "--drop"][..],
            "no dashboard entry nope",
        ),
        (
            &["dashboard", "act", "fwd:OCA/pos#2->17.0", "--drop"],
            "offers: port, mute",
        ),
        (
            &[
                "dashboard",
                "act",
                "fwd:OCA/pos#3->17.0",
                "--mute",
                "--reason",
                "x",
            ],
            "offers: nothing",
        ),
        (
            &["dashboard", "act", "fwd:OCA/pos#2->17.0", "--mute"],
            "--mute needs --reason",
        ),
    ] {
        let (code, _, err) = setup.odx(args);
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains(message), "{args:?}: {err}");
    }
    // Ported in a clone under the cache, once a clone can be made there.
    let port = ["dashboard", "act", "fwd:OCA/pos#2->17.0", "--port"];
    let clone = setup.root.join("cache/ports/OCA-pos");
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    std::fs::write(&clone, "").unwrap();
    let (code, _, err) = setup.odx(&port);
    assert_eq!(code, 1);
    assert!(err.contains("git clone failed"), "{err}");
    std::fs::remove_file(&clone).unwrap();
    // Nor while its lock cannot be taken.
    let lock = setup.root.join("cache/ports/OCA-pos.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir_all(&lock).unwrap();
    let (code, _, err) = setup.odx(&port);
    assert_eq!(code, 1);
    assert!(err.contains("OCA-pos.lock: "), "{err}");
    std::fs::remove_dir(&lock).unwrap();
    let (code, out, err) = setup.odx(&port);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("ported on branch 17.0-port-pos_x-OCA-pos-2: 1 commit(s) cherry-picked\n"),
        "{out}"
    );
    assert_eq!(git(&clone, &["log", "-1", "--format=%s"]), "[FIX] pos_x: b");
    let (code, _, err) = setup.odx(&port);
    assert_eq!(code, 1);
    assert!(
        err.contains("cannot create branch 17.0-port-pos_x-OCA-pos-2"),
        "{err}"
    );
    // With a pull request the forge does not take, the port stays.
    git(&clone, &["checkout", "--quiet", "--detach"]);
    git(&clone, &["branch", "-D", "17.0-port-pos_x-OCA-pos-2"]);
    let mut open = port.to_vec();
    open.push("--open-pr");
    let (code, out, err) = setup.odx(&open);
    assert_eq!(code, 1, "{err}");
    assert!(
        out.contains("ported on branch 17.0-port-pos_x-OCA-pos-2"),
        "{out}"
    );
    assert!(err.contains("cannot see"), "{err}");
    let (code, out, err) = setup.odx(&[
        "dashboard",
        "act",
        "fwd:OCA/pos#2->17.0",
        "--mute",
        "--reason",
        "not for us",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.starts_with("recorded m:17.0:pos_x:"), "{out}");
    let (_, out, _) = setup.odx(&["--fresh", "dashboard", "--section", "suppressed"]);
    assert!(out.contains("fwd:OCA/pos#2->17.0"), "{out}");
    // A deferred mute resurfaces when the 17.0 code it was about changes.
    let (code, out, err) = setup.odx(&[
        "dashboard",
        "act",
        "bwd:OCA/pos#9->17.0",
        "--mute",
        "--kind",
        "deferred",
        "--reason",
        "later",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    git(&setup.up, &["checkout", "--quiet", "17.0"]);
    change(
        &setup.up,
        &[("pos_x/d.py", "d, the 17.0 way\n")],
        "[IMP] pos_x: d",
    );
    let (_, out, _) = setup.odx(&["--fresh", "dashboard", "--section", "backport"]);
    assert!(
        out.contains("previously declined (deferred): later (target code changed since "),
        "{out}"
    );
    let pyproject = setup.pyproject();
    std::fs::write(
        setup.project.join("pyproject.toml"),
        pyproject.replace(
            "default_from",
            "migration_commit_patterns = [\"(\"]\ndefault_from",
        ),
    )
    .unwrap();
    let (code, _, _) = setup.odx(&["--fresh", "dashboard"]);
    assert_eq!(code, 2);
    std::fs::write(setup.project.join("pyproject.toml"), pyproject).unwrap();

    let (code, out, err) = setup.odx(&["dashboard", "act", "new:OCA/pos#5", "--include"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        setup.pyproject().contains("{ pr = 5 }"),
        "{}",
        setup.pyproject()
    );
    let (code, out, err) = setup.odx(&[
        "dashboard",
        "act",
        "pr:OCA/pos#1",
        "--drop",
        "--reason",
        "merged",
    ]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        !setup.pyproject().contains("{ pr = 1 }"),
        "{}",
        setup.pyproject()
    );
    let (_, out, err) = setup.odx(&[
        "dashboard",
        "act",
        "pkg:odoo-addon-web-z@17.0.1.0.1",
        "--follow",
    ]);
    assert!(
        out.contains("odoo-addon-web-z 17.0.1.0.0 -> 17.0.1.0.1"),
        "{out}{err}"
    );
}
