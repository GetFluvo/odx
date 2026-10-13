//! `odx verify --installed`.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;
use odx_platform::installed::record_hash;
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

/// `files` with their RECORD lines.
fn record(files: &[(&str, &str)], dist_info: &str) -> String {
    let mut text = String::new();
    for (name, content) in files {
        let _ = writeln!(
            text,
            "{name},{},{}",
            record_hash(content.as_bytes()),
            content.len()
        );
    }
    let _ = writeln!(text, "{dist_info}/RECORD,,");
    text
}

/// A wheel of `odoo-addon-a` 1.0 holding `files`; returns its sha256.
fn wheel(path: &Path, files: &[(&str, &str)]) -> String {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, content) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content.as_bytes()).unwrap();
    }
    writer
        .start_file("odoo_addon_a-1.0.dist-info/RECORD", options)
        .unwrap();
    writer
        .write_all(record(files, "odoo_addon_a-1.0.dist-info").as_bytes())
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    write(path, "");
    std::fs::write(path, &bytes).unwrap();
    sha2::Sha256::digest(&bytes)
        .iter()
        .fold(String::from("sha256:"), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Install a distribution into `site` as an installer would.
fn install(site: &Path, name: &str, version: &str, files: &[(&str, &str)], extra: &[(&str, &str)]) {
    let dist_info = format!("{}-{version}.dist-info", name.replace('-', "_"));
    for (file, content) in files {
        write(&site.join(file), content);
    }
    write(
        &site.join(&dist_info).join("METADATA"),
        &format!("Metadata-Version: 2.1\nName: {name}\nVersion: {version}\n"),
    );
    write(
        &site.join(&dist_info).join("RECORD"),
        &record(files, &dist_info),
    );
    write(
        &site.join(&dist_info).join("WHEEL"),
        "Wheel-Version: 1.0\nTag: py3-none-any\n",
    );
    for (file, content) in extra {
        write(&site.join(&dist_info).join(file), content);
    }
}

struct Project {
    tmp: tempfile::TempDir,
    root: PathBuf,
    site: PathBuf,
    env: MapEnv,
}

const FILES: &[(&str, &str)] = &[
    ("odoo/addons/a/__manifest__.py", "{}"),
    ("odoo/addons/a/x.py", "x = 1\n"),
];

impl Project {
    /// A project whose uv.lock pins `odoo-addon-a` 1.0 (a local wheel),
    /// `gitpkg` from git and `other` 2.0, and a virtual environment with
    /// all of them installed as locked.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let wheel_path = tmp.path().join("wheels/odoo_addon_a-1.0-py3-none-any.whl");
        let hash = wheel(&wheel_path, FILES);
        write(
            &root.join("pyproject.toml"),
            "[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"17.0\"\n",
        );
        write(
            &root.join("uv.lock"),
            &format!(
                "version = 1\n\n[[package]]\nname = \"odoo-addon-a\"\nversion = \"1.0\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n\
                 wheels = [{{ url = \"https://files/broken.whl\", hash = \"sha256:00\" }}, {{ url = \"https://files/odoo_addon_a-1.0-cp312-cp312-win_amd64.whl\", hash = \"sha256:00\" }}, {{ url = \"{}\", hash = \"{hash}\" }}]\n\n\
                 [[package]]\nname = \"gitpkg\"\nversion = \"0.1\"\nsource = {{ git = \"https://x/gitpkg.git?rev=main#0123\" }}\n\n\
                 [[package]]\nname = \"localpkg\"\nversion = \"0.1\"\nsource = {{ directory = \"../local\" }}\n\n[[package]]\nname = \"other\"\nversion = \"2.0\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n",
                file_url(&wheel_path)
            ),
        );
        let site = tmp.path().join("venv/lib/python3.12/site-packages");
        install(&site, "odoo-addon-a", "1.0", FILES, &[]);
        install(
            &site,
            "gitpkg",
            "0.1",
            &[("gitpkg/__init__.py", "")],
            &[(
                "direct_url.json",
                "{\"vcs_info\": {\"commit_id\": \"0123\"}}",
            )],
        );
        install(
            &site,
            "pip",
            "24.0",
            &[("pip/__init__.py", "")],
            &[("direct_url.json", "{\"url\": \"https://x/pip.whl\"}")],
        );
        install(
            &site,
            "localpkg",
            "0.1",
            &[("localpkg/__init__.py", "")],
            &[],
        );
        // Not distributions, and a RECORD with quoted names.
        std::fs::create_dir_all(site.join("broken-1.0.dist-info")).unwrap();
        write(
            &site.join("noversion-1.dist-info/METADATA"),
            "Name: noversion\n",
        );
        write(&site.join("file.dist-info"), "");
        write(&site.join("noname-1.dist-info/METADATA"), "Version: 1\n");
        install(&site, "quoted", "1.0", &[], &[]);
        write(
            &site.join("quoted-1.0.dist-info/RECORD"),
            "\"q\"\"uote, too.py\",,\n",
        );
        let env = MapEnv::default().with(
            "ODX_CACHE_DIR",
            tmp.path().join("cache").display().to_string(),
        );
        Self {
            tmp,
            root,
            site,
            env,
        }
    }

    fn verify(&self, dir: &Path) -> (Exit, String) {
        let dir = dir.display().to_string();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(
            ["odx", "verify", "--installed", dir.as_str()],
            &OsFs,
            &self.env,
            &self.root,
            &mut out,
            &mut err,
        );
        (exit, String::from_utf8(err).unwrap())
    }
}

#[test]
fn installed_wheels_match_the_lock() {
    let project = Project::new();
    let venv = project.tmp.path().join("venv");
    let (exit, err) = project.verify(&venv);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    let (exit, _) = project.verify(&project.site);
    assert_eq!(exit, Exit::Success);
}

#[test]
fn substituted_and_changed_installations_are_found() {
    let project = Project::new();
    let venv = project.tmp.path().join("venv");
    // Another wheel with the same version, installed with its own RECORD.
    install(
        &project.site,
        "odoo-addon-a",
        "1.0",
        &[
            ("odoo/addons/a/__manifest__.py", "{}"),
            ("odoo/addons/a/x.py", "x = 'evil'\n"),
        ],
        &[],
    );
    // A file edited after installation, another commit, another version.
    write(&project.site.join("gitpkg/__init__.py"), "patched = True\n");
    install(
        &project.site,
        "gitpkg",
        "0.1",
        &[],
        &[(
            "direct_url.json",
            "{\"vcs_info\": {\"commit_id\": \"4567\"}}",
        )],
    );
    install(&project.site, "other", "1.0", &[], &[]);
    let (exit, err) = project.verify(&venv);
    assert_eq!(exit, Exit::Failure);
    for expected in [
        "gitpkg: installed from commit 4567, uv.lock pins 0123",
        "odoo-addon-a 1.0: not the locked wheel (1 files differ, such as odoo/addons/a/x.py)",
        "other 1.0 is installed, uv.lock pins 2.0",
    ] {
        assert!(err.contains(expected), "{expected}\n{err}");
    }

    let project = Project::new();
    write(&project.site.join("odoo/addons/a/x.py"), "x = 2\n");
    let (_, err) = project.verify(&project.site);
    assert!(
        err.contains("odoo-addon-a: odoo/addons/a/x.py changed after installation"),
        "{err}"
    );
    let (_, err) = project.verify(&project.tmp.path().join("nowhere"));
    assert!(err.contains("no site-packages found"), "{err}");
}

#[test]
fn installed_failures() {
    let project = Project::new();
    // No wheel with the installed tags.
    write(
        &project.site.join("odoo_addon_a-1.0.dist-info/WHEEL"),
        "Tag: cp311-cp311-manylinux_2_17_x86_64\n",
    );
    let (exit, err) = project.verify(&project.site);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("odoo-addon-a 1.0: no wheel in uv.lock matches what is installed"),
        "{err}"
    );

    // The locked wheel cannot be had.
    let project = Project::new();
    std::fs::write(
        project
            .tmp
            .path()
            .join("wheels/odoo_addon_a-1.0-py3-none-any.whl"),
        "gone",
    )
    .unwrap();
    let (_, err) = project.verify(&project.site);
    assert!(
        err.contains("odoo-addon-a: cannot compare with the locked wheel: "),
        "{err}"
    );

    // A wheel that is no zip, under its own hash.
    let project = Project::new();
    let broken = project
        .tmp
        .path()
        .join("wheels/broken-1.0-py3-none-any.whl");
    std::fs::write(&broken, "no zip").unwrap();
    let hash =
        sha2::Sha256::digest(b"no zip")
            .iter()
            .fold(String::from("sha256:"), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
    let lock = std::fs::read_to_string(project.root.join("uv.lock")).unwrap();
    let start = lock.find("{ url = \"file").unwrap();
    let end = start + lock[start..].find('}').unwrap() + 1;
    let replaced = format!(
        "{}{{ url = \"{}\", hash = \"{hash}\" }}{}",
        &lock[..start],
        file_url(&broken),
        &lock[end..]
    );
    write(&project.root.join("uv.lock"), &replaced);
    let (_, err) = project.verify(&project.site);
    assert!(
        err.contains("odoo-addon-a: cannot compare with the locked wheel: "),
        "{err}"
    );

    std::fs::remove_file(project.root.join("uv.lock")).unwrap();
    let (_, err) = project.verify(&project.site);
    assert!(err.contains("uv.lock is missing or unreadable"), "{err}");
}
