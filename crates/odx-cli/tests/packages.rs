//! `odx lock` for git packages and the Odoo core source.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

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
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    up: PathBuf,
    url: String,
    vault: PathBuf,
    env: MapEnv,
}

impl Project {
    /// An upstream with addons `pos_x` and `other` on 17.0 (tagged `v1`)
    /// and pull request 1 changing `pos_x`.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let up = tmp.path().join("up");
        for dir in [&root, &up.join("pos_x"), &up.join("other")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        std::fs::write(up.join("pos_x").join("x.py"), "x").unwrap();
        std::fs::write(up.join("other").join("o.py"), "o").unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        git(&up, &["tag", "v1"]);
        let vault = tmp.path().join("vault.git");
        git(
            tmp.path(),
            &["init", "--quiet", "--bare", vault.to_str().unwrap()],
        );
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with("ODX_UV", if cfg!(windows) { "cmd" } else { "true" });
        let project = Self {
            url: file_url(&up),
            root,
            up,
            vault,
            env,
            _tmp: tmp,
        };
        project.push_pr("pr1");
        project
    }

    fn push_pr(&self, content: &str) -> String {
        git(&self.up, &["checkout", "--quiet", "--detach", "17.0"]);
        std::fs::write(self.up.join("pos_x").join("x.py"), content).unwrap();
        git(&self.up, &["commit", "--quiet", "-am", content]);
        let head = git(&self.up, &["rev-parse", "HEAD"]);
        git(&self.up, &["update-ref", "refs/pull/1/head", &head]);
        git(&self.up, &["checkout", "--quiet", "17.0"]);
        head
    }

    fn write(&self, mirror: bool, odoo: &str) {
        let url = &self.url;
        let base = git(&self.up, &["rev-parse", "17.0"]);
        let mirror = if mirror {
            format!(
                "[tool.odx.mirror]\nurl = \"{}\"\nref_style = \"tags\"\n",
                file_url(&self.vault)
            )
        } else {
            String::new()
        };
        let text = format!(
            r#"[project]
name = "client"
version = "1"

[project.optional-dependencies]
oca = [
  "odoo-addon-pos_x @ git+{url}@refs/pull/1/head#subdirectory=pos_x",
  "odoo-addon-norev @ git+{url}#subdirectory=other",
  "odoo-addon-branchy @ git+{url}@17.0#subdirectory=other",
  "odoo-addon-fixed @ git+{url}@{base}#subdirectory=other",
]

[tool.uv.sources]
{odoo}

[tool.odx]
odoo_version = "17.0"
{mirror}
[tool.odx.groups]
oca = "modules/oca"

[tool.odx.packages]
odoo-addon-other = {{ url = "{url}", tag = "v1", subdirectory = "other", group = "oca" }}
"#
        );
        std::fs::write(self.root.join("pyproject.toml"), text).unwrap();
    }

    fn run(&self, args: &[&str]) -> (Exit, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
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

#[test]
fn packages_and_the_core_are_locked() {
    let project = Project::new();
    let base = git(&project.up, &["rev-parse", "17.0"]);
    let pr = git(&project.up, &["rev-parse", "refs/pull/1/head"]);
    project.write(
        false,
        &format!("odoo = {{ git = \"{}\", branch = \"17.0\" }}", project.url),
    );
    let before = project.read("pyproject.toml");
    let (exit, out, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        err.contains("odoo-addon-norev: a git direct reference needs a branch"),
        "{err}"
    );
    assert!(out.contains("locked odoo-addon-pos_x package at"), "{out}");
    assert!(out.contains("locked odoo core at"), "{out}");
    let lock = project.read("odx.lock");
    assert!(lock.contains("[packages.odoo-addon-pos_x]"), "{lock}");
    assert!(lock.contains(&format!("sha = \"{pr}\"")), "{lock}");
    assert!(lock.contains(&format!("rev = \"{base}\"")), "{lock}");
    assert!(lock.contains("source = { tag = \"v1\" }"), "{lock}");
    assert_eq!(
        project.read("pyproject.toml"),
        before,
        "nothing to point at without a vault"
    );

    // Pinned until --update.
    let moved = project.push_pr("pr2");
    let (_, out, _) = project.run(&["lock"]);
    assert_eq!(out, "");
    let (_, out, _) = project.run(&["lock", "--update"]);
    assert!(out.contains("updated odoo-addon-pos_x package"), "{out}");
    assert!(project.read("odx.lock").contains(&moved));
}

#[test]
fn vaulted_packages_point_uv_at_the_vault() {
    let project = Project::new();
    project.write(true, "");
    let (exit, out, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("vaulted odoo-addon-pos_x: 1 commits"), "{out}");
    assert!(
        out.contains("pointed packages at the vault in pyproject.toml"),
        "{out}"
    );
    let pr = git(&project.up, &["rev-parse", "refs/pull/1/head"]);
    let vault = file_url(&project.vault);
    let text = project.read("pyproject.toml");
    assert!(
        text.contains(&format!(
            "\"odoo-addon-pos_x @ git+{vault}@odx/{pr}#subdirectory=pos_x\""
        )),
        "{text}"
    );
    assert!(text.contains("odoo-addon-other = { git = "), "{text}");
    assert!(text.contains("  \"odoo-addon-other\""), "{text}");
    assert_eq!(
        git(
            &project.vault,
            &["rev-parse", &format!("refs/tags/odx/{pr}")]
        ),
        pr
    );

    // The rewritten reference still stands for pull request 1.
    let (exit, out, _) = project.run(&["lock"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, ""));
    let lock = project.read("odx.lock");
    assert!(lock.contains("source = { pr = 1 }"), "{lock}");
    assert!(lock.contains("source = { branch = \"17.0\" }"), "{lock}");
    let moved = project.push_pr("pr2");
    let (_, out, _) = project.run(&["lock", "--update", "--no-uv"]);
    assert!(out.contains("updated odoo-addon-pos_x package"), "{out}");
    assert!(
        project
            .read("pyproject.toml")
            .contains(&format!("@odx/{moved}#"))
    );
}

#[test]
fn uv_is_found_by_its_name() {
    let mut project = Project::new();
    project.write(true, "");
    project.env = MapEnv::default().with(
        "ODX_CACHE_DIR",
        project
            .root
            .parent()
            .unwrap()
            .join("cache")
            .display()
            .to_string(),
    );
    // Whether uv is installed decides the outcome; either way it is asked.
    let (exit, _, err) = project.run(&["lock"]);
    assert!(
        exit == Exit::Success || err.contains("uv lock failed"),
        "{err}"
    );
}

#[test]
fn vaulting_and_rewriting_failures() {
    let project = Project::new();
    project.write(true, "");
    let text = project
        .read("pyproject.toml")
        .replace("[tool.uv.sources]\n", "[tool.uv]\nsources = \"none\"\n");
    std::fs::write(project.root.join("pyproject.toml"), &text).unwrap();
    let (exit, _, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("odoo-addon-other: cannot point it at the vault"),
        "{err}"
    );

    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    std::fs::write(
        project.root.join("pyproject.toml"),
        text.replace(
            &file_url(&project.vault),
            "file:///nonexistent/odx/vault.git",
        ),
    )
    .unwrap();
    let (exit, _, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odoo-addon-pos_x: vaulting failed"), "{err}");

    // A pyproject.toml that cannot be written.
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    project.write(true, "");
    let file = project.root.join("pyproject.toml");
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions).unwrap();
    let (exit, _, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("pyproject.toml"), "{err}");
}

#[test]
fn package_and_core_failures() {
    let mut project = Project::new();
    project.write(true, "");
    project.env = project.env.clone().with("ODX_UV", "odx-surely-missing-uv");
    let (exit, _, err) = project.run(&["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx-surely-missing-uv lock failed"), "{err}");
    project.env = project.env.clone().with("ODX_UV", "git");
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    project.write(true, "");
    let (_, _, err) = project.run(&["lock"]);
    assert!(err.contains("git lock failed"), "{err}");
    let (_, _, err) = project.run(&["lock", "--update", "--no-uv"]);
    assert!(!err.contains("lock failed"), "{err}");

    for (odoo, expected) in [
        (
            format!("odoo = {{ git = \"{}\", branch = \"16.0\" }}", project.url),
            "has no 16.0",
        ),
        (
            "odoo = { git = \"file:///nonexistent/odx/ocb\", tag = \"x\" }".to_owned(),
            "odoo: git ls-remote failed",
        ),
    ] {
        let _ = std::fs::remove_file(project.root.join("odx.lock"));
        project.write(false, &odoo);
        let (exit, _, err) = project.run(&["lock"]);
        assert_eq!(exit, Exit::Failure, "{odoo}");
        assert!(err.contains(expected), "{err}");
    }
    let base = git(&project.up, &["rev-parse", "17.0"]);
    for odoo in [
        format!("odoo = {{ git = \"{}\", rev = \"{base}\" }}", project.url),
        format!("odoo = {{ git = \"{}\", tag = \"v1\" }}", project.url),
        format!("odoo = {{ git = \"{}\" }}", project.url),
    ] {
        project.write(false, &odoo);
        let (exit, _, err) = project.run(&["lock", "--update"]);
        assert_eq!(exit, Exit::Success, "{odoo}: {err}");
        assert!(
            project
                .read("odx.lock")
                .contains(&format!("rev = \"{base}\"")),
            "{odoo}"
        );
    }

    // A package whose ref does not exist, and an unreadable pyproject.
    std::fs::write(
        project.root.join("pyproject.toml"),
        project
            .read("pyproject.toml")
            .replace("tag = \"v1\"", "tag = \"v9\""),
    )
    .unwrap();
    let (exit, _, err) = project.run(&["lock", "--update"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odoo-addon-other: not resolved"), "{err}");
    assert!(err.contains("packages from "), "{err}");
}

#[test]
fn verify_checks_packages_and_uv_lock() {
    let project = Project::new();
    project.write(false, "");
    assert_eq!(project.run(&["lock"]).0, Exit::Success);
    let (exit, _, err) = project.run(&["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("uv.lock is missing"), "{err}");

    let lock = project.read("odx.lock");
    let sha_of = |dist: &str| {
        let start = lock.find(&format!("[packages.{dist}]")).unwrap();
        let line = lock[start..]
            .lines()
            .find(|l| l.starts_with("sha = "))
            .unwrap();
        line.trim_start_matches("sha = ")
            .trim_matches('"')
            .to_owned()
    };
    let uv_lock = |pos_x: &str| {
        use std::fmt::Write as _;
        let mut text = String::from("version = 1\n");
        // uv writes normalized names.
        for (name, dist) in [
            ("odoo-addon-pos-x", "odoo-addon-pos_x"),
            ("odoo-addon-branchy", "odoo-addon-branchy"),
            ("odoo-addon-fixed", "odoo-addon-fixed"),
            ("odoo-addon-other", "odoo-addon-other"),
        ] {
            let sha = if name == "odoo-addon-pos-x" {
                pos_x.to_owned()
            } else {
                sha_of(dist)
            };
            let _ = write!(
                text,
                "\n[[package]]\nname = \"{name}\"\nsource = {{ git = \"{}?subdirectory=x#{sha}\" }}\n",
                project.url
            );
        }
        // Entries that are not git packages, or are odd, are ignored.
        text.push_str(concat!(
            "\n[[package]]\nname = \"requests\"\nsource = { registry = \"https://pypi.org/simple\" }\n",
            "\n[[package]]\nversion = \"1\"\n",
            "\n[[package]]\nname = 1\n",
            "\n[[package]]\nname = \"no-source\"\n",
            "\n[[package]]\nname = \"odd-git\"\nsource = { git = 1 }\n",
            "\n[[package]]\nname = \"no-commit\"\nsource = { git = \"https://x/y.git\" }\n",
        ));
        text
    };
    let good = sha_of("odoo-addon-pos_x");
    std::fs::write(project.root.join("uv.lock"), uv_lock(&good)).unwrap();
    let (exit, out, err) = project.run(&["verify"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.starts_with("verified"), "{out}");

    std::fs::write(project.root.join("uv.lock"), uv_lock(&"0".repeat(40))).unwrap();
    let (_, _, err) = project.run(&["verify"]);
    assert!(
        err.contains("odoo-addon-pos_x: uv.lock has another commit"),
        "{err}"
    );
    std::fs::write(project.root.join("uv.lock"), "version = 1\n").unwrap();
    let (_, _, err) = project.run(&["verify"]);
    assert!(
        err.contains("odoo-addon-pos_x: not in uv.lock from git"),
        "{err}"
    );

    // Develop mode, a package changed in the configuration, one removed.
    let text = project.read("pyproject.toml").replace(
        "[tool.uv.sources]\n",
        "[tool.uv.sources]\nodoo-addon-other = { path = \"../other\", editable = true }\nunrelated = { path = \"../x\", editable = true }\n",
    );
    let text = text
        .replace("@refs/pull/1/head", "@17.0")
        .replace("  \"odoo-addon-fixed", "  \"#odoo-addon-fixed");
    std::fs::write(project.root.join("pyproject.toml"), text).unwrap();
    let (_, _, err) = project.run(&["verify"]);
    assert!(err.contains("odoo-addon-other: in develop mode"), "{err}");
    assert!(!err.contains("unrelated"), "{err}");
    assert!(
        err.contains("odoo-addon-pos_x: package not locked as configured"),
        "{err}"
    );
    assert!(
        err.contains("odoo-addon-fixed: package locked but no longer configured"),
        "{err}"
    );
}
