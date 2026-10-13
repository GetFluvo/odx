//! `odx build` with a stand-in for uv.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

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

/// A wheel as a backend would build it for `addon` (no metadata when
/// `broken`).
fn built_wheel(path: &Path, addon: &str, broken: bool) {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    writer
        .start_file(format!("odoo/addons/{addon}/__manifest__.py"), options)
        .unwrap();
    writer.write_all(b"{}").unwrap();
    if !broken {
        let dist_info = format!("odoo_addon_{addon}-17.0.1.0.0.dist-info");
        writer
            .start_file(format!("{dist_info}/METADATA"), options)
            .unwrap();
        writer
            .write_all(format!("Name: odoo-addon-{addon}\nVersion: 17.0.1.0.0\n").as_bytes())
            .unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    write(path, "");
    std::fs::write(path, bytes).unwrap();
}

struct Project {
    tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
    log: PathBuf,
}

impl Project {
    /// Upstream `pos` with addons `pos_x` (with its own pyproject.toml),
    /// `pos_z` (whose wheel comes out broken) and `notes` (no manifest);
    /// pull request 1 adds `pos_y`, locked as a package. The project has
    /// custom addons `c` and `d` (whose build makes no wheel).
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        write(&up.join("pos_x/__manifest__.py"), "{}");
        write(&up.join("pos_x/pyproject.toml"), "[build-system]\n");
        write(&up.join("pos_z/__manifest__.py"), "{}");
        write(&up.join("notes/readme.txt"), "");
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        git(&up, &["checkout", "--quiet", "-b", "pr"]);
        write(&up.join("pos_y/__manifest__.py"), "{}");
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "pr"]);
        let pr = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/1/head", &pr]);
        git(&up, &["checkout", "--quiet", "17.0"]);

        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        let url = file_url(&up);
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\nmerge_strategy = \"merge\"\n\n\
                 [tool.odx.groups]\nproject_addons = [\"modules\"]\n\n\
                 [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{url}\"\naddons = [\"pos_x\", \"pos_z\", \"notes\"]\nmerges = [{{ pr = 1 }}]\n\n\
                 [[tool.uv.index]]\nname = \"private\"\nurl = \"https://index.example/simple\"\npublish-url = \"https://index.example/upload\"\n\n\
                 [[tool.uv.index]]\nname = \"pypi\"\nurl = \"https://pypi.org/simple\"\n\n\
                 [[tool.uv.index]]\nname = \"broken\"\nurl = \"https://broken.example/simple\"\n"
            ),
        );
        write(&root.join("modules/c/__manifest__.py"), "{}");
        write(&root.join("modules/d/__manifest__.py"), "{}");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "project"]);

        let wheels = tmp.path().join("wheels");
        for addon in ["pos_x", "pos_y", "c"] {
            built_wheel(&wheels.join(format!("{addon}.whl")), addon, false);
        }
        built_wheel(&wheels.join("pos_z.whl"), "pos_z", true);
        let log = tmp.path().join("uv.log");
        let env = MapEnv::default().with(
            "ODX_CACHE_DIR",
            tmp.path().join("cache").display().to_string(),
        );
        let mut project = Self {
            tmp,
            root,
            env,
            log,
        };
        let (exit, out, err) = project.odx(&["lock"]);
        assert_eq!(exit, Exit::Success, "{out}{err}");
        let lock = std::fs::read_to_string(project.root.join("odx.lock")).unwrap();
        write(
            &project.root.join("odx.lock"),
            &format!(
                "{lock}\n[packages.odoo-addon-pos-y]\nurl = \"{url}\"\nsource = {{ pr = 1 }}\nsubdirectory = \"pos_y\"\nsha = \"{pr}\"\n"
            ),
        );
        project.fake_uv(&wheels);
        project
    }

    #[cfg(unix)]
    fn fake_uv(&mut self, wheels: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let uv = self.tmp.path().join("fake-uv");
        write(
            &uv,
            &format!(
                "#!/bin/sh\necho \"$@\" >> '{}'\n\
                 if [ \"$1\" = build ]; then mkdir -p \"$5\"; cp '{}'/\"$(basename \"$3\")\".whl \"$5\"/ 2>/dev/null || true; fi\n\
                 if [ \"$1\" = publish ] && [ \"$3\" = broken ]; then echo 'upload refused' >&2; exit 1; fi\n",
                self.log.display(),
                wheels.display()
            ),
        );
        std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
        self.env = self.env.clone().with("ODX_UV", uv.display().to_string());
    }

    #[cfg(not(unix))]
    fn fake_uv(&mut self, _: &Path) {
        self.env = self.env.clone().with("ODX_UV", "git");
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

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.root.join(file)).unwrap()
    }
}

#[cfg(unix)]
#[test]
fn wheels_get_content_derived_versions() {
    let project = Project::new();
    let (exit, out, err) = project.odx(&["build", "--addon", "pos_[xy]"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("built odoo-addon-pos-x 17.0.1.0.0+odx."),
        "{out}"
    );
    assert!(
        lines[0].contains(" from repository pos into dist/wheels/odoo_addon_pos_x-17.0.1.0.0+odx."),
        "{out}"
    );
    assert!(
        lines[1].starts_with("built odoo-addon-pos-y 17.0.1.0.0+odx.pr1."),
        "{out}"
    );
    assert!(
        lines[1].contains(" from package odoo-addon-pos-y "),
        "{out}"
    );
    let lock = project.read("odx.lock");
    assert!(
        lock.contains("[wheels.odoo-addon-pos-x]\nversion = \"17.0.1.0.0+odx."),
        "{lock}"
    );
    // The backend got WHOOL's settings and an addon without pyproject.toml
    // got whool as its backend.
    let log = std::fs::read_to_string(&project.log).unwrap();
    assert!(log.contains("build --wheel "), "{log}");

    // Again: the same versions and hashes.
    let (_, again, _) = project.odx(&["build", "--addon", "pos_[xy]"]);
    assert_eq!(again, out);
    assert_eq!(project.read("odx.lock"), lock);

    // Publishing records the index.
    let (exit, out, err) = project.odx(&["build", "--addon", "pos_x", "--publish"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(out.ends_with("published 1 wheels to private\n"), "{out}");
    assert!(project.read("odx.lock").contains("index = \"private\""));
    let log = std::fs::read_to_string(&project.log).unwrap();
    assert!(log.contains("publish --index private "), "{log}");
    // PyPI refuses local versions; unknown and failing indexes fail.
    for (index, expected) in [
        (
            "pypi",
            "pypi is PyPI, which refuses the +odx local versions",
        ),
        ("nope", "no [[tool.uv.index]] named \"nope\""),
        ("broken", "publish failed: upload refused"),
    ] {
        let (exit, _, err) =
            project.odx(&["build", "--addon", "pos_x", "--publish", "--index", index]);
        assert_eq!(exit, Exit::Failure, "{index}");
        assert!(err.contains(expected), "{index}: {err}");
    }
}

#[cfg(unix)]
#[test]
#[expect(clippy::too_many_lines, reason = "one project through every failure")]
fn build_failures() {
    let project = Project::new();
    // The same addon from the aggregated pull request and as a package.
    let pyproject = project.read("pyproject.toml");
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace("\"pos_x\", \"pos_z\"", "\"pos_*\""),
    );
    let (exit, out, err) = project.odx(&["build", "--addon", "pos_y"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.contains("odoo-addon-pos-y 17.0.1.0.0+odx.") && out.contains("from repository pos"),
        "{out}"
    );
    assert!(
        err.contains(
            "pos_y: comes from repository pos and from package odoo-addon-pos-y; keep one"
        ),
        "{err}"
    );
    write(&project.root.join("pyproject.toml"), &pyproject);
    // pos_z comes out without metadata; d makes no wheel.
    let (exit, out, err) = project.odx(&[
        "build",
        "--project",
        "--addon",
        "pos_z",
        "--addon",
        "c",
        "--addon",
        "d",
    ]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("built odoo-addon-c 17.0.1.0.0+odx."), "{out}");
    assert!(out.contains("from project into"), "{out}");
    assert!(
        err.contains("pos_z: the wheel has no .dist-info/METADATA"),
        "{err}"
    );
    assert!(err.contains("made no wheel"), "{err}");

    // A package that is a whole repository, and one whose subdirectory is
    // a file.
    let lock = project.read("odx.lock");
    let base = git(&project.tmp.path().join("up"), &["rev-parse", "17.0"]);
    let url = file_url(&project.tmp.path().join("up"));
    write(
        &project.root.join("odx.lock"),
        &format!(
            "{lock}\n[packages.odoo-addon-whole]\nurl = \"{url}\"\nsource = {{ branch = \"17.0\" }}\nsha = \"{base}\"\n\n\
             [packages.odoo-addon-file]\nurl = \"{url}\"\nsource = {{ branch = \"17.0\" }}\nsubdirectory = \"pos_x/__manifest__.py\"\nsha = \"{base}\"\n"
        ),
    );
    let (_, _, err) = project.odx(&["build", "--addon", "whole", "--addon", "__manifest__.py"]);
    assert!(
        err.contains("whole: ") && err.contains("made no wheel"),
        "{err}"
    );
    assert!(err.contains("__manifest__.py: "), "{err}");
    write(&project.root.join("odx.lock"), &lock);

    // Scratch space blocked: nothing can be exported.
    let scratch = project.tmp.path().join("cache/build");
    let _ = std::fs::remove_dir_all(&scratch);
    write(&scratch, "a file");
    let (_, _, err) = project.odx(&["build", "--addon", "pos_x"]);
    assert!(err.contains("pos_x: "), "{err}");
    std::fs::remove_file(&scratch).unwrap();

    // The index from [tool.odx.platform], here one that refuses.
    let pyproject = project.read("pyproject.toml");
    write(
        &project.root.join("pyproject.toml"),
        &format!("{pyproject}\n[tool.odx.platform]\nindex = \"broken\"\n"),
    );
    let (_, _, err) = project.odx(&["build", "--addon", "pos_x", "--publish"]);
    assert!(err.contains("publish failed: upload refused"), "{err}");
    write(&project.root.join("pyproject.toml"), &pyproject);

    // odx.lock cannot be written.
    {
        use std::os::unix::fs::PermissionsExt;
        let lock = project.root.join("odx.lock");
        let before = project.read("odx.lock");
        let cleaned: String = before.split("\n[wheels.").next().unwrap().to_owned();
        write(&lock, &format!("{cleaned}\n"));
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o444)).unwrap();
        let (exit, _, err) = project.odx(&["build", "--addon", "pos_x"]);
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(exit, Exit::Failure);
        assert!(err.contains("odx.lock"), "{err}");
    }

    // Output directory blocked.
    write(&project.root.join("blocked"), "");
    let (exit, _, err) = project.odx(&["build", "--addon", "pos_x", "--out-dir", "blocked"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("blocked"), "{err}");

    // An uncommitted project addon, and one whose content git lost.
    write(&project.root.join("modules/e/__manifest__.py"), "{}");
    let (_, _, err) = project.odx(&["build", "--project", "--addon", "e"]);
    assert!(
        err.contains("modules/e: not committed; commit it first"),
        "{err}"
    );
    let blob = git(
        &project.root,
        &["rev-parse", "HEAD:modules/c/__manifest__.py"],
    );
    std::fs::remove_file(
        project
            .root
            .join(".git/objects")
            .join(&blob[..2])
            .join(&blob[2..]),
    )
    .unwrap();
    let (_, _, err) = project.odx(&["build", "--project", "--addon", "c"]);
    assert!(err.contains("c: "), "{err}");
}

#[test]
fn build_errors_without_uv() {
    let mut project = Project::new();
    project.env = project.env.clone().with("ODX_UV", "git");
    let (exit, _, err) = project.odx(&["build", "--addon", "pos_x"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pos_x: git build failed: "), "{err}");
    project.env = project.env.clone().with("ODX_UV", "odx-surely-missing-uv");
    let (_, _, err) = project.odx(&["build", "--addon", "pos_x"]);
    assert!(err.contains("odx-surely-missing-uv build failed"), "{err}");

    let (exit, _, err) = project.odx(&["build", "--sbom"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("--sbom: not implemented yet"), "{err}");
    let (exit, out, _) = project.odx(&["build", "--addon", "nothing"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, "nothing to build\n"));

    // Without the cache, nothing can be exported.
    std::fs::remove_dir_all(project.tmp.path().join("cache")).unwrap();
    let (exit, _, err) = project.odx(&["build"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("pos: the locked result is not in the cache"),
        "{err}"
    );
    assert!(err.contains("odoo-addon-pos-y: commit "), "{err}");

    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    let (exit, _, err) = project.odx(&["build"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock is missing; run `odx lock`"), "{err}");
    let (exit, _, _) = project.odx(&["-C", "/", "build"]);
    assert_eq!(exit, Exit::Usage);
}
