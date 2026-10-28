//! `odx wanted` against a local server playing the package index and a
//! forge API.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

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
            let target = text.split_whitespace().nth(1).unwrap_or("/");
            let path = target.split('?').next().unwrap_or(target);
            let (status, body) = routes
                .lock()
                .unwrap()
                .get(path)
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
    api: String,
    routes: Routes,
    env: MapEnv,
}

impl Setup {
    /// A 17.0 project wanting `contract` (released on 17.0), `agreement`
    /// (on 16.0, migrating to 17.0), `rma` (on 16.0, not started) and
    /// `ghost` (nowhere).
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let routes: Routes = Arc::default();
        let api = serve(&routes);
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            "[project]\nname = \"p\"\nversion = \"1\"\n\n[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.wanted]]\naddon = \"contract\"\nnote = \"customer asks in Q3\"\n\n[[tool.odx.wanted]]\naddon = \"agreement\"\nseries = [\"17.0\", \"18.0\"]\n\n[[tool.odx.wanted]]\naddon = \"rma\"\n\n[[tool.odx.wanted]]\naddon = \"ghost\"\n",
        )
        .unwrap();
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            )
            .with("ODX_PYPI_URL", format!("{api}/pypi"));
        let setup = Self {
            _tmp: tmp,
            root,
            api,
            routes,
            env,
        };
        for (dist, version, repo) in [
            ("odoo-addon-contract", "17.0.1.0.0", "OCA/contract"),
            ("odoo-addon-agreement", "16.0.1.0.0", "OCA/agreement"),
            ("odoo-addon-rma", "16.0.2.0.0", "OCA/rma"),
        ] {
            setup.route(
                &format!("/pypi/{dist}/json"),
                &format!(r#"{{"releases":{{"{version}":[]}}}}"#),
            );
            setup.route(
                &format!("/pypi/{dist}/{version}/json"),
                &format!(r#"{{"info":{{"home_page":"{}/{repo}"}}}}"#, setup.api),
            );
        }
        setup.route("/api/v3/meta", "{}");
        setup.route(
            "/api/v3/repos/OCA/agreement/pulls",
            r#"[{"number":12,"title":"[17.0][MIG] agreement: Migration to 17.0","state":"open","base":{"ref":"17.0"},
                 "user":{"login":"joe"},"created_at":"2026-05-06T07:08:09Z"},
                {"number":13,"title":"[17.0][MIG] agreement_sale","state":"open","base":{"ref":"17.0"}},
                {"number":14,"title":"[18.0][MIG] agreement","state":"open","base":{"ref":"18.0"},
                 "user":{"login":"kim"},"created_at":"2026-06-01T00:00:00Z"}]"#,
        );
        setup.route(
            "/api/v3/repos/OCA/rma/pulls",
            r#"[{"number":4,"title":"[17.0][MIG] rma_sale","state":"open","base":{"ref":"17.0"}}]"#,
        );
        setup
    }

    fn route(&self, path: &str, body: &str) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_owned(), (200, body.to_owned()));
    }

    fn odx(&self, args: &[&str]) -> (Exit, String, String) {
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(
            command_line,
            &OsFs,
            &self.env,
            &self.root,
            &mut out,
            &mut err,
        );
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn pyproject(&self) -> String {
        std::fs::read_to_string(self.root.join("pyproject.toml")).unwrap()
    }
}

#[test]
fn where_wanted_addons_stand() {
    let setup = Setup::new();
    let (exit, out, err) = setup.odx(&["wanted"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let api = &setup.api;
    assert_eq!(
        out,
        format!(
            "contract (customer asks in Q3)\n  17.0  released in OCA/contract\n        odx add {api}/OCA/contract@17.0 --package odoo-addon-contract --subdirectory contract\nagreement\n  17.0  migrating: OCA/agreement#12 by joe, opened 2026-05-06\n        {api}/OCA/agreement/pull/12\n        odx add {api}/OCA/agreement#12 --package odoo-addon-agreement --subdirectory agreement\n  18.0  migrating: OCA/agreement#14 by kim, opened 2026-06-01\n        {api}/OCA/agreement/pull/14\nrma\n  17.0  not started; last on 16.0 in OCA/rma\nghost\n  17.0  not started; not found on an earlier series (set `repository` to watch for a migration)\n"
        )
    );
    let (exit, out, _) = setup.odx(&["--json", "wanted"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    let agreement = &value["addons"][1];
    assert_eq!(
        (
            agreement["state"].as_str(),
            agreement["pull_request"]["author"].as_str(),
            agreement["pull_request"]["number"].as_u64()
        ),
        (Some("migrating"), Some("joe"), Some(12))
    );
    assert_eq!(value["addons"][3]["last_series"], "16.0");
    assert_eq!(value["addons"][4]["state"], "not-started");
}

#[test]
fn offline_only_the_cache_answers() {
    let setup = Setup::new();
    let (exit, out, err) = setup.odx(&["--offline", "wanted"]);
    assert_eq!(exit, Exit::Success, "{err}");
    // Nothing is cached yet: odx cannot tell where the addons stand.
    assert!(
        out.contains("contract (customer asks in Q3)\n  17.0  not checked (offline)\n"),
        "{out}"
    );
    // With an index from an earlier run, a known repository is not asked.
    setup.odx(&["wanted"]);
    let (_, out, _) = setup.odx(&["--offline", "wanted"]);
    assert!(out.contains("  17.0  released in OCA/contract\n"), "{out}");
    assert!(
        out.contains("agreement\n  17.0  not checked (offline)\n"),
        "{out}"
    );
    let (_, out, _) = setup.odx(&["--offline", "--json", "wanted"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["addons"][1]["state"], "unchecked");
    assert_eq!(value["addons"][1]["repository"], serde_json::Value::Null);
    assert_eq!(value["addons"][1]["reason"], "offline");
}

#[test]
fn a_configured_repository_and_a_forge_that_fails() {
    let setup = Setup::new();
    let (exit, _, err) = setup.odx(&[
        "wanted",
        "add",
        "ghost",
        "--repository",
        &format!("{}/OCA/ghost", setup.api),
    ]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = setup.odx(&["wanted"]);
    assert_eq!(exit, Exit::Success);
    // A forge that does not answer leaves it unchecked, not "not started".
    assert!(
        out.ends_with("ghost\n  17.0  not checked (127.0.0.1 did not answer)\n"),
        "{out}"
    );
    assert!(err.contains("wanted OCA/ghost"), "{err}");
    // A repository no forge serves is not asked.
    setup.odx(&[
        "wanted",
        "add",
        "ghost",
        "--repository",
        "file:///srv/git/ghost",
    ]);
    let (_, out, err) = setup.odx(&["wanted"]);
    assert!(
        out.ends_with(
            "ghost\n  17.0  not checked (file:///srv/git/ghost is no forge repository)\n"
        ),
        "{out}"
    );
    assert!(
        err.contains("cannot look for a migration in file:///srv/git/ghost"),
        "{err}"
    );
}

#[test]
fn a_release_since_the_last_run_is_seen_at_once() {
    let setup = Setup::new();
    let (_, out, _) = setup.odx(&["wanted"]);
    assert!(out.contains("ghost\n  17.0  not started"), "{out}");
    // Released after the index learnt it was not: the next run asks again.
    setup.route(
        "/pypi/odoo-addon-ghost/json",
        r#"{"releases":{"17.0.1.0.0":[]}}"#,
    );
    setup.route(
        "/pypi/odoo-addon-ghost/17.0.1.0.0/json",
        &format!(r#"{{"info":{{"home_page":"{}/OCA/ghost"}}}}"#, setup.api),
    );
    let (_, out, _) = setup.odx(&["wanted"]);
    assert!(
        out.contains("ghost\n  17.0  released in OCA/ghost\n"),
        "{out}"
    );
}

#[test]
fn adding_and_removing() {
    let setup = Setup::new();
    let (exit, out, err) = setup.odx(&[
        "wanted",
        "add",
        "helpdesk_mgmt",
        "--series",
        "17.0,18.0",
        "--repository",
        "OCA/helpdesk",
        "--note",
        "next year",
    ]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.starts_with("helpdesk_mgmt is wanted in "), "{out}");
    assert!(!out.contains("without a repository"), "{out}");
    // Changing the note keeps the series and the repository.
    let (_, out, _) = setup.odx(&["wanted", "add", "helpdesk_mgmt", "--note", "next year"]);
    assert!(!out.contains("without a repository"), "{out}");
    let (_, out, _) = setup.odx(&["wanted", "add", "rma_sale"]);
    assert!(
        out.ends_with("without a repository, odx looks for one on the 3 series before; pass --repository for an addon that did not live there\n"),
        "{out}"
    );
    setup.odx(&["wanted", "remove", "rma_sale"]);
    assert!(
        setup.pyproject().ends_with(
            "[[tool.odx.wanted]]\naddon = \"helpdesk_mgmt\"\nseries = [\"17.0\", \"18.0\"]\nrepository = \"OCA/helpdesk\"\nnote = \"next year\"\n"
        ),
        "{}",
        setup.pyproject()
    );
    let (exit, out, _) = setup.odx(&["wanted", "remove", "rma"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("rma is no longer wanted in "), "{out}");
    assert!(!setup.pyproject().contains("\"rma\""));
    let (exit, _, err) = setup.odx(&["wanted", "remove", "rma"]);
    assert_eq!(
        (exit, err.as_str()),
        (Exit::Usage, "error: rma is not wanted\n")
    );
    std::fs::write(
        setup.root.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\nwanted = []\n",
    )
    .unwrap();
    let (exit, out, _) = setup.odx(&["wanted"]);
    assert_eq!(
        (exit, out.as_str()),
        (
            Exit::Success,
            "no wanted addons; `odx wanted add <addon>` watches one\n"
        )
    );
    let (exit, _, err) = setup.odx(&["wanted", "add", "x"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("wanted has an unexpected type"), "{err}");
    let (exit, _, _) = setup.odx(&["-C", "/nonexistent-odx-dir", "wanted"]);
    assert_eq!(exit, Exit::Usage);
}

#[test]
fn the_dashboard_shows_what_can_be_added() {
    let setup = Setup::new();
    let (exit, out, err) = setup.odx(&["--fresh", "dashboard", "--section", "wanted"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("Wanted addons"), "{out}");
    assert!(out.contains("want:agreement@17.0"), "{out}");
    assert!(out.contains("want:contract@17.0"), "{out}");
    assert!(!out.contains("want:rma@17.0"), "{out}");
    // On the next series, it is only shown: nothing to add yet.
    assert!(out.contains("want:agreement@18.0"), "{out}");
    let (exit, out, err) = setup.odx(&["dashboard", "act", "want:agreement@17.0", "--include"]);
    assert_eq!(exit, Exit::Success, "{out}{err}");
    let pyproject = setup.pyproject();
    assert!(pyproject.contains("odoo-addon-agreement"), "{pyproject}");
    assert!(
        pyproject.contains("/OCA/agreement\", pr = 12, subdirectory = \"agreement\" }"),
        "{pyproject}"
    );
    // Once added, the dashboard stops offering it.
    let (_, out, _) = setup.odx(&["--fresh", "dashboard", "--section", "wanted"]);
    assert!(!out.contains("want:agreement@17.0"), "{out}");
}

#[cfg(unix)]
#[test]
fn an_unwritable_pyproject_is_said() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new();
    let file = setup.root.join("pyproject.toml");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (exit, _, err) = setup.odx(&["wanted", "add", "x"]);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pyproject.toml: "), "{err}");
}
