//! `odx lint`: odoo-lint in-process with the project's context.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::Path;
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

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

/// A manifest with a key set to its default (a safe fix removes it) and
/// no license or author (findings no fix clears).
const MANIFEST: &str = "{\n    'name': 'X',\n    'version': '18.0.1.0.0',\n    'depends': ['base'],\n    'installable': True,\n}\n";

/// A project with two own addons, a vendored group and installed addons.
fn project(root: &Path) {
    write(
        &root.join("pyproject.toml"),
        "[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"18.0\"\n\n\
         [tool.odx.groups]\noca = \"modules/oca\"\nproject_addons = [\"modules/custom\"]\n\n\
         [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = []\n",
    );
    write(&root.join("modules/custom/a/__manifest__.py"), MANIFEST);
    write(&root.join("modules/custom/b/__manifest__.py"), MANIFEST);
    write(&root.join("modules/oca/c/__manifest__.py"), MANIFEST);
    write(
        &root.join(".venv/lib/python3.12/site-packages/odoo/addons/base/__manifest__.py"),
        "{}\n",
    );
    write(&root.join(".venv/lib/README"), "not a python\n");
}

#[test]
fn findings_formats_and_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let (exit, out, err) = odx(root, &["lint"]);
    assert_eq!(exit, Exit::Failure, "{err}");
    // Only the own addons, relative to where odx runs.
    assert!(out.contains("modules/custom/a/__manifest__.py:"), "{out}");
    assert!(!out.contains("modules/oca"), "{out}");
    assert!(out.ends_with(" finding(s)\n"), "{out}");
    let (exit, out, _) = odx(root, &["lint", "--output-format", "json", "--exit-zero"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(value.as_array().is_some_and(|all| !all.is_empty()), "{out}");
    let (_, out, _) = odx(root, &["lint", "modules/oca", "--output-format", "github"]);
    assert!(out.starts_with("::"), "{out}");
    assert!(out.contains("modules/oca/c/__manifest__.py"), "{out}");
    // Selecting a rule that does not exist finds nothing, and says why.
    let (exit, out, err) = odx(root, &["lint", "--select", "NOPE9999"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, "no findings\n"));
    assert!(err.contains("warning: "), "{err}");
    // Fixes: shown, then applied.
    let manifest = root.join("modules/custom/a/__manifest__.py");
    let (exit, out, _) = odx(root, &["lint", "--diff", "--ignore", "C8101"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("installable"), "{out}");
    assert!(out.ends_with("fix(es) would change 2 file(s)\n"), "{out}");
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), MANIFEST);
    let (_, out, _) = odx(root, &["lint", "--fix"]);
    assert!(out.contains("fixed 2 finding(s) in 2 file(s)\n"), "{out}");
    assert!(
        !std::fs::read_to_string(&manifest)
            .unwrap()
            .contains("installable")
    );
    let (_, out, _) = odx(
        root,
        &["lint", "--fix", "--unsafe-fixes", "--output-format", "json"],
    );
    assert!(!out.contains("fixed"), "{out}");
    let (exit, out, _) = odx(root, &["lint", "--diff", "--exit-zero"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(out, "0 fix(es) would change 0 file(s)\n");
    let (exit, _, _) = odx(root, &["lint", "--diff"]);
    assert_eq!(exit, Exit::Success);
    // Configuration odoo-lint or odx cannot read.
    write(&root.join("odoo-lint.toml"), "select = 3\n");
    let (exit, _, err) = odx(root, &["lint"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("odoo-lint: "), "{err}");
    write(
        &root.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = 3\n",
    );
    let (exit, _, _) = odx(root, &["lint"]);
    assert_eq!(exit, Exit::Usage);
}

#[test]
fn outside_a_project_and_clean_addons() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join("x/__manifest__.py"),
        "{\n    'name': 'X',\n    'version': '18.0.1.0.0',\n    'license': 'AGPL-3',\n    'author': 'Odoo Community Association (OCA)',\n    'website': 'https://github.com/OCA/x',\n    'depends': ['base'],\n}\n",
    );
    write(&root.join("x/__init__.py"), "");
    write(&root.join("x/README.rst"), "X\n=\n");
    let (exit, out, err) = odx(root, &["lint", "--ignore", "C8112,C8105,C8107,W8150"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert_eq!(out, "no findings\n");
    // Paths elsewhere are shown as they are.
    let other = tempfile::tempdir().unwrap();
    write(&other.path().join("y/__manifest__.py"), MANIFEST);
    let elsewhere = other.path().join("y").display().to_string();
    let (exit, out, _) = odx(root, &["lint", &elsewhere]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.starts_with(&elsewhere), "{out}");
}

#[test]
fn only_changed_addons() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Not a repository yet, and nothing installed.
    project(root);
    std::fs::remove_dir_all(root.join(".venv")).unwrap();
    let (exit, _, err) = odx(root, &["lint", "--only-changed"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("needs a git repository"), "{err}");
    git(root, &["init", "--quiet"]);
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "init"]);
    // Nowhere to compare with, or a base that does not exist.
    let (exit, _, err) = odx(root, &["lint", "--only-changed"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("--base"), "{err}");
    let (exit, _, err) = odx(root, &["lint", "--only-changed", "--base", "nope"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("--base nope"), "{err}");
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/HEAD", &head]);
    let (exit, out, _) = odx(root, &["lint", "--only-changed"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, "no addon changed\n"));
    // A change to b, and a new file in a vendored addon odx does not lint.
    write(&root.join("modules/custom/b/models/x.py"), "x = 1\n");
    write(&root.join("modules/oca/c/y.py"), "y = 1\n");
    let (_, out, _) = odx(root, &["lint", "--only-changed"]);
    assert!(out.contains("modules/custom/b/"), "{out}");
    assert!(!out.contains("modules/custom/a/"), "{out}");
    assert!(!out.contains("modules/oca"), "{out}");
    git(root, &["add", "-A"]);
    git(root, &["commit", "--quiet", "-m", "b"]);
    let (_, out, _) = odx(
        root,
        &["lint", "--only-changed", "--base", &head, "modules/oca"],
    );
    assert!(out.contains("modules/oca/c/"), "{out}");
    assert!(!out.contains("modules/custom"), "{out}");
}

#[test]
fn the_embedded_odoo_lint_version() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, out, _) = odx(tmp.path(), &["--version"]);
    assert!(
        out.ends_with(concat!(
            " (odoo-lint ",
            odx_lint::odoo_lint_version!(),
            ")\n"
        )),
        "{out}"
    );
}
