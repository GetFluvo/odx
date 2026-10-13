//! `odx vendor --requirements` and `odx verify --requirements`.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

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

const UV_LOCK: &str = r#"version = 1

[[package]]
name = "p"
version = "17.0.1.0.0"
source = { virtual = "." }
dependencies = [{ name = "python-stdnum" }]

[package.optional-dependencies]
oca = [{ name = "odoo-addon-pos-x" }, { name = "paramiko" }]
test = [{ name = "pytest" }]

[package.metadata]
requires-dist = [
    { name = "odoo-addon-pos-x", marker = "extra == 'oca'", specifier = "==17.0.1.0.0" },
    { name = "python-stdnum", specifier = ">=1.18" },
    { name = "paramiko", marker = "extra == 'oca'", specifier = ">=3" },
    { name = "pytest", marker = "extra == 'test'" },
]

[package.metadata.requires-dev]
tools = [{ name = "cachetools", specifier = ">=5" }]

[package.dev-dependencies]
tools = [{ name = "cachetools" }]

[[package]]
name = "odoo-addon-pos-x"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo" }, { name = "odoo-addon-base-y" }, { name = "unidecode" }]

[[package]]
name = "odoo-addon-base-y"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo-addon-pos-x" }, { name = "lxml" }]

[[package]]
name = "python-stdnum"
version = "1.20"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "unidecode"
version = "1.3.8"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "paramiko"
version = "3.4.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "cachetools"
version = "5.3.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "pytest"
version = "8.0.0"
source = { registry = "https://pypi.org/simple" }
"#;

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

impl Project {
    /// A vendored-source project: groups `oca` and `tools` ship, `test`
    /// does not; a custom addon and an aggregated repository with a
    /// selected and an unselected addon.
    fn new(platform: &str, repositories: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let repos = if repositories {
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\naddons = [\"pos_*\"]\nmerges = []\n"
        } else {
            ""
        };
        write(
            &root.join("pyproject.toml"),
            &format!(
                "[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"17.0\"\n\n\
                 [tool.odx.groups]\noca = \"modules/oca\"\ntools = \"modules/tools\"\nghost = \"modules/ghost\"\n\
                 project_addons = [\"modules/custom\"]\n\n[tool.odx.platform]\n{platform}\n{repos}"
            ),
        );
        write(&root.join("uv.lock"), UV_LOCK);
        write(
            &root.join("modules/custom/c/__manifest__.py"),
            "{'name': 'C', 'external_dependencies': {'python': ['dateutil', 'babel>=99', 'paramiko']}}\n",
        );
        write(
            &root.join("modules/custom/old/__manifest__.py"),
            "{'external_dependencies': {'python': ['ignored']}, 'installable': False}\n",
        );
        write(
            &root.join("vendor/pos/pos_y/__manifest__.py"),
            "{\"external_dependencies\": {\"python\": [\"pysftp\"]}}\n",
        );
        write(
            &root.join("vendor/pos/other/__manifest__.py"),
            "{\"external_dependencies\": {\"python\": [\"unselected\"]}}\n",
        );
        let env = MapEnv::default().with(
            "ODX_CACHE_DIR",
            tmp.path().join("cache").display().to_string(),
        );
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

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.root.join(file)).unwrap()
    }
}

const BODY: &str = "\
cachetools==5.3.0  # via group tools
paramiko==3.4.0  # via c, group oca
pysftp  # via pos_y
python-stdnum==1.20  # via pyproject.toml
unidecode==1.3.8  # via odoo-addon-pos-x
";

#[test]
fn vendored_source_requirements() {
    let project = Project::new(
        "profile = \"vendored-source\"\nprovided_extra = [\"python-stdnum==1.19\"]\nprovided_remove = [\"python-stdnum\"]",
        true,
    );
    let (exit, out, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(
        out,
        "wrote dist/requirements.txt: 5 requirements, 3 provided by the platform left out\n"
    );
    assert_eq!(
        err,
        "warning: babel: c requires >=99, the platform provides 2.9.1 or 2.10.3 or 2.17.0\n\
         warning: pysftp: no single version in uv.lock; written unpinned (run `uv lock`)\n"
    );
    let text = project.read("dist/requirements.txt");
    assert!(
        text.starts_with(
            "# Generated by `odx vendor --requirements`; edits are overwritten.\n\
             # profile vendored-source, provided set odoo-standard-17.0\n# odx-digest: "
        ),
        "{text}"
    );
    assert!(text.ends_with(BODY), "{text}");

    // Unchanged: nothing to write, nothing to report about edits.
    let (_, _, err) = project.odx(&["vendor", "--requirements"]);
    assert!(!err.contains("edited by hand"), "{err}");
    // A hand edit is overwritten and reported.
    write(
        &project.root.join("dist/requirements.txt"),
        &text.replace("paramiko==3.4.0", "paramiko==3.0"),
    );
    let (_, _, err) = project.odx(&["vendor", "--requirements"]);
    assert!(
        err.starts_with(
            "warning: dist/requirements.txt: edited by hand; the edits are overwritten\n"
        ),
        "{err}"
    );
    assert_eq!(project.read("dist/requirements.txt"), text);

    let (exit, _, err) = project.odx(&["vendor", "--requirements", "--strict"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.ends_with("error: 1 requirements conflict with the platform (--strict)\n"),
        "{err}"
    );

    let (exit, out, _) = project.odx(&["--json", "vendor", "--requirements", "deploy/req.txt"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["file"], "deploy/req.txt");
    assert_eq!(
        value["omitted"],
        serde_json::json!(["babel", "lxml", "python-dateutil"])
    );
    assert_eq!(value["conflicts"][0]["by"], "c");
    assert_eq!(value["lines"][3]["specifier"], "==1.20");
    assert_eq!(project.read("deploy/req.txt"), text);
}

#[test]
fn odoo_sh_requirements_and_verify() {
    let project = Project::new("profile = \"odoo-sh\"", false);
    git(&project.root, &["init", "--quiet"]);
    git(&project.root, &["add", "-A"]);
    git(&project.root, &["commit", "--quiet", "-m", "init"]);

    let (exit, _, err) = project.odx(&["verify", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("requirements.txt is missing; run `odx vendor --requirements`"),
        "{err}"
    );
    assert!(err.contains("babel: c requires >=99"), "{err}");

    let (exit, out, _) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("wrote requirements.txt: "), "{out}");
    let text = project.read("requirements.txt");
    assert!(
        text.contains("# profile odoo-sh, provided set odoo-sh-17.0\n"),
        "{text}"
    );

    // The conflict is still unresolved.
    let (exit, _, err) = project.odx(&["verify", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.ends_with(
            "\n  babel: c requires >=99, the platform provides 2.9.1 or 2.10.3 or 2.17.0\n"
        ),
        "{err}"
    );
    write(
        &project.root.join("modules/custom/c/__manifest__.py"),
        "{'external_dependencies': {'python': ['dateutil']}}",
    );
    git(&project.root, &["commit", "--quiet", "-am", "fix"]);
    let (exit, _, err) = project.odx(&["verify", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("requirements.txt is not what `odx vendor --requirements` writes"),
        "{err}"
    );
    project.odx(&["vendor", "--requirements"]);
    let (exit, out, err) = project.odx(&["verify", "--requirements"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");

    // A provided package added by hand.
    let text = project.read("requirements.txt");
    write(
        &project.root.join("requirements.txt"),
        &format!("{text}-r other.txt\n\nLXML==5.0\n"),
    );
    let (exit, _, err) = project.odx(&["verify", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("requirements.txt: lists lxml, which the platform provides"),
        "{err}"
    );
}

#[test]
fn requirements_failures() {
    let project = Project::new("profile = \"vendored-source\"", false);
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("installing addon directories is not implemented yet"),
        "{err}"
    );

    write(
        &project.root.join("modules/custom/c/__manifest__.py"),
        "{'name': open('x')}",
    );
    let (exit, _, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        err.contains("warning: modules/custom/c/__manifest__.py: cannot read the manifest: expected a literal at byte 9"),
        "{err}"
    );

    std::fs::remove_file(project.root.join("uv.lock")).unwrap();
    let (exit, _, _) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    write(&project.root.join("uv.lock"), "package = 1");
    let (exit, _, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("uv.lock: "), "{err}");
    std::fs::remove_file(project.root.join("uv.lock")).unwrap();

    write(
        &project.root.join("blocked"),
        "a file where a directory belongs",
    );
    let (exit, _, err) = project.odx(&["vendor", "--requirements", "blocked/x/requirements.txt"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("blocked"), "{err}");
    let (exit, _, err) = project.odx(&["vendor", "--requirements", "modules"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("modules"), "{err}");

    let other = Project::new(
        "profile = \"vendored-source\"\nprovided = \"missing.txt\"",
        false,
    );
    let (exit, _, err) = other.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("missing.txt"), "{err}");
    let (exit, _, err) = other.odx(&["verify", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("missing.txt"), "{err}");
    write(&other.root.join("missing.txt"), "paramiko==3.4.0\n");
    let (exit, out, _) = other.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("1 provided by the platform left out"), "{out}");
    let (exit, _, err) = other.odx(&["-C", "/", "vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage, "{err}");

    let old = Project::new("profile = \"vendored-source\"", false);
    let pyproject = old.read("pyproject.toml").replace("\"17.0\"", "\"12.0\"");
    write(&old.root.join("pyproject.toml"), &pyproject);
    let (exit, _, err) = old.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("odx has no provided set odoo-standard for Odoo 12.0"), "{err}");

    // generic-wheels exports uv.lock, which must exist, with uv.
    let wheels = Project::new("profile = \"generic-wheels\"", false);
    std::fs::remove_file(wheels.root.join("uv.lock")).unwrap();
    let (exit, _, err) = wheels.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("uv.lock is missing; the generic-wheels profile exports it"),
        "{err}"
    );
    write(&wheels.root.join("uv.lock"), UV_LOCK);
    let mut wheels = wheels;
    wheels.env = wheels.env.clone().with("ODX_UV", "odx-surely-missing-uv");
    let (exit, _, err) = wheels.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx-surely-missing-uv export failed"), "{err}");
    wheels.env = wheels.env.clone().with("ODX_UV", "git");
    let (exit, _, err) = wheels.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("git export failed: "), "{err}");
}

/// A stand-in for `uv` that prints a fixed export and records its
/// arguments.
#[cfg(unix)]
#[test]
fn generic_wheels_export() {
    use std::os::unix::fs::PermissionsExt;

    let mut project = Project::new("", false);
    let uv = project.root.join("fake-uv");
    let args = project.root.join("uv-args");
    write(
        &uv,
        &format!(
            "#!/bin/sh\necho \"$@\" > '{}'\ncat <<'EOF'\n\
             # This file was autogenerated by uv\n\
             babel==2.10.3 \\\n    --hash=sha256:aa\n\
             odoo @ git+https://github.com/OCA/OCB.git@0123\n\
             paramiko==3.4.0 \\\n    --hash=sha256:bb\nEOF\n",
            args.display()
        ),
    );
    std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
    project.env = project.env.clone().with("ODX_UV", uv.display().to_string());
    let (exit, out, _) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "wrote dist/requirements.txt: 0 requirements, 2 provided by the platform left out\n"
    );
    assert_eq!(
        project.read("uv-args"),
        "export --frozen --no-dev --no-emit-project --extra oca --group tools\n"
    );
    let text = project.read("dist/requirements.txt");
    assert!(
        text.ends_with(
            "# This file was autogenerated by uv\nparamiko==3.4.0 \\\n    --hash=sha256:bb\n"
        ),
        "{text}"
    );
    assert!(text.contains("# profile generic-wheels, provided set odoo-standard-17.0\n"));
}
