//! `odx lock --auto-update` against a package index on disk.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;
use sha2::Digest as _;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn manifest(version: &str) -> String {
    format!("{{\"name\": \"X\", \"version\": \"{version}\"}}")
}

/// A wheel of `addon` at `version` with `files`; returns its URL and sha256.
fn wheel(dir: &Path, addon: &str, version: &str, files: &[(&str, &str)]) -> (String, String) {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer
        .start_file(format!("odoo/addons/{addon}/__manifest__.py"), options)
        .unwrap();
    writer.write_all(manifest(version).as_bytes()).unwrap();
    for (name, content) in files {
        writer
            .start_file(format!("odoo/addons/{addon}/{name}"), options)
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    let path = dir.join(format!("odoo_addon_{addon}-{version}-py3-none-any.whl"));
    write(&path, "");
    std::fs::write(&path, &bytes).unwrap();
    let hex = sha2::Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    (file_url(&path), hex)
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    /// Locked at 17.0.1.0.0: `a` (the index has a translation-only
    /// release), `b` (a code change), `c` (denied), `d` (only a new minor
    /// version), `e` (nothing newer), `f` (unknown to the index).
    fn new(settings: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let wheels = tmp.path().join("wheels");
        let index = tmp.path().join("index");
        let base = [("i18n/de.po", "msgid a"), ("models/x.py", "x = 1")];
        let mut lock = String::from("version = 1\n");
        for addon in ["a", "b", "c", "d", "e", "f"] {
            let (url, hash) = wheel(&wheels, addon, "17.0.1.0.0", &base);
            let _ = write!(
                lock,
                "\n[[package]]\nname = \"odoo-addon-{addon}\"\nversion = \"17.0.1.0.0\"\n\
                 source = {{ registry = \"https://pypi.org/simple\" }}\n\
                 wheels = [{{ url = \"{url}\", hash = \"sha256:{hash}\" }}]\n"
            );
        }
        lock.push_str("\n[[package]]\nname = \"requests\"\nversion = \"2.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n");
        write(&root.join("uv.lock"), &lock);
        let release = |addon: &str, version: &str, files: &[(&str, &str)]| {
            let (url, hash) = wheel(&wheels, addon, version, files);
            format!(
                "\"{version}\": [{{\"filename\": \"odoo_addon_{addon}-{version}-py3-none-any.whl\", \"url\": \"{url}\", \"digests\": {{\"sha256\": \"{hash}\"}}}}]"
            )
        };
        let releases = [
            (
                "a",
                vec![release(
                    "a",
                    "17.0.1.0.1",
                    &[("i18n/de.po", "msgid b"), ("models/x.py", "x = 1")],
                )],
            ),
            (
                "b",
                vec![release(
                    "b",
                    "17.0.1.0.1",
                    &[("i18n/de.po", "msgid a"), ("models/x.py", "x = 2")],
                )],
            ),
            ("c", vec![release("c", "17.0.1.0.1", &base)]),
            ("d", vec![release("d", "17.0.2.0.0", &base)]),
            ("e", vec![release("e", "17.0.1.0.0", &base)]),
        ];
        for (addon, versions) in releases {
            write(
                &index.join(format!("odoo-addon-{addon}/json")),
                &format!("{{\"releases\": {{{}}}}}", versions.join(", ")),
            );
        }
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.lock.auto_update]\ndeny = [\"odoo-addon-c\"]\n{settings}"
            ),
        );
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with("ODX_PYPI_URL", file_url(&index))
            .with("ODX_UV", "git");
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

#[test]
fn translations_update_and_code_waits_for_review() {
    let project = Project::new("");
    let (exit, out, err) = project.odx(&["lock", "--auto-update"]);
    // `git lock` stands in for a failing uv.
    assert_eq!(exit, Exit::Failure);
    assert!(err.starts_with("error: git lock failed: "), "{err}");
    assert!(
        out.starts_with("auto-updated odoo-addon-a 17.0.1.0.0 -> 17.0.1.0.1 (i18n/de.po)\n"),
        "{out}"
    );
    assert!(
        out.contains("needs review odoo-addon-b 17.0.1.0.0 -> 17.0.1.0.1: changes files outside allowed_paths: models/x.py\n"),
        "{out}"
    );
    assert!(
        out.contains("needs review odoo-addon-d 17.0.1.0.0 -> 17.0.2.0.0: changes more than the last version segment\n"),
        "{out}"
    );
    assert!(
        out.contains("needs review odoo-addon-f 17.0.1.0.0: "),
        "{out}"
    );
    assert!(
        !out.contains("odoo-addon-c") && !out.contains("odoo-addon-e"),
        "{out}"
    );

    let (_, out, _) = project.odx(&["--json", "lock", "--auto-update"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["updated"][0]["package"], "odoo-addon-a");
    assert_eq!(value["review"].as_array().unwrap().len(), 3);
}

#[test]
fn settings() {
    let off = Project::new("policy = \"none\"\n");
    let (exit, out, _) = off.odx(&["lock", "--auto-update"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("auto-update is off"), "{out}");

    // Without the content check, last-segment updates are taken as is;
    // allow narrows the packages looked at.
    let mut unchecked = Project::new("content_check = false\nallow = [\"odoo-addon-[bd]\"]\n");
    unchecked.env = unchecked
        .env
        .clone()
        .with("ODX_UV", "odx-surely-missing-uv");
    let (_, out, err) = unchecked.odx(&["lock", "--auto-update"]);
    assert!(
        out.starts_with("auto-updated odoo-addon-b 17.0.1.0.0 -> 17.0.1.0.1\n"),
        "{out}"
    );
    assert!(err.contains("odx-surely-missing-uv lock failed"), "{err}");
    assert!(!out.contains("odoo-addon-a"), "{out}");

    // Everything allowed: a code change passes.
    let open = Project::new("allowed_paths = [\"*\"]\nallow = [\"odoo-addon-b\"]\n");
    let (_, out, _) = open.odx(&["lock", "--auto-update"]);
    assert!(
        out.starts_with("auto-updated odoo-addon-b 17.0.1.0.0 -> 17.0.1.0.1 (models/x.py)\n"),
        "{out}"
    );

    let nothing = Project::new("allow = [\"odoo-addon-e\"]\n");
    let (exit, out, _) = nothing.odx(&["lock", "--auto-update"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, "no auto-updates\n"));

    std::fs::remove_file(nothing.root.join("uv.lock")).unwrap();
    let (exit, _, err) = nothing.odx(&["lock", "--auto-update"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("uv.lock is missing or unreadable"), "{err}");
}

#[test]
fn broken_index_answers() {
    let project = Project::new("allow = [\"odoo-addon-a\"]\n");
    let index = project
        .root
        .parent()
        .unwrap()
        .join("index/odoo-addon-a/json");
    write(&index, "{}");
    let (_, out, _) = project.odx(&["lock", "--auto-update"]);
    assert!(out.contains("no releases in the index answer"), "{out}");
    write(
        &index,
        "{\"releases\": {\"17.0.1.0.1\": [{\"filename\": \"a.whl\", \"url\": \"file:///nowhere/a.whl\", \"digests\": {\"sha256\": \"00\"}}]}}",
    );
    let (_, out, _) = project.odx(&["lock", "--auto-update"]);
    assert!(
        out.contains("needs review odoo-addon-a 17.0.1.0.0: "),
        "{out}"
    );
    assert!(out.contains("nowhere"), "{out}");
    // A release whose wheel, under its own hash, is no zip archive.
    let broken = project.root.parent().unwrap().join("broken.whl");
    std::fs::write(&broken, "no zip").unwrap();
    let hex = sha2::Sha256::digest(b"no zip")
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    write(
        &index,
        &format!(
            "{{\"releases\": {{\"17.0.1.0.1\": [{{\"filename\": \"a.whl\", \"url\": \"{}\", \"digests\": {{\"sha256\": \"{hex}\"}}}}]}}}}",
            file_url(&broken)
        ),
    );
    let (_, out, _) = project.odx(&["lock", "--auto-update"]);
    assert!(out.contains("broken.whl: "), "{out}");
}

/// A stand-in for uv that records its arguments.
#[cfg(unix)]
#[test]
fn eligible_updates_go_through_uv() {
    use std::os::unix::fs::PermissionsExt;

    let mut project = Project::new("allow = [\"odoo-addon-a\"]\n");
    let uv = project.root.join("fake-uv");
    let args = project.root.join("uv-args");
    write(
        &uv,
        &format!("#!/bin/sh\necho \"$@\" > '{}'\n", args.display()),
    );
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    project.env = project.env.clone().with("ODX_UV", uv.display().to_string());
    let (exit, _, err) = project.odx(&["lock", "--auto-update"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        std::fs::read_to_string(&args).unwrap(),
        "lock --upgrade-package odoo-addon-a==17.0.1.0.1\n"
    );

    // An exact pin in pyproject.toml moves along, so uv may take it.
    let pyproject = project.root.join("pyproject.toml");
    let text = std::fs::read_to_string(&pyproject).unwrap();
    let pinned = format!(
        "[project]\nname = \"p\"\n\n[project.optional-dependencies]\noca = [\"odoo-addon-a==17.0.1.0.0\"]\n\n[dependency-groups]\ndev = [\"odoo-addon-a==17.0.1.0.0\"]\n\n{text}"
    );
    write(&pyproject, &pinned);
    let (exit, _, err) = project.odx(&["lock", "--auto-update"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert!(
        std::fs::read_to_string(&pyproject)
            .unwrap()
            .contains("oca = [\"odoo-addon-a==17.0.1.0.1\"]")
    );

    // A pyproject.toml that cannot be written stops before uv runs.
    write(&pyproject, &pinned);
    std::fs::remove_file(&args).unwrap();
    std::fs::set_permissions(&pyproject, std::fs::Permissions::from_mode(0o444)).unwrap();
    let (exit, _, err) = project.odx(&["lock", "--auto-update"]);
    std::fs::set_permissions(&pyproject, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pyproject.toml"), "{err}");
    assert!(!args.exists());
}

#[test]
fn locked_wheels_that_cannot_be_compared() {
    let project = Project::new("allow = [\"odoo-addon-a\"]\n");
    let wheel_dir = project.root.parent().unwrap().join("wheels");
    let locked = wheel_dir.join("odoo_addon_a-17.0.1.0.0-py3-none-any.whl");
    // The locked wheel is gone.
    let bytes = std::fs::read(&locked).unwrap();
    std::fs::remove_file(&locked).unwrap();
    let (_, out, _) = project.odx(&["lock", "--auto-update"]);
    assert!(
        out.contains("needs review odoo-addon-a 17.0.1.0.0: "),
        "{out}"
    );
    // The locked "wheel" is no zip archive, under its own hash.
    std::fs::write(&locked, "no zip").unwrap();
    let old_hex = sha2::Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let new_hex = sha2::Sha256::digest(b"no zip")
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let lock = std::fs::read_to_string(project.root.join("uv.lock")).unwrap();
    write(
        &project.root.join("uv.lock"),
        &lock.replace(&old_hex, &new_hex),
    );
    let (_, out, _) = project.odx(&["lock", "--auto-update"]);
    assert!(
        out.contains("odoo_addon_a-17.0.1.0.0-py3-none-any.whl: "),
        "{out}"
    );
}

#[test]
fn outside_a_project_and_the_default_index() {
    let project = Project::new("allow = [\"nothing-matches\"]\n");
    let nowhere = project.root.parent().unwrap().join("wheels");
    let (exit, _, _) = project.odx(&["-C", nowhere.to_str().unwrap(), "lock", "--auto-update"]);
    assert_eq!(exit, Exit::Usage);
    // Without ODX_PYPI_URL the public index is meant; nothing is asked.
    let env = MapEnv::default().with(
        "ODX_CACHE_DIR",
        project
            .root
            .parent()
            .unwrap()
            .join("cache")
            .display()
            .to_string(),
    );
    let quiet = Project { env, ..project };
    let (exit, out, _) = quiet.odx(&["lock", "--auto-update"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, "no auto-updates\n"));
}
