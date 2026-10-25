//! `odx check-deps` on a project with its own addons, an aggregated
//! repository, addon packages and a vendored group.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
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
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A package index on localhost knowing `missing_z` on 17.0 only.
fn index() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let home = base.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 2048];
            let n = stream.read(&mut request).unwrap_or(0);
            let text = String::from_utf8_lossy(request.get(..n).unwrap_or_default());
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let body = match path.as_str() {
                "/pypi/odoo-addon-missing-z/json" => r#"{"releases":{"17.0.1.0.0":[]}}"#.to_owned(),
                "/pypi/odoo-addon-missing-z/17.0.1.0.0/json" => {
                    format!(r#"{{"info":{{"home_page":"{home}/OCA/z"}}}}"#)
                }
                _ => String::new(),
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

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        for addon in ["pos_agg", "pos_x", "other"] {
            write(
                &up.join(addon).join("__manifest__.py"),
                "{\"depends\": [\"base\"]}",
            );
        }
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[project]\nname = \"p\"\nversion = \"1\"\ndependencies = [\"phonenumbers\", \"odoo-addon-main-only\"]\n\n[project.optional-dependencies]\noca = [\"odoo-addon-pos-x\", \"odoo-addon-unused-y\", \"requests\"]\n\n[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.groups]\ndefault = \"oca\"\nproject_addons = [\"addons\"]\noca = \"vendor/oca\"\n\n[tool.odx.check_deps]\nprovided = [\"account_accountant\"]\n\n[[tool.odx.repositories]]\nname = \"agg\"\nurl = \"{}\"\naddons = [\"pos_*\"]\nmerges = []\n",
                file_url(&up)
            ),
        );
        write(
            &root.join("addons/my_mod/__manifest__.py"),
            r#"{
    "name": "Mine",
    "license": "LGPL-3",
    "depends": ["sale", "account_accountant", "pos_x", "pos_agg", "agpl_dep", "missing_z", "gone_w"],
    "external_dependencies": {"python": ["phonenumbers", "python-stdnum>=1.18", "!odd"]},
}"#,
        );
        write(
            &root.join("addons/agpl_dep/__manifest__.py"),
            r#"{"license": "AGPL-3", "depends": ["base"]}"#,
        );
        git(&root, &["init", "--quiet"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "project"]);
        // Not committed.
        write(&root.join("addons/new_mod/__manifest__.py"), "{}");
        // A vendored group with an addon odx vendor did not put there.
        write(
            &root.join(".odx/vendor.lock"),
            "version = 1\n\n[[addon]]\nname = \"kept_v\"\ngroup = \"oca\"\npackage = \"odoo-addon-kept-v\"\nsource = \"1\"\nsource_version = \"1\"\nwritten_version = \"1\"\nsource_content = \"x\"\ntree = \"y\"\n",
        );
        write(&root.join("vendor/oca/kept_v/__manifest__.py"), "{}");
        write(&root.join("vendor/oca/stray_v/__manifest__.py"), "{}");
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            )
            .with("ODX_PYPI_URL", format!("{}/pypi", index()));
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

    fn edit(&self, from: &str, to: &str) {
        let text = std::fs::read_to_string(self.root.join("pyproject.toml")).unwrap();
        assert!(text.contains(from), "{from}");
        std::fs::write(self.root.join("pyproject.toml"), text.replacen(from, to, 1)).unwrap();
    }
}

fn path(text: &str) -> String {
    text.split('/').collect::<PathBuf>().display().to_string()
}

#[test]
fn every_check() {
    let project = Project::new();
    // Before the aggregated repository is built, its cached base counts.
    project.odx(&["aggregate", "--fetch-only"]);
    let (exit, out, _) = project.odx(&["check-deps"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.contains("duplicate: pos_x in repository agg, package odoo-addon-pos-x (oca)\n"),
        "{out}"
    );
    assert!(!out.contains("missing: pos_agg"), "{out}");

    let (exit, _, err) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = project.odx(&["check-deps", "--unused", "--licenses"]);
    assert_eq!((exit, err.as_str()), (Exit::Failure, ""));
    assert_eq!(
        out,
        format!(
            "missing: missing_z (needed by my_mod)\n\
             missing: gone_w (needed by my_mod)\n\
             missing python package: python-stdnum (needed by my_mod)\n\
             missing python package: !odd (needed by my_mod)\n\
             duplicate: pos_x in repository agg, package odoo-addon-pos-x (oca)\n\
             stray: {} (not vendored by odx vendor)\n\
             stray: {} (not tracked by git)\n\
             unused: odoo-addon-main-only (no scanned addon depends on it)\n\
             unused: odoo-addon-unused-y (no scanned addon depends on it)\n\
             license: my_mod (LGPL-3) depends on agpl_dep (AGPL-3)\n",
            path("vendor/oca/stray_v"),
            path("addons/new_mod")
        )
    );
    let (_, out, _) = project.odx(&["--json", "check-deps", "--licenses"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["licenses"][0]["depends_on"], "agpl_dep");
    assert_eq!(value["missing"][0]["addon"], "missing_z");
    assert_eq!(value["stray"].as_array().unwrap().len(), 2);

    // --fix adds what the package index has.
    let (exit, out, _) = project.odx(&["check-deps", "--fix"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.starts_with("added odoo-addon-missing-z to group oca; run uv lock\nmissing: gone_w"),
        "{out}"
    );
    let text = std::fs::read_to_string(project.root.join("pyproject.toml")).unwrap();
    assert!(
        text.contains("\"requests\", \"odoo-addon-missing-z\"]"),
        "{text}"
    );
    // A project file that cannot be written.
    let file = project.root.join("pyproject.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace(", \"odoo-addon-missing-z\"", "")).unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions.clone()).unwrap();
    let (exit, _, err) = project.odx(&["check-deps", "--fix"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pyproject.toml: "), "{err}");
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "a test file, made writable again"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&file, permissions).unwrap();
    // Offline, nothing is found; a group that is not declared, or none.
    let (_, out, _) = project.odx(&["--offline", "check-deps", "--fix"]);
    assert!(out.starts_with("missing: missing_z"), "{out}");
    project.edit(
        "default = \"oca\"",
        "default = \"nope\"\nnope = \"vendor/nope\"",
    );
    let (exit, _, err) = project.odx(&["check-deps", "--fix"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("dependency group \"nope\" is not declared"),
        "{err}"
    );
    project.edit("default = \"nope\"\nnope = \"vendor/nope\"\n", "");
    let (exit, _, err) = project.odx(&["check-deps", "--fix"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("--fix needs [tool.odx.groups] default"),
        "{err}"
    );
}

#[test]
fn a_clean_project_and_an_unknown_series() {
    let project = Project::new();
    std::fs::remove_dir_all(project.root.join("addons")).unwrap();
    std::fs::remove_dir_all(project.root.join("vendor")).unwrap();
    std::fs::remove_dir_all(project.root.join(".git")).unwrap();
    std::fs::remove_dir_all(project.root.join(".odx")).unwrap();
    project.edit("addons = [\"pos_*\"]", "addons = [\"pos_agg\"]");
    project.odx(&["aggregate"]);
    let (exit, out, err) = project.odx(&["check-deps"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert_eq!(out, "the dependencies of 1 addons are provided\n");
    // Manifests odx cannot read are said to be unchecked, not fine.
    write(
        &project.root.join("addons/broken/__manifest__.py"),
        "{'name': ",
    );
    std::fs::create_dir_all(project.root.join("addons/latin")).unwrap();
    std::fs::write(
        project.root.join("addons/latin/__manifest__.py"),
        b"{'name': 'bad \xff'}",
    )
    .unwrap();
    let (_, _, err) = project.odx(&["check-deps"]);
    for addon in ["broken", "latin"] {
        assert!(
            err.contains(&format!("{addon}: the manifest cannot be read (")),
            "{err}"
        );
    }
    std::fs::remove_dir_all(project.root.join("addons")).unwrap();
    project.edit("odoo_version = \"17.0\"", "odoo_version = \"15.0\"");
    let (exit, out, err) = project.odx(&["check-deps"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("no list of Odoo's own addons for 15.0"),
        "{err}"
    );
    assert!(out.contains("missing: base (needed by pos_agg)"), "{out}");
    let (exit, _, _) = project.odx(&["-C", "..", "check-deps"]);
    assert_eq!(exit, Exit::Usage);
}
