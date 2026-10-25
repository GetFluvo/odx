//! `odx doctor`, with a forge served on localhost.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::{BufRead, BufReader, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A Gitea-like API on localhost answering `routes` (path → status, body)
/// and 404 otherwise; returns its origin.
fn forge(routes: Vec<(&'static str, u16, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let (status, body) = routes
                .iter()
                .find(|(route, _, _)| *route == path)
                .map_or((404, ""), |(_, status, body)| (*status, *body));
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(response.as_bytes());
        }
    });
    origin
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    fn new(extra: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n{extra}\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = []\n"
            ),
        );
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with("ODX_UV", "odx-surely-missing-uv")
            .with("ODX_MERGIRAF", "odx-surely-missing-mergiraf")
            .with("ODX_ODL", "odx-surely-missing-odl")
            .with("GITHUB_TOKEN", "t");
        Self {
            _tmp: tmp,
            root,
            env,
        }
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
}

const VERSION: (&str, u16, &str) = ("/api/v1/version", 200, "{\"version\":\"1.21\"}");

#[test]
fn a_protected_private_vault_passes() {
    let origin = forge(vec![
        VERSION,
        ("/api/v1/repos/odoo/vault", 200, "{\"private\":true}"),
        (
            "/api/v1/repos/odoo/vault/tag_protections",
            200,
            "[{\"name_pattern\":\"odx/*\"}]",
        ),
    ]);
    let project = Project::new(&format!(
        "mergiraf = \"auto\"\n[tool.odx.mirror]\nurl = \"{origin}/odoo/vault.git\"\nref_style = \"tags\"\n"
    ));
    let (exit, out, err) = project.odx(&["doctor"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    for line in [
        "ok   git: git version ",
        "warn mergiraf: not found",
        "warn uv: odx-surely-missing-uv not found",
        "info odl: odx-surely-missing-odl not found",
        "ok   project: ",
        "info languages: English only",
        "ok   token github.com: found",
        "warn token 127.0.0.1: none",
        "ok   vault visibility: ",
        "ok   vault protection: odx/* tags cannot be deleted or moved",
        "ok   platform: generic-wheels, provided set odoo-standard-17.0 (",
        "info cache: ",
        "info background sync: not installed",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    let (_, out, _) = project.odx(&["--json", "doctor"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["checks"][0]["item"], "git");
}

#[test]
fn an_unprotected_or_public_vault_fails() {
    let origin = forge(vec![
        VERSION,
        ("/api/v1/repos/odoo/vault", 200, "{\"private\":false}"),
        ("/api/v1/repos/odoo/vault/tag_protections", 200, "[]"),
        (
            "/api/v1/repos/odoo/internal",
            200,
            "{\"visibility\":\"internal\"}",
        ),
        ("/api/v1/repos/odoo/internal/tag_protections", 200, "[]"),
    ]);
    let project = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{origin}/odoo/vault.git\"\nref_style = \"tags\"\n"
    ));
    let (exit, out, err) = project.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("FAIL vault visibility: "), "{out}");
    assert!(
        out.contains("FAIL vault protection: odx/* tags are not protected"),
        "{out}"
    );
    assert_eq!(err, "error: 2 checks failed\n");

    let allowed = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{origin}/odoo/vault.git\"\nallow_public = true\n"
    ));
    let (_, out, _) = allowed.odx(&["doctor"]);
    assert!(out.contains("warn vault visibility: "), "{out}");
    assert!(
        out.contains("FAIL vault protection: refs/odx/* cannot be protected"),
        "{out}"
    );

    let internal = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{origin}/odoo/internal.git\"\nref_style = \"tags\"\n"
    ));
    let (_, out, _) = internal.odx(&["doctor"]);
    assert!(out.contains("warn vault visibility: "), "{out}");

    let missing = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{origin}/odoo/nothing.git\"\n"
    ));
    let (exit, out, _) = missing.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("FAIL vault: "), "{out}");
}

#[test]
fn vaults_that_are_not_checked() {
    let none =
        Project::new("mergiraf = \"never\"\n[tool.odx.platform]\nprovided = \"missing.txt\"\n");
    write(
        &none.root.join(".odx/develop.toml"),
        "[packages.x]\nkey = \"x\"\npath = \".src/x\"\n",
    );
    let (exit, out, _) = none.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure);
    for line in [
        "info mergiraf: off",
        "warn vault: no [tool.odx.mirror]",
        "FAIL platform: ",
        "warn develop mode: x; ",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    let local = Project::new("[tool.odx.mirror]\nurl = \"/srv/vault.git\"\n");
    let (_, out, _) = local.odx(&["doctor"]);
    assert!(out.contains("info vault: /srv/vault.git is local"), "{out}");
    let offline =
        Project::new("[tool.odx.mirror]\nurl = \"https://forge.example/odoo/vault.git\"\n");
    let (_, out, _) = offline.odx(&["--offline", "doctor"]);
    assert!(out.contains("warn vault: not checked (--offline)"), "{out}");
    let always = Project::new("mergiraf = \"always\"\n");
    let (exit, out, _) = always.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("FAIL mergiraf: "), "{out}");
    // Outside a project only the machine is checked.
    let mut outside = always;
    outside.env = MapEnv::default().with("ODX_UV", "odx-surely-missing-uv");
    let (_, out, _) = outside.odx(&["-C", "/", "doctor"]);
    assert!(out.contains("info project: no pyproject.toml"), "{out}");
}

#[test]
fn forges_that_do_not_answer() {
    // No forge API at all, and rules the token may not read.
    let origin = forge(vec![
        VERSION,
        ("/api/v1/repos/odoo/vault", 200, "{\"private\":true}"),
        ("/api/v1/repos/odoo/vault/tag_protections", 500, ""),
    ]);
    let unreadable = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{origin}/odoo/vault.git\"\nref_style = \"tags\"\n"
    ));
    let (exit, out, _) = unreadable.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("FAIL vault: "), "{out}");
    let silent = forge(vec![]);
    let unknown = Project::new(&format!(
        "[tool.odx.mirror]\nurl = \"{silent}/odoo/vault.git\"\n"
    ));
    let (_, out, _) = unknown.odx(&["doctor"]);
    assert!(
        out.contains("does not look like GitHub, GitLab, Gitea or Forgejo"),
        "{out}"
    );
}

#[test]
fn a_project_that_does_not_load_fails() {
    let project = Project::new("jobs = \"many\"");
    let (exit, out, _) = project.odx(&["doctor"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(out.contains("FAIL project: "), "{out}");
    assert!(out.contains("pyproject.toml:3:8: invalid type"), "{out}");
}

#[test]
fn everything_found() {
    // git stands in for mergiraf and odl: both answer --version.
    let mut project = Project::new(
        "languages = [\"es\", \"nl\"]\n[tool.odx.platform]\nprovided = \"constraints.txt\"\nprovided_extra = [\"a==1\"]\n\n\
         [tool.odx.packages]\nodoo-addon-x = { url = \"https://gitlab.example/odoo/x.git\", branch = \"17.0\" }\n",
    );
    write(&project.root.join("constraints.txt"), "requests==2.31.0\n");
    project.env = project
        .env
        .clone()
        .with("ODX_MERGIRAF", "git")
        .with("ODX_ODL", "git");
    let cache = project.root.parent().unwrap().join("cache");
    write(&cache.join("repos/x/file"), "0123456789");
    write(&cache.join("big"), &"x".repeat(2048));
    let (exit, out, _) = project.odx(&["doctor"]);
    for line in [
        "ok   mergiraf: git resolves conflicts",
        "ok   odl: git version",
        "warn token gitlab.example: none",
        "info languages: English, es, nl",
        "ok   platform: generic-wheels, provided set constraints.txt (2 packages, 1 overrides)",
        "info cache: ",
        " (2.0 KB)",
    ] {
        assert!(out.contains(line), "{line}\n{out}");
    }
    assert_eq!(exit, Exit::Success, "{out}");
}
