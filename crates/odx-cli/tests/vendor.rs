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
wheels = [{ url = "POS_X_URL", hash = "POS_X_HASH" }]

[[package]]
name = "odoo-addon-base-y"
version = "17.0.1.0.0"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo-addon-pos-x" }, { name = "lxml" }]
wheels = [{ url = "BASE_Y_URL", hash = "BASE_Y_HASH" }]

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
    tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// A wheel of addon `addon` with `files` (path inside the addon, content)
/// at `path`; returns its URL and hash.
fn wheel(path: &Path, addon: &str, files: &[(&str, &str)]) -> (String, String) {
    use sha2::Digest as _;
    use std::io::Write as _;

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in files {
        writer
            .start_file(format!("odoo/addons/{addon}/{name}"), options)
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer
        .start_file(format!("{addon}.dist-info/RECORD"), options)
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    write(path, "");
    std::fs::write(path, &bytes).unwrap();
    let hash = sha2::Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    (file_url(path), format!("sha256:{hash}"))
}

fn manifest(version: &str) -> String {
    format!("{{\n    \"name\": \"X\",\n    \"version\": \"{version}\",\n}}\n")
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
        let wheels = tmp.path().join("wheels");
        let (pos_url, pos_hash) = wheel(
            &wheels.join("pos_x.whl"),
            "pos_x",
            &[
                ("__manifest__.py", &manifest("17.0.1.0.0")),
                ("models/x.py", "x = 1\n"),
                ("static/description/index.html", "<html/>"),
            ],
        );
        let (base_url, base_hash) = wheel(
            &wheels.join("base_y.whl"),
            "base_y",
            &[("__manifest__.py", &manifest("17.0.2.0.0"))],
        );
        write(
            &root.join("uv.lock"),
            &UV_LOCK
                .replace("POS_X_URL", &pos_url)
                .replace("POS_X_HASH", &pos_hash)
                .replace("BASE_Y_URL", &base_url)
                .replace("BASE_Y_HASH", &base_hash),
        );
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
        "vendored ghost: 0 addons into modules/ghost\n\
         vendored oca: 2 addons into modules/oca\n\
         vendored tools: 0 addons into modules/tools\n\
         wrote dist/requirements.txt: 5 requirements, 3 provided by the platform left out\n"
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
    assert_eq!(value["requirements"]["file"], "deploy/req.txt");
    assert_eq!(
        value["requirements"]["omitted"],
        serde_json::json!(["babel", "lxml", "python-dateutil"])
    );
    assert_eq!(value["requirements"]["conflicts"][0]["by"], "c");
    assert_eq!(value["requirements"]["lines"][3]["specifier"], "==1.20");
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
    assert!(out.contains("\nwrote requirements.txt: "), "{out}");
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

    let uv_lock = project.read("uv.lock");
    std::fs::remove_file(project.root.join("uv.lock")).unwrap();
    let (exit, _, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("uv.lock is missing; run `uv lock`"), "{err}");
    write(&project.root.join("uv.lock"), "package = 1");
    let (exit, _, err) = project.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("uv.lock: "), "{err}");
    write(&project.root.join("uv.lock"), &uv_lock);

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
    assert!(
        err.contains("odx has no provided set odoo-standard for Odoo 12.0"),
        "{err}"
    );

    // generic-wheels exports uv.lock, which must exist, with uv.
    let wheels = Project::new("profile = \"generic-wheels\"", false);
    let (exit, _, err) = wheels.odx(&["vendor"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("the generic-wheels profile ships wheels"),
        "{err}"
    );
    write(&wheels.root.join("uv.lock"), "package = 1");
    let (exit, _, err) = wheels.odx(&["vendor", "--requirements"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("uv.lock: "), "{err}");
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

/// Every file under `dir` with its bytes, sorted.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, files);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                files.push((name.replace('\\', "/"), std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort();
    files
}

impl Project {
    /// Replace the wheel of `pos_x` in `uv.lock` with one holding `files`.
    fn replace_pos_x(&self, name: &str, files: &[(&str, &str)]) {
        let (url, hash) = wheel(&self.tmp.path().join("wheels").join(name), "pos_x", files);
        let text: String = self
            .read("uv.lock")
            .lines()
            .map(|line| {
                if line.contains("pos_x") && line.starts_with("wheels") {
                    format!("wheels = [{{ url = \"{url}\", hash = \"{hash}\" }}]\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        write(&self.root.join("uv.lock"), &text);
    }
}

const VENDORED: &str = "\
vendored ghost: 0 addons into modules/ghost
vendored oca: 2 addons into modules/oca
vendored tools: 0 addons into modules/tools
";

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one project through every vendoring step"
)]
fn vendoring_is_reproducible_and_bumps_changed_content() {
    let project = Project::new("profile = \"vendored-source\"", false);
    let (exit, out, err) = project.odx(&["vendor"]);
    assert_eq!(
        (exit, out.as_str(), err.as_str()),
        (Exit::Success, VENDORED, "")
    );
    let oca = project.root.join("modules/oca");
    assert_eq!(project.read("modules/oca/pos_x/models/x.py"), "x = 1\n");
    assert!(!oca.join("pos_x/static/description/index.html").exists());
    assert!(!oca.join("pos_x.dist-info").exists());
    let record = project.read(".odx/vendor.lock");
    assert!(record.starts_with("# Generated by odx vendor."), "{record}");
    assert!(record.contains("name = \"base_y\"\ngroup = \"oca\"\npackage = \"odoo-addon-base-y\"\nsource = \"17.0.1.0.0\"\nwheel = \"sha256:"), "{record}");
    let first = snapshot(&project.root.join("modules"));

    // Again: byte for byte the same.
    let (exit, out, _) = project.odx(&["vendor"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, VENDORED));
    assert_eq!(snapshot(&project.root.join("modules")), first);
    assert_eq!(project.read(".odx/vendor.lock"), record);

    // New content under the same version: the version is bumped, and the
    // bump is kept while the content stays.
    project.replace_pos_x(
        "pos_x-2.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.0.0")),
            ("models/x.py", "x = 2\n"),
        ],
    );
    let (_, out, _) = project.odx(&["vendor"]);
    assert!(
        out.ends_with(
            "bumped pos_x 17.0.1.0.0 -> 17.0.1.0.1: content changed without a version change\n"
        ),
        "{out}"
    );
    assert!(
        project
            .read("modules/oca/pos_x/__manifest__.py")
            .contains("\"17.0.1.0.1\"")
    );
    let bumped = snapshot(&project.root.join("modules"));
    let (_, out, _) = project.odx(&["vendor"]);
    assert_eq!(out, VENDORED);
    assert_eq!(snapshot(&project.root.join("modules")), bumped);
    let (_, out, _) = project.odx(&["--json", "vendor"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        value["vendored"][1]["addons"],
        serde_json::json!(["base_y", "pos_x"])
    );
    assert_eq!(value["bumped"], serde_json::json!([]));
    assert!(value.get("requirements").is_none());

    // Upstream releases a new version: no bump of its own.
    project.replace_pos_x(
        "pos_x-3.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.1.0")),
            ("models/x.py", "x = 3\n"),
        ],
    );
    let (_, out, _) = project.odx(&["vendor"]);
    assert_eq!(out, VENDORED);
    assert!(
        project
            .read("modules/oca/pos_x/__manifest__.py")
            .contains("\"17.0.1.1.0\"")
    );

    // A version without a numeric last segment cannot be bumped.
    project.replace_pos_x(
        "pos_x-4.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.1.x")),
            ("models/x.py", "x = 4\n"),
        ],
    );
    project.odx(&["vendor"]);
    project.replace_pos_x(
        "pos_x-5.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.1.x")),
            ("models/x.py", "x = 5\n"),
        ],
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Success);
    assert!(err.contains("warning: pos_x: content changed without a version change, and the version has no numeric last segment to bump"), "{err}");

    // Turned off, nothing is bumped.
    let pyproject = project.read("pyproject.toml");
    write(
        &project.root.join("pyproject.toml"),
        &format!(
            "{pyproject}\n[tool.odx.vendor]\nbump_on_content_change = false\nexclude = []\nmanifest = \"vendor.toml\"\n"
        ),
    );
    project.odx(&["vendor"]);
    project.replace_pos_x(
        "pos_x-6.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.1.0")),
            ("models/x.py", "x = 6\n"),
        ],
    );
    project.odx(&["vendor"]);
    project.replace_pos_x(
        "pos_x-7.whl",
        &[
            ("__manifest__.py", &manifest("17.0.1.1.0")),
            ("models/x.py", "x = 7\n"),
            ("static/description/index.html", "<html/>"),
        ],
    );
    let (_, out, _) = project.odx(&["vendor"]);
    assert_eq!(out, VENDORED);
    assert!(
        project
            .read("modules/oca/pos_x/static/description/index.html")
            .contains("html")
    );
    assert!(
        project
            .read("vendor.toml")
            .contains("written_version = \"17.0.1.1.0\"")
    );
}

#[test]
fn verify_vendored_and_deployed() {
    let project = Project::new("profile = \"vendored-source\"", false);
    git(&project.root, &["init", "--quiet"]);
    git(&project.root, &["add", "pyproject.toml", "modules/custom"]);
    git(&project.root, &["commit", "--quiet", "-m", "init"]);
    let (exit, _, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains(".odx/vendor.lock is missing; run `odx vendor`"),
        "{err}"
    );

    project.odx(&["vendor"]);
    let (exit, out, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    // An empty group whose directory is gone is no problem.
    std::fs::remove_dir_all(project.root.join("modules/ghost")).unwrap();
    let (exit, out, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");

    let oca = project.root.join("modules/oca");
    write(&oca.join("pos_x/models/x.py"), "x = 'patched'\n");
    std::fs::remove_dir_all(oca.join("base_y")).unwrap();
    write(&oca.join("stray/__manifest__.py"), "{}");
    let (exit, _, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.ends_with(
            "  modules/oca/base_y: missing\n  modules/oca/pos_x: changed since `odx vendor`\n  modules/oca/stray: not vendored by odx\n"
        ),
        "{err}"
    );

    // A deployment checkout with the same directories.
    project.odx(&["vendor"]);
    let deploy = project.tmp.path().join("deploy");
    for group in ["oca", "tools", "ghost"] {
        for (name, bytes) in snapshot(&project.root.join("modules").join(group)) {
            let path = deploy.join("modules").join(group).join(name);
            write(&path, "");
            std::fs::write(&path, bytes).unwrap();
        }
        std::fs::create_dir_all(deploy.join("modules").join(group)).unwrap();
    }
    let deployed = deploy.display().to_string();
    let (exit, out, err) = project.odx(&["verify", "--deployed", &deployed]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    write(&deploy.join("modules/oca/base_y/__manifest__.py"), "{}");
    let (exit, _, err) = project.odx(&["verify", "--deployed", &deployed]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("  modules/oca/base_y: changed since `odx vendor`"),
        "{err}"
    );

    // uv.lock moved on without vendoring again.
    project.replace_pos_x(
        "pos_x-2.whl",
        &[("__manifest__.py", &manifest("17.0.1.0.1"))],
    );
    let (exit, _, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("pos_x: vendored from odoo-addon-pos-x 17.0.1.0.0, which the lockfiles no longer pin; run `odx vendor`"),
        "{err}"
    );

    write(&project.root.join(".odx/vendor.lock"), "version = 9\n");
    let (exit, _, err) = project.odx(&["verify", "--vendored"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("vendor manifest version 9 is not supported"),
        "{err}"
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("vendor manifest version 9 is not supported"),
        "{err}"
    );
}

#[test]
fn packages_from_git_come_from_the_cache() {
    let project = Project::new("profile = \"vendored-source\"", false);
    let up = project.tmp.path().join("up");
    std::fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
    write(&up.join("pos_g/__manifest__.py"), &manifest("17.0.1.0.0"));
    write(&up.join("pos_g/models/g.py"), "g = 1\n");
    write(&up.join("other/__manifest__.py"), "{}");
    git(&up, &["add", "-A"]);
    git(&up, &["commit", "--quiet", "-m", "g"]);
    let out = Command::new("git")
        .current_dir(&up)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let url = file_url(&up);
    let cache = project
        .tmp
        .path()
        .join("cache")
        .join("repos")
        .join(odx_git::url_hash(&url));
    git(
        project.tmp.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            up.to_str().unwrap(),
            cache.to_str().unwrap(),
        ],
    );
    let uv_lock = project.read("uv.lock").replace(
        "oca = [{ name = \"odoo-addon-pos-x\" }",
        "oca = [{ name = \"odoo-addon-pos-g\" }, { name = \"odoo-addon-pos-x\" }",
    ) + &format!(
        "\n[[package]]\nname = \"odoo-addon-pos-g\"\nversion = \"17.0.1.0.0\"\nsource = {{ git = \"{url}?subdirectory=pos_g&branch=17.0#{sha}\" }}\n"
    );
    write(&project.root.join("uv.lock"), &uv_lock);
    let lock = |sha: &str| {
        format!(
            "# Generated by odx. Do not edit.\nversion = 1\n\n[packages.odoo-addon-pos-g]\nurl = \"{url}\"\nsource = {{ branch = \"17.0\" }}\nsubdirectory = \"pos_g\"\nsha = \"{sha}\"\n"
        )
    };
    write(&project.root.join("odx.lock"), &lock(&sha));
    let (exit, out, err) = project.odx(&["vendor"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(
        out.contains("vendored oca: 3 addons into modules/oca\n"),
        "{out}"
    );
    assert_eq!(project.read("modules/oca/pos_g/models/g.py"), "g = 1\n");
    assert!(!project.root.join("modules/oca/other").exists());
    assert!(
        project
            .read(".odx/vendor.lock")
            .contains(&format!("source = \"git {sha}\""))
    );

    let (exit, _, _) = project.odx(&["verify", "--vendored"]);
    assert_eq!(exit, Exit::Failure, "the project is no git checkout");
    write(&project.root.join("odx.lock"), &lock(&"2".repeat(40)));
    let (_, _, err) = project.odx(&["verify", "--vendored"]);
    assert!(
        err.contains(&format!(
            "pos_g: vendored from odoo-addon-pos-g git {sha}, which the lockfiles no longer pin"
        )),
        "{err}"
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains(&format!(
            "odoo-addon-pos-g: commit {} of {url} is not in the cache",
            "2".repeat(40)
        )),
        "{err}"
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "one project through every failure")]
fn vendoring_failures() {
    let project = Project::new("profile = \"vendored-source\"", false);
    let pyproject = project.read("pyproject.toml");
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace("oca = \"modules/oca\"", "oca = \".\""),
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx vendor replaces this directory, so it must be inside the project and hold no project files"), "{err}");
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace("oca = \"modules/oca\"", "oca = \"sub\""),
    );
    write(&project.root.join("sub/pyproject.toml"), "");
    let (exit, _, _) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace("oca = \"modules/oca\"", "oca = \"../outside\""),
    );
    let (exit, _, _) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);

    // A group directory that is a file, or whose parent is one.
    write(&project.root.join("pyproject.toml"), &pyproject);
    write(&project.root.join("modules/oca"), "a file");
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("modules/oca"), "{err}");
    std::fs::remove_file(project.root.join("modules/oca")).unwrap();
    write(&project.root.join("blocked"), "a file");
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace("oca = \"modules/oca\"", "oca = \"blocked/oca\""),
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("blocked"), "{err}");
    write(
        &project.root.join("pyproject.toml"),
        &pyproject.replace(
            "oca = \"modules/oca\"",
            "oca = \"./modules/../modules/oca\"",
        ),
    );
    let (exit, out, _) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Success, "{out}");

    let groupless: String = pyproject
        .lines()
        .filter(|line| !line.contains("= \"modules/"))
        .flat_map(|line| [line, "\n"])
        .collect();
    write(&project.root.join("pyproject.toml"), &groupless);
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("no [tool.odx.groups] directories to vendor into"),
        "{err}"
    );

    write(&project.root.join("pyproject.toml"), &pyproject);
    let uv_lock = project.read("uv.lock");
    write(
        &project.root.join("uv.lock"),
        &uv_lock.replace("source = { virtual = \".\" }", ""),
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("uv.lock has no project package"), "{err}");

    // A wheel that does not match its hash.
    write(&project.root.join("uv.lock"), &uv_lock);
    std::fs::write(project.tmp.path().join("wheels/base_y.whl"), "tampered").unwrap();
    std::fs::remove_dir_all(project.tmp.path().join("cache/wheels")).unwrap();
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("hash mismatch"), "{err}");

    // The vendor manifest cannot be written.
    let (url, hash) = wheel(
        &project.tmp.path().join("wheels/base_y.whl"),
        "base_y",
        &[("__manifest__.py", &manifest("17.0.2.0.0"))],
    );
    let fixed: String = uv_lock
        .lines()
        .map(|line| {
            if line.contains("base_y") && line.starts_with("wheels") {
                format!("wheels = [{{ url = \"{url}\", hash = \"{hash}\" }}]\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect();
    write(&project.root.join("uv.lock"), &fixed);
    std::fs::remove_dir_all(project.root.join(".odx")).unwrap();
    write(
        &project.root.join(".odx"),
        "a file where the manifest directory belongs",
    );
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("vendor.lock"), "{err}");

    // A "wheel" with the right hash that is no zip archive.
    std::fs::remove_file(project.root.join(".odx")).unwrap();
    let broken = project.tmp.path().join("wheels/broken.whl");
    std::fs::write(&broken, "not a zip").unwrap();
    let broken_hash = {
        use sha2::Digest as _;
        sha2::Sha256::digest(b"not a zip")
            .iter()
            .fold(String::from("sha256:"), |mut hex, byte| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{byte:02x}");
                hex
            })
    };
    let corrupt = fixed
        .replace(&url, &file_url(&broken))
        .replace(&hash, &broken_hash);
    write(&project.root.join("uv.lock"), &corrupt);
    let (exit, _, err) = project.odx(&["vendor"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("broken.whl: "), "{err}");

    // The vendor manifest where [tool.odx.layout] puts it.
    write(&project.root.join("uv.lock"), &fixed);
    write(
        &project.root.join("pyproject.toml"),
        &format!("{pyproject}\n[tool.odx.layout]\nvendor_manifest = \"deploy/vendor.lock\"\n"),
    );
    let (exit, out, err) = project.odx(&["vendor"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(
        project
            .read("deploy/vendor.lock")
            .contains("name = \"pos_x\"")
    );
}
