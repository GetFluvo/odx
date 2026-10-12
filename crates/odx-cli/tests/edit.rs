//! `odx add`, `odx drop` and `odx import --write` against an in-memory file system.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::ErrorKind;
use std::path::Path;

use odx_core::fake::MemoryFs;
use odx_core::{Exit, FileSystem};

fn run(fs: &MemoryFs, args: &[&str]) -> (Exit, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let exit = odx_cli::run_in(command_line, fs, Path::new("/w"), &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// `/w/<name>` as odx displays it on this platform.
fn w(name: &str) -> String {
    Path::new("/w").join(name).display().to_string()
}

fn read(fs: &MemoryFs, path: &str) -> String {
    String::from_utf8(fs.read(Path::new(path)).unwrap()).unwrap()
}

fn fs_with(files: &[(&str, &str)]) -> MemoryFs {
    let fs = MemoryFs::default();
    for (path, text) in files {
        fs.write(Path::new(path), text.as_bytes()).unwrap();
    }
    fs
}

const PYPROJECT: &str = r#"[project]
name = "addons"

[project.optional-dependencies]
oca_unmerged = [
  "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
]

[tool.odx]
odoo_version = "17.0"

[tool.odx.groups]
oca_unmerged = "modules/oca_unmerged"

[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"
merges = [
    { pr = 123 },
]
"#;

#[test]
fn add_merges_and_repositories() {
    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT)]);
    let (exit, out, err) = run(&fs, &["add", "OCA/pos#200"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        format!(
            "added pull request #200 of https://github.com/OCA/pos.git to {}\n",
            w("pyproject.toml")
        )
    );
    assert!(read(&fs, "/w/pyproject.toml").contains("    { pr = 123 },\n    { pr = 200 },\n]"));
    let (_, out, _) = run(&fs, &["add", "OCA/pos#200"]);
    assert_eq!(
        out,
        "pull request #200 of https://github.com/OCA/pos.git is already configured\n"
    );
    let (_, out, _) = run(&fs, &["add", "OCA/web@abcdef1"]);
    assert!(
        out.starts_with("added commit abcdef1 of https://github.com/OCA/web.git"),
        "{out}"
    );
    let (_, out, _) = run(&fs, &["add", "OCA/web@17.0-fix"]);
    assert!(out.starts_with("added branch 17.0-fix of"), "{out}");
}

#[test]
fn add_packages() {
    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT)]);
    let (exit, out, _) = run(
        &fs,
        &[
            "add",
            "OCA/pos#7",
            "--package",
            "odoo-addon-x",
            "--subdirectory",
            "x",
            "--group",
            "oca_unmerged",
        ],
    );
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("added package odoo-addon-x to"), "{out}");
    assert!(read(&fs, "/w/pyproject.toml").contains(
        "\"odoo-addon-x @ git+https://github.com/OCA/pos.git@refs/pull/7/head#subdirectory=x\","
    ));
    let (exit, _, err) = run(
        &fs,
        &[
            "add",
            "OCA/pos#8",
            "--package",
            "odoo-addon-y",
            "--group",
            "nope",
        ],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("tool.odx.packages.odoo-addon-y.group: names no configured group"),
        "{err}"
    );
    assert!(
        !read(&fs, "/w/pyproject.toml").contains("odoo-addon-y"),
        "invalid edits are not written"
    );
}

#[test]
fn add_creates_tool_odx_in_a_plain_pyproject() {
    let fs = fs_with(&[("/w/pyproject.toml", "[project]\nname = \"x\"\n")]);
    let (exit, _, err) = run(&fs, &["add", "OCA/pos#1"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        format!(
            "error: {}: no [tool.odx] table; pass the Odoo series to create one\n",
            w("pyproject.toml")
        )
    );
    let (exit, _, _) = run(&fs, &["add", "OCA/pos#1", "--odoo-version", "18.0"]);
    assert_eq!(exit, Exit::Success);
    assert!(read(&fs, "/w/pyproject.toml").contains("[tool.odx]\nodoo_version = \"18.0\"\n"));
}

#[test]
fn add_errors() {
    let empty = MemoryFs::default();
    let (exit, _, err) = run(&empty, &["add", "OCA/pos#1"]);
    assert_eq!(
        (exit, err),
        (
            Exit::Usage,
            format!("error: {}: no such file\n", w("pyproject.toml"))
        )
    );
    let (exit, _, err) = run(&empty, &["add", "nonsense"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.starts_with("error: cannot parse \"nonsense\""), "{err}");

    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT), ("/w/bad.toml", "[x")]);
    let (exit, _, err) = run(&fs, &["--config", "bad.toml", "add", "OCA/pos#1"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.starts_with(&format!("error: {}: ", w("bad.toml"))),
        "{err}"
    );

    fs.fail_writes_to("/w/pyproject.toml", ErrorKind::ReadOnlyFilesystem);
    let (exit, _, err) = run(&fs, &["add", "OCA/pos#9"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.starts_with(&format!("error: {}: ", w("pyproject.toml"))),
        "{err}"
    );

    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT)]);
    fs.fail_next(ErrorKind::PermissionDenied);
    let (exit, _, _) = run(&fs, &["add", "OCA/pos#9"]);
    assert_eq!(exit, Exit::Failure);
    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT)]);
    fs.fail_on("/w/other.toml", ErrorKind::PermissionDenied);
    let (exit, _, _) = run(&fs, &["--config", "other.toml", "add", "OCA/pos#9"]);
    assert_eq!(exit, Exit::Failure);

    let invalid = fs_with(&[("/w/pyproject.toml", "[tool.odx]\nodoo_version = \"x\"\n")]);
    let (exit, _, err) = run(&invalid, &["add", "OCA/pos#9"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("invalid configuration"), "{err}");
    let malformed = fs_with(&[(
        "/w/pyproject.toml",
        "[tool.odx]\nodoo_version = \"17.0\"\nrepositories = 1\n",
    )]);
    let (exit, _, err) = run(
        &malformed,
        &["--config", "pyproject.toml", "add", "OCA/pos#9"],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("unexpected type"), "{err}");
}

#[test]
fn drop_merges_and_packages() {
    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT)]);
    let (exit, out, _) = run(&fs, &["drop", "OCA/pos#123", "--reason", "merged upstream"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!(
            "dropped pull request #123 of https://github.com/OCA/pos.git from {} (reason: merged upstream)\n",
            w("pyproject.toml")
        )
    );
    let (exit, _, err) = run(&fs, &["drop", "OCA/pos#123"]);
    assert_eq!(
        (exit, err.as_str()),
        (
            Exit::Failure,
            "error: no merge #123 on https://github.com/OCA/pos.git\n"
        )
    );
    let (exit, out, _) = run(&fs, &["drop", "--package", "odoo-addon-pos-vouchers"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.starts_with("dropped package odoo-addon-pos-vouchers from"),
        "{out}"
    );
    let (exit, _, err) = run(&fs, &["drop", "--package", "odoo-addon-pos-vouchers"]);
    assert_eq!(
        (exit, err.as_str()),
        (Exit::Failure, "error: no package odoo-addon-pos-vouchers\n")
    );
    let (exit, _, _) = run(&fs, &["drop", "x"]);
    assert_eq!(exit, Exit::Usage);
    let empty = MemoryFs::default();
    assert_eq!(run(&empty, &["drop", "OCA/pos#1"]).0, Exit::Usage);
}

#[test]
fn drop_package_without_tool_odx_uses_no_groups() {
    let plain = "[project]\nname = \"x\"\n\n[tool.uv.sources]\nodoo-addon-x = { git = \"https://x/x.git\" }\n";
    let fs = fs_with(&[("/w/pyproject.toml", plain)]);
    let (exit, _, _) = run(&fs, &["drop", "--package", "odoo-addon-x"]);
    assert_eq!(exit, Exit::Success);
    let without_groups = "[tool.odx]\nodoo_version = \"17.0\"\n[tool.odx.packages]\nx = { repo = \"OCA/x\", pr = 1 }\n";
    let fs = fs_with(&[("/w/pyproject.toml", without_groups)]);
    assert_eq!(run(&fs, &["drop", "--package", "x"]).0, Exit::Success);
}

#[test]
fn open_and_save_errors_in_drop_and_import() {
    let repos = "./crm:\n  remotes:\n    o: https://github.com/OCA/crm.git\n  merges: [o 17.0]\n";
    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/bad.toml", "[x"),
        ("/w/repos.yaml", repos),
    ]);
    assert_eq!(
        run(&fs, &["--config", "bad.toml", "drop", "OCA/pos#123"]).0,
        Exit::Usage
    );
    assert_eq!(
        run(&fs, &["--config", "bad.toml", "import", "--write"]).0,
        Exit::Usage
    );
    fs.fail_writes_to("/w/pyproject.toml", ErrorKind::ReadOnlyFilesystem);
    assert_eq!(run(&fs, &["drop", "OCA/pos#123"]).0, Exit::Failure);
    assert_eq!(run(&fs, &["import", "--write"]).0, Exit::Failure);
}

#[test]
fn import_write() {
    let repos = "./pos:\n  remotes:\n    o: https://github.com/OCA/pos.git\n  merges: [o 17.0]\n./crm:\n  remotes:\n    o: https://github.com/OCA/crm.git\n  merges: [o 17.0, o refs/pull/3/head]\n";
    let fs = fs_with(&[("/w/pyproject.toml", PYPROJECT), ("/w/repos.yaml", repos)]);
    let (exit, out, err) = run(&fs, &["import", "--write"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!("wrote 1 repositories to {}\n", w("pyproject.toml"))
    );
    assert_eq!(
        err,
        "warning: https://github.com/OCA/pos.git: already configured; skipped\n"
    );
    assert!(read(&fs, "/w/pyproject.toml").contains("name = \"crm\""));

    let plain = fs_with(&[
        ("/w/pyproject.toml", "[project]\nname = \"x\"\n"),
        ("/w/repos.yaml", repos),
    ]);
    assert_eq!(run(&plain, &["import", "--write"]).0, Exit::Success);
    assert!(read(&plain, "/w/pyproject.toml").contains("odoo_version = \"17.0\""));
    let explicit = fs_with(&[
        ("/w/pyproject.toml", "[project]\n"),
        ("/w/repos.yaml", repos),
    ]);
    assert_eq!(
        run(&explicit, &["import", "--write", "--odoo-version", "16.0"]).0,
        Exit::Success
    );
    assert!(read(&explicit, "/w/pyproject.toml").contains("odoo_version = \"16.0\""));

    let malformed = fs_with(&[
        ("/w/pyproject.toml", "tool = 1\n"),
        ("/w/repos.yaml", repos),
    ]);
    let (exit, _, err) = run(
        &malformed,
        &["--config", "pyproject.toml", "import", "--write"],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("[tool] has an unexpected type"), "{err}");
    let missing = fs_with(&[("/w/repos.yaml", repos)]);
    assert_eq!(run(&missing, &["import", "--write"]).0, Exit::Usage);
}

const SHA: &str = "a1b2c3d4e5f67890123456789012345678901234";

fn lockfile() -> String {
    format!(
        "version = 1\n\n[[repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nbase = \"17.0\"\nbase_sha = \"{SHA}\"\nresult_tree = \"{SHA}\"\n\n[[repositories.merges]]\nsource = {{ pr = 123 }}\nsha = \"{SHA}\"\ntree = \"{SHA}\"\npatch_id = \"{SHA}\"\nstatus = \"merged\"\n\n[packages.odoo-addon-pos-vouchers]\nurl = \"https://github.com/OCA/pos.git\"\nsource = {{ pr = 45 }}\nsha = \"{SHA}\"\n"
    )
}

#[test]
fn drop_also_cleans_the_lockfile() {
    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/odx.lock", &lockfile()),
    ]);
    let (exit, out, _) = run(&fs, &["drop", "OCA/pos#123"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!(
            "dropped pull request #123 of https://github.com/OCA/pos.git from {} and {}\n",
            w("pyproject.toml"),
            w("odx.lock")
        )
    );
    let lock = read(&fs, "/w/odx.lock");
    assert!(lock.starts_with("# Generated by odx. Do not edit.\n") && !lock.contains("pr = 123"));
    let (exit, out, _) = run(&fs, &["drop", "--package", "odoo-addon-pos-vouchers"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains(" and "), "{out}");
    assert!(!read(&fs, "/w/odx.lock").contains("pos-vouchers"));
}

#[test]
fn drop_leaves_an_unrelated_lockfile_alone() {
    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/odx.lock", &lockfile()),
    ]);
    run(&fs, &["add", "OCA/pos#200"]);
    let (exit, out, _) = run(&fs, &["drop", "OCA/pos#200"]);
    assert_eq!(exit, Exit::Success);
    assert!(!out.contains(" and "), "{out}");
    assert_eq!(read(&fs, "/w/odx.lock"), lockfile());
}

#[test]
fn drop_with_a_broken_lockfile_changes_nothing() {
    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/odx.lock", "version = 9\n"),
    ]);
    let (exit, _, err) = run(&fs, &["drop", "OCA/pos#123"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("odx.lock has version 9"), "{err}");
    assert_eq!(read(&fs, "/w/pyproject.toml"), PYPROJECT);

    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/odx.lock", &lockfile()),
    ]);
    fs.fail_on("/w/odx.lock", ErrorKind::PermissionDenied);
    assert_eq!(run(&fs, &["drop", "OCA/pos#123"]).0, Exit::Failure);

    let fs = fs_with(&[
        ("/w/pyproject.toml", PYPROJECT),
        ("/w/odx.lock", &lockfile()),
    ]);
    fs.fail_writes_to("/w/odx.lock", ErrorKind::ReadOnlyFilesystem);
    let (exit, _, err) = run(&fs, &["drop", "OCA/pos#123"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.starts_with(&format!("error: {}: ", w("odx.lock"))),
        "{err}"
    );
}

#[test]
fn drop_uses_the_configured_lockfile_and_every_target_kind() {
    let custom = PYPROJECT.replace(
        "odoo_version = \"17.0\"\n",
        "odoo_version = \"17.0\"\nlockfile = \"locks/odx.lock\"\n",
    );
    let lock = lockfile().replace("source = { pr = 123 }", "source = { sha = \"abcdef1\" }");
    let fs = fs_with(&[("/w/pyproject.toml", &custom), ("/w/locks/odx.lock", &lock)]);
    run(&fs, &["add", "OCA/pos@abcdef1"]);
    run(&fs, &["add", "OCA/pos@17.0-x"]);
    let (_, out, _) = run(&fs, &["drop", "OCA/pos@abcdef1"]);
    assert!(out.contains("locks"), "{out}");
    let (exit, out, _) = run(&fs, &["drop", "OCA/pos@17.0-x"]);
    assert_eq!(exit, Exit::Success);
    assert!(!out.contains(" and "), "{out}");
}
