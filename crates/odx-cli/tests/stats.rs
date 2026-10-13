//! `odx stats`.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::Path;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn odx(root: &Path, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let env = MapEnv::default().with("ODX_CACHE_DIR", root.join(".cache").display().to_string());
    let exit = odx_cli::run_with_env(command_line, &OsFs, &env, root, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// A project with one custom addon, one vendored group addon and one
/// aggregated repository addon.
fn project(root: &Path) {
    write(
        &root.join("pyproject.toml"),
        "[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"17.0\"\n\n\
         [tool.odx.groups]\noca = \"modules/oca\"\nproject_addons = [\"modules/custom\"]\n\n\
         [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = []\n",
    );
    write(&root.join("modules/custom/a/__manifest__.py"), "{}\n");
    write(
        &root.join("modules/custom/a/models/a.py"),
        "# A.\nimport os\n\nx = 1\n",
    );
    write(&root.join("modules/custom/a/views/a.xml"), "<odoo/>\n");
    write(&root.join("modules/oca/b/__manifest__.py"), "{}\n");
    write(&root.join("vendor/pos/pos_x/__manifest__.py"), "{}\n");
    write(&root.join("vendor/pos/pos_x/x.js"), "let x;\n");
}

#[test]
fn stats_per_language() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path());
    let (exit, out, err) = odx(tmp.path(), &["stats"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        "Language   Files     Lines      Code  Comments    Blanks\n\
         Python         2         5         3         1         1\n\
         XML            1         1         1         0         0\n\
         Total          3         6         4         1         1\n"
    );

    let (_, out, _) = odx(tmp.path(), &["stats", "--include-vendor"]);
    assert!(out.contains("JavaScript       1"), "{out}");
    assert!(out.contains("\nTotal            6         9"), "{out}");

    let (_, out, _) = odx(tmp.path(), &["stats", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["total"]["code"], 4);
    assert_eq!(value["languages"]["Python"]["code"], 3);
    let report = &value["languages"]["XML"]["reports"][0]["name"];
    assert_eq!(
        report.as_str().unwrap().replace('\\', "/"),
        "modules/custom/a/views/a.xml"
    );
}

#[test]
fn stats_per_addon() {
    let tmp = tempfile::tempdir().unwrap();
    project(tmp.path());
    let (exit, out, _) = odx(tmp.path(), &["stats", "--per-addon", "--include-vendor"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "Addon              Files     Lines      Code  Comments    Blanks\n\
         modules/custom/a       3         6         4         1         1\n\
         modules/oca/b          1         1         1         0         0\n\
         vendor/pos/pos_x       2         2         2         0         0\n\
         Total                  6         9         7         1         1\n"
    );
    let (_, out, _) = odx(tmp.path(), &["stats", "--per-addon", "--json"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["addons"]["modules/custom/a"]["Python"]["code"], 3);
    assert_eq!(value["total"]["files"], 3);
}

#[test]
fn stats_without_addons_or_project() {
    let tmp = tempfile::tempdir().unwrap();
    let (exit, _, err) = odx(tmp.path(), &["stats"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("pyproject.toml"), "{err}");
    write(
        &tmp.path().join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\n",
    );
    let (exit, out, err) = odx(tmp.path(), &["stats"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        err.starts_with("warning: stats: no addon directories found"),
        "{err}"
    );
    assert!(
        out.ends_with("Total          0         0         0         0         0\n"),
        "{out}"
    );
}
