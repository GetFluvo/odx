//! The `custom` platform profile's prepare and verify hooks.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

struct Project {
    tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    fn new(prepare: &str, verify: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.platform]\nprofile = \"custom\"\n\n\
                 [tool.odx.platform.custom]\nprepare = {prepare:?}\nverify = {verify:?}\n"
            ),
        );
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
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
}

#[test]
fn hooks_run_only_when_allowed() {
    let project = Project::new("exit 0", "exit 0");
    let (exit, out, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    assert!(!out.contains("ran the custom prepare hook"), "{out}");
    assert!(
        err.contains("the custom prepare hook `exit 0` was not run; pass --allow-shell"),
        "{err}"
    );
    let (exit, _, err) = project.odx(&["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("the custom verify hook `exit 0` was not run"),
        "{err}"
    );

    let (exit, out, _) = project.odx(&["vendor", "--requirements", "--allow-shell"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.ends_with("ran the custom prepare hook\n"), "{out}");
    let (exit, _, err) = project.odx(&["verify", "--allow-shell"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));

    // Trusted in the user configuration.
    let config = project.tmp.path().join("config");
    write(
        &config.join("config.toml"),
        &format!(
            "[trust]\nprojects = [\"~/nowhere\", {:?}]\n",
            project.root.display().to_string()
        ),
    );
    let (exit, _, err) = project.odx(&["verify"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
}

#[test]
fn failing_hooks_fail() {
    let project = Project::new("exit 3", "exit 4");
    let (exit, _, err) = project.odx(&["vendor", "--requirements", "--allow-shell"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("the custom prepare hook `exit 3` failed"),
        "{err}"
    );
    let (exit, _, err) = project.odx(&["verify", "--allow-shell"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("the custom verify hook `exit 4` failed"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn hooks_see_the_project() {
    let project = Project::new(
        "echo \"$ODX_SERIES $ODX_ENVIRONMENT|$(basename \"$ODX_LOCKFILE\") $(basename \"$ODX_REQUIREMENTS\") $(basename \"$ODX_VENDOR_MANIFEST\")\" > hook.txt",
        "test -f hook.txt",
    );
    let (exit, _, err) = project.odx(&["vendor", "--requirements", "--allow-shell"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        std::fs::read_to_string(project.root.join("hook.txt")).unwrap(),
        "17.0 |odx.lock requirements.txt vendor.lock\n"
    );
    let (exit, _, _) = project.odx(&["verify", "--allow-shell"]);
    assert_eq!(exit, Exit::Success);
}
