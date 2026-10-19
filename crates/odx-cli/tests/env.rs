//! `odx env list`: the environment model with its preset applied.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::Path;
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
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

fn project(root: &Path, environments: &str) {
    std::fs::write(
        root.join("pyproject.toml"),
        format!("[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"19.0\"\n{environments}"),
    )
    .unwrap();
}

#[test]
fn presets_and_the_current_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(
        root,
        "[tool.odx.environments]\npreset = \"major-branches\"\nbranch_specific = [\".template-answers.yml\"]\n\
         [tool.odx.environments.env.prod]\nrepo = \"https://example.com/prod.git\"\n",
    );
    git(root, &["init", "-q", "-b", "19-test"]);
    let (exit, out, err) = odx(root, &["env", "list"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        "preset major-branches\n\
         \x20 prod  prod  19.0  19-prod  https://example.com/prod.git\n\
         * test  test  19.0  19-test\n\
         \x20 uat   uat   19.0  19-uat   (disposable)\n\
         promotion: test → uat, test → prod, uat → prod, prod → test\n\
         branch-specific: .template-answers.yml\n"
    );
    let (exit, out, _) = odx(root, &["--json", "env", "list"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["current"], "test");
    assert_eq!(
        value["environments"][0]["repo"],
        "https://example.com/prod.git"
    );
    assert_eq!(value["environments"][2]["disposable"], true);
    assert_eq!(value["promotion"][0], serde_json::json!(["test", "uat"]));
    // A branch of no environment, and explicit environments without edges
    // or roles.
    git(root, &["checkout", "-q", "-b", "feature"]);
    project(
        root,
        "[tool.odx.environments.env.staging]\nbranch = \"{series}-staging\"\n",
    );
    let (exit, out, _) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "preset explicit\n\
         \x20 staging  -  19.0  19.0-staging\n\
         promotion: none\n\
         the current branch feature is no environment's\n"
    );
}

#[test]
fn no_environments_and_invalid_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root, "");
    let (exit, out, err) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "error: no environments configured: add [tool.odx.environments] to pyproject.toml\n"
    );
    project(
        root,
        "[tool.odx.environments]\n[tool.odx.environments.env.qa]\nbranch = \"{x}\"\n",
    );
    let (exit, _, err) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("tool.odx.environments.env.qa.branch: unknown placeholder {x}"),
        "{err}"
    );
    // Not a git checkout: no current branch to mark.
    project(
        root,
        "[tool.odx.environments]\npreset = \"main-production\"\n",
    );
    let (exit, out, _) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        !out.contains('*') && !out.contains("current branch"),
        "{out}"
    );
}
