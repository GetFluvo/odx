//! `odx dev` workspace mode: addons path, `odoo.conf`, editable installs.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// `path` with the platform's separators, as odx writes it.
fn native(path: &Path) -> String {
    path.components().collect::<PathBuf>().display().to_string()
}

struct Project {
    tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    /// Project addons `a` (with a pyproject.toml) and `b`, an aggregated
    /// repository with `pos_x` (pyproject.toml, selected) and `other` (not
    /// selected, with pyproject.toml), and a vendored group.
    fn new(dev: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.groups]\noca = \"modules/oca\"\nproject_addons = [\"modules/custom\"]\n\n\
                 [tool.odx.dev]\n{dev}\n\n\
                 [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\naddons = [\"pos_*\"]\nmerges = []\n"
            ),
        );
        for addon in [
            "modules/custom/a",
            "modules/custom/b",
            "vendor/pos/pos_x",
            "vendor/pos/other",
            "modules/oca/x",
        ] {
            write(&root.join(addon).join("__manifest__.py"), "{}");
        }
        for addon in ["modules/custom/a", "vendor/pos/pos_x", "vendor/pos/other"] {
            write(&root.join(addon).join("pyproject.toml"), "[build-system]\n");
        }
        let env = MapEnv::default().with(
            "ODX_CACHE_DIR",
            tmp.path().join("cache").display().to_string(),
        );
        Self { tmp, root, env }
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

    fn path(&self) -> String {
        ["modules/custom", "vendor/pos", "modules/oca"]
            .iter()
            .map(|dir| native(&self.root.join(dir)))
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[test]
fn addons_path_and_odoo_conf() {
    let project = Project::new("odoo_conf = \"odoo.conf\"\naddons_path_file = \"addons.txt\"");
    write(
        &project.root.join("odoo.conf"),
        "[options]\ndb_name = d\naddons_path = /old\n",
    );
    let (exit, out, _) = project.odx(&["dev", "--print-addons-path"]);
    assert_eq!(
        (exit, out),
        (Exit::Success, format!("{}\n", project.path()))
    );
    // A package in develop mode comes right after the project's addons.
    write(&project.root.join(".src/p/addon/__manifest__.py"), "{}");
    write(
        &project.root.join(".odx/develop.toml"),
        "[packages.p]\nkey = \"p\"\npath = \".src/p/addon\"\n",
    );
    let (_, out, _) = project.odx(&["dev", "--print-addons-path"]);
    let custom = native(&project.root.join("modules/custom"));
    let src = native(&project.root.join(".src/p"));
    assert!(out.starts_with(&format!("{custom},{src},")), "{out}");
    std::fs::remove_file(project.root.join(".odx/develop.toml")).unwrap();

    let (_, out, _) = project.odx(&["dev", "--dry-run", "--no-editable"]);
    assert_eq!(
        out,
        format!(
            "addons path: {}\nwould write addons.txt\nwould write addons_path in odoo.conf\n",
            project.path()
        )
    );
    assert!(!project.root.join("addons.txt").exists());

    let (exit, _, err) = project.odx(&["dev", "--no-editable"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        std::fs::read_to_string(project.root.join("addons.txt")).unwrap(),
        format!("{}\n", project.path())
    );
    assert_eq!(
        std::fs::read_to_string(project.root.join("odoo.conf")).unwrap(),
        format!("[options]\naddons_path = {}\ndb_name = d\n", project.path())
    );

    // No virtual environment: editable installs are skipped with a warning.
    let (exit, _, err) = project.odx(&["dev"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        err.contains(".venv: no virtual environment; editable installs skipped"),
        "{err}"
    );
}

#[test]
fn project_addon_directory_that_is_an_addon() {
    let project = Project::new("editable = false");
    let pyproject = std::fs::read_to_string(project.root.join("pyproject.toml")).unwrap();
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace(
            "project_addons = [\"modules/custom\"]",
            "project_addons = [\"modules/custom/a\", \"missing\", \"modules/oca\"]",
        ),
    );
    let (_, out, _) = project.odx(&["dev"]);
    // modules/oca is both a project directory and a group: listed once.
    let expected = ["modules/custom", "modules/oca", "vendor/pos"]
        .iter()
        .map(|dir| native(&project.root.join(dir)))
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        out.starts_with(&format!("addons path: {expected}\n")),
        "{out}"
    );
    assert!(project.root.join(".odx/addons-path").exists());
    assert_eq!(out.lines().count(), 2, "{out}");
}

#[test]
fn editable_installs() {
    let mut project = Project::new("venv = \"env\"");
    std::fs::create_dir_all(project.root.join("env")).unwrap();
    project.env = project.env.clone().with("ODX_UV", "git");
    let (exit, _, err) = project.odx(&["dev"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("git pip install failed"), "{err}");
    // Nothing ran a stand-in for uv yet.
    assert!(!project.tmp.path().join("uv.log").exists());
    let (exit, out, _) = project.odx(&["dev", "--dry-run", "--venv", "env"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.ends_with("would install 2 addons in editable mode into env\n"),
        "{out}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let uv = project.tmp.path().join("fake-uv");
        let log = project.tmp.path().join("uv.log");
        write(
            &uv,
            &format!("#!/bin/sh\necho \"$@\" > '{}'\n", log.display()),
        );
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        project.env = project.env.clone().with("ODX_UV", uv.display().to_string());
        let (exit, out, err) = project.odx(&["dev"]);
        assert_eq!((exit, err.as_str()), (Exit::Success, ""));
        assert!(
            out.ends_with("installed 2 addons in editable mode into env\n"),
            "{out}"
        );
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            format!(
                "pip install --python {} -e {} -e {}\n",
                project.root.join("env").display(),
                project.root.join("modules/custom/a").display(),
                project.root.join("vendor/pos/pos_x").display()
            )
        );
    }
}

#[test]
fn workspace_failures() {
    let project =
        Project::new("odoo_conf = \"conf/odoo.conf\"\naddons_path_file = \"blocked/path\"");
    write(&project.root.join("blocked"), "a file");
    let (exit, _, err) = project.odx(&["dev"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("blocked"), "{err}");
    std::fs::remove_file(project.root.join("blocked")).unwrap();
    let (exit, _, err) = project.odx(&["dev"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odoo.conf"), "{err}");
    write(&project.root.join(".odx/develop.toml"), "broken");
    let (exit, _, _) = project.odx(&["dev"]);
    assert_eq!(exit, Exit::Usage);
    let (exit, _, _) = project.odx(&["-C", "/", "dev"]);
    assert_eq!(exit, Exit::Usage);
}

#[test]
fn nothing_to_install_in_editable_mode() {
    let project = Project::new("");
    for addon in ["modules/custom/a", "vendor/pos/pos_x", "vendor/pos/other"] {
        std::fs::remove_file(project.root.join(addon).join("pyproject.toml")).unwrap();
    }
    let (exit, out, err) = project.odx(&["dev"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(out.lines().count(), 2, "{out}");
}
