//! `odx sync` and `odx cache` with two workspaces sharing one cache.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

struct Machine {
    tmp: tempfile::TempDir,
    env: MapEnv,
}

impl Machine {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            );
        Self { tmp, env }
    }

    /// A workspace whose repository `name` comes from its own upstream.
    fn workspace(&self, name: &str) -> PathBuf {
        let up = self.tmp.path().join(format!("up-{name}"));
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        std::fs::write(up.join("a.txt"), name).unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "a"]);
        let root = self.tmp.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"{name}\"\nurl = \"{}\"\nmerges = []\n",
                file_url(&up)
            ),
        )
        .unwrap();
        std::fs::canonicalize(root).unwrap()
    }

    fn odx(&self, dir: &Path, args: &[&str]) -> (Exit, String, String) {
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(command_line, &OsFs, &self.env, dir, &mut out, &mut err);
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn cache(&self) -> PathBuf {
        self.tmp.path().join("cache")
    }
}

#[test]
fn registered_workspaces_are_synced() {
    let machine = Machine::new();
    let a = machine.workspace("a");
    let b = machine.workspace("b");
    let (exit, out, _) = machine.odx(&a, &["sync", "--register"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.starts_with(&format!("registered {} for `odx sync --all`", a.display())),
        "{out}"
    );
    machine.odx(&b, &["sync", "--register"]);
    machine.odx(&b, &["sync", "--register"]);
    let config = std::fs::read_to_string(machine.tmp.path().join("config/config.toml")).unwrap();
    assert_eq!(config.matches("series = \"17.0\"").count(), 2, "{config}");

    let (exit, out, err) = machine.odx(machine.tmp.path(), &["sync", "--all"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(
        out.contains(&format!("synced {}: fetched a: 1 refs", a.display())),
        "{out}"
    );
    assert!(
        out.contains(&format!("synced {}: fetched b: 1 refs", b.display())),
        "{out}"
    );
    let record = std::fs::read_to_string(machine.cache().join("last_sync.json")).unwrap();
    assert!(record.contains("\"status\": \"synced\""), "{record}");
    let log = std::fs::read_to_string(machine.cache().join("sync.log")).unwrap();
    assert!(log.contains("sync of 2 workspaces, 0 failed"), "{log}");

    // Here only, as JSON, and with no time left.
    let (_, out, _) = machine.odx(&a, &["--json", "sync"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["workspaces"][0]["status"], "synced");
    let (_, out, _) = machine.odx(&a, &["sync", "--max-duration", "0s", "--workspace", "."]);
    assert!(out.starts_with("skipped "), "{out}");

    let (_, out, _) = machine.odx(&a, &["cache", "info"]);
    assert!(
        out.contains("repositories: 2 (0 unused by registered workspaces)"),
        "{out}"
    );
    assert!(out.contains("last sync: "), "{out}");
    let (_, out, _) = machine.odx(&a, &["--json", "cache", "info"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["repositories"], 2);

    // b leaves the registry: its repository is unused from a.
    let (_, out, _) = machine.odx(&b, &["sync", "--unregister"]);
    assert!(out.starts_with("unregistered "), "{out}");
    let (_, out, _) = machine.odx(&b, &["sync", "--unregister"]);
    assert!(out.ends_with("was not registered\n"), "{out}");
    // A registered workspace odx cannot read could use any repository:
    // nothing is collected.
    let pyproject = std::fs::read_to_string(a.join("pyproject.toml")).unwrap();
    std::fs::write(a.join("pyproject.toml"), "[tool.odx\n").unwrap();
    let (code, out, err) = machine.odx(&b, &["cache", "gc"]);
    assert_eq!(code, Exit::Failure, "{out}");
    assert!(err.contains("nothing collected"), "{err}");
    assert!(err.contains(&a.display().to_string()), "{err}");
    std::fs::write(a.join("pyproject.toml"), pyproject).unwrap();
    let (_, out, _) = machine.odx(&a, &["cache", "gc", "--dry-run"]);
    assert!(out.starts_with("would remove file://"), "{out}");
    assert!(out.contains("up-b"), "{out}");
    let (_, out, _) = machine.odx(&a, &["cache", "gc", "--max-size", "1TB"]);
    assert_eq!(out, "freed 0 B\n");
    // A repository another odx is fetching into stays.
    let hash = std::fs::read_dir(machine.cache().join("repos"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| {
            std::fs::read_to_string(machine.cache().join("repos").join(format!("{name}.url")))
                .is_ok_and(|url| url.contains("up-b"))
        })
        .unwrap();
    let held = odx_git::RepoLock::acquire(
        &machine.cache().join("locks").join(format!("{hash}.lock")),
        Duration::ZERO,
    )
    .unwrap();
    let (_, out, _) = machine.odx(&a, &["cache", "gc"]);
    assert_eq!(out, "freed 0 B\n");
    drop(held);
    let (_, out, _) = machine.odx(&a, &["cache", "gc"]);
    assert!(out.starts_with("removed file://"), "{out}");
    let stamps = std::fs::read_dir(machine.cache().join("repos"))
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "fetched"))
        .count();
    assert_eq!(stamps, 1);
    let (_, out, _) = machine.odx(&a, &["cache", "info"]);
    assert!(out.contains("repositories: 1 (0 unused"), "{out}");
}

#[test]
fn sync_failures() {
    let machine = Machine::new();
    let a = machine.workspace("a");
    // Not a workspace.
    let (exit, out, _) = machine.odx(&a, &["sync", "--workspace", "../nowhere"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(out.starts_with("failed "), "{out}");
    let (exit, _, err) = machine.odx(&a, &["sync", "--max-duration", "soon"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("invalid duration"), "{err}");
    // Another sync holds the machine's lock.
    let held = odx_git::RepoLock::acquire(&machine.cache().join("locks/sync.lock"), Duration::ZERO)
        .unwrap();
    let (exit, out, _) = machine.odx(&a, &["sync"]);
    assert_eq!(
        (exit, out.as_str()),
        (
            Exit::Success,
            "another odx sync is running; nothing to do\n"
        )
    );
    drop(held);
    // A foreground command keeps the sync waiting until max_duration.
    let foreground = odx_sync::Presence::enter(&machine.cache()).unwrap();
    let (exit, out, _) = machine.odx(&a, &["sync", "--max-duration", "1s"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("skipped "), "{out}");
    drop(foreground);
    // A workspace whose upstream is gone, and one whose dashboard cannot
    // be built.
    let b = machine.workspace("b");
    std::fs::remove_dir_all(machine.tmp.path().join("up-b")).unwrap();
    let (exit, out, _) = machine.odx(&b, &["sync"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(out.starts_with("failed "), "{out}");
    let pyproject = std::fs::read_to_string(a.join("pyproject.toml")).unwrap();
    std::fs::write(
        a.join("pyproject.toml"),
        pyproject.replacen(
            "odoo_version = \"17.0\"\n",
            "odoo_version = \"17.0\"\n\n[tool.odx.port]\nmigration_commit_patterns = [\"(\"]\n",
            1,
        ),
    )
    .unwrap();
    let (exit, out, _) = machine.odx(&a, &["sync"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(
        out.contains("; dashboard: migration_commit_patterns"),
        "{out}"
    );
    std::fs::write(a.join("pyproject.toml"), pyproject).unwrap();
    // A user configuration that is not TOML.
    std::fs::create_dir_all(machine.tmp.path().join("config")).unwrap();
    std::fs::write(machine.tmp.path().join("config/config.toml"), "[cache\n").unwrap();
    let (exit, _, _) = machine.odx(&a, &["sync"]);
    assert_eq!(exit, Exit::Usage);
    let (exit, _, _) = machine.odx(&a, &["sync", "--register"]);
    assert_eq!(exit, Exit::Usage);
    std::fs::write(
        machine.tmp.path().join("config/config.toml"),
        "[cache]\nmax_size = \"huge\"\n",
    )
    .unwrap();
    let (exit, _, err) = machine.odx(&a, &["cache", "gc"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("invalid size \"huge\""), "{err}");
    let (exit, _, _) = machine.odx(&machine.tmp.path().join("nowhere"), &["sync", "--register"]);
    assert_eq!(exit, Exit::Usage);
}

#[test]
fn registry_without_a_workspace_or_configuration_directory() {
    let machine = Machine::new();
    let a = machine.workspace("a");
    std::fs::create_dir_all(machine.tmp.path().join("config")).unwrap();
    std::fs::write(
        machine.tmp.path().join("config/config.toml"),
        "[workspaces]\n\"/gone\" = { registered = \"2026-10-13\", series = \"17.0\" }\n",
    )
    .unwrap();
    // A locked package's repository counts as used; a repository
    // without its URL file is named by its hash.
    std::fs::write(
        a.join("odx.lock"),
        format!(
            "version = 1\n\n[packages.odoo-addon-p]\nurl = \"https://example.com/p.git\"\nsource = {{ branch = \"17.0\" }}\nsha = \"{}\"\n",
            "0".repeat(40)
        ),
    )
    .unwrap();
    let used = odx_git::url_hash("https://example.com/p.git");
    std::fs::create_dir_all(machine.cache().join("repos").join(&used)).unwrap();
    std::fs::create_dir_all(machine.cache().join("repos/deadbeef")).unwrap();
    let (exit, out, err) = machine.odx(&a, &["cache", "info"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("last sync: never"), "{out}");
    assert!(out.contains("repositories: 2 (1 unused"), "{out}");
    let (_, out, _) = machine.odx(&a, &["cache", "gc"]);
    assert!(out.starts_with("removed deadbeef ("), "{out}");

    // No cache directory at all.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(
        ["odx", "cache", "info"],
        &OsFs,
        &MapEnv::default(),
        &a,
        &mut out,
        &mut err,
    );
    assert_eq!(exit, Exit::Usage);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("set ODX_CACHE_DIR")
    );
    // A configuration directory that cannot be created.
    let blocked = machine.tmp.path().join("file");
    std::fs::write(&blocked, "").unwrap();
    let env = machine
        .env
        .clone()
        .with("ODX_CONFIG_DIR", blocked.join("odx").display().to_string());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(
        ["odx", "sync", "--register"],
        &OsFs,
        &env,
        &a,
        &mut out,
        &mut err,
    );
    assert_eq!(exit, Exit::Failure);
    assert!(String::from_utf8(err).unwrap().contains("config.toml"));
    let env = MapEnv::default().with("ODX_CACHE_DIR", machine.cache().display().to_string());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(
        ["odx", "sync", "--register"],
        &OsFs,
        &env,
        &a,
        &mut out,
        &mut err,
    );
    assert_eq!(exit, Exit::Usage);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("set ODX_CONFIG_DIR"),
        "{}",
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn workspaces_register_themselves_when_asked() {
    let machine = Machine::new();
    let a = machine.workspace("a");
    std::fs::create_dir_all(machine.tmp.path().join("config")).unwrap();
    std::fs::write(
        machine.tmp.path().join("config/config.toml"),
        "[cache]\nregister_workspaces = true\n",
    )
    .unwrap();
    let (exit, _, err) = machine.odx(&a, &["aggregate", "--fetch-only"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    let config = std::fs::read_to_string(machine.tmp.path().join("config/config.toml")).unwrap();
    assert!(
        config.starts_with("[cache]\nregister_workspaces = true\n\n[workspaces]\n"),
        "{config}"
    );
    assert_eq!(config.matches("series = \"17.0\"").count(), 1, "{config}");
    // Registered once.
    machine.odx(&a, &["aggregate", "--fetch-only"]);
    let again = std::fs::read_to_string(machine.tmp.path().join("config/config.toml")).unwrap();
    assert_eq!(again, config);
    let (_, out, _) = machine.odx(machine.tmp.path(), &["sync", "--all"]);
    assert!(out.starts_with("synced "), "{out}");
}

#[test]
fn sync_prefetches_the_wheels_vendor_installs() {
    use sha2::Digest as _;

    let machine = Machine::new();
    let a = machine.workspace("a");
    let wheel = machine
        .tmp
        .path()
        .join("odoo_addon_x-17.0.1.0.0-py3-none-any.whl");
    std::fs::write(&wheel, "a wheel").unwrap();
    let hash = sha2::Sha256::digest(b"a wheel")
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let pyproject = std::fs::read_to_string(a.join("pyproject.toml")).unwrap();
    let with_groups = pyproject.replacen(
        "odoo_version = \"17.0\"\n",
        "odoo_version = \"17.0\"\n\n[tool.odx.groups]\noca = \"vendor/oca\"\n",
        1,
    );
    std::fs::write(a.join("pyproject.toml"), &with_groups).unwrap();
    // No uv.lock yet: the wheels cannot be planned.
    let (exit, out, _) = machine.odx(&a, &["sync"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(out.contains("; wheels: "), "{out}");
    // One package from an index, one from git (which odx fetches as a
    // repository, not as a wheel).
    let uv_lock = |hash: &str| {
        format!(
            "version = 1\n\n[[package]]\nname = \"a\"\nversion = \"1\"\nsource = {{ virtual = \".\" }}\n\n[package.optional-dependencies]\noca = [{{ name = \"odoo-addon-x\" }}, {{ name = \"odoo-addon-y\" }}]\n\n[[package]]\nname = \"odoo-addon-x\"\nversion = \"17.0.1.0.0\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\nwheels = [{{ url = \"{}\", hash = \"sha256:{hash}\" }}]\n\n[[package]]\nname = \"odoo-addon-y\"\nversion = \"17.0.1.0.0\"\nsource = {{ git = \"https://example.com/y.git#{}\" }}\n",
            file_url(&wheel),
            "0".repeat(40)
        )
    };
    std::fs::write(a.join("uv.lock"), uv_lock("00")).unwrap();
    let (exit, out, _) = machine.odx(&a, &["sync"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(out.contains("; wheels: "), "{out}");
    std::fs::write(a.join("uv.lock"), uv_lock(&hash)).unwrap();
    let (exit, out, _) = machine.odx(&a, &["sync"]);
    assert_eq!(exit, Exit::Success, "{out}");
    assert!(
        out.ends_with("; 1 wheels cached; dashboard of 1 entries\n"),
        "{out}"
    );
    assert!(
        machine
            .cache()
            .join("wheels")
            .join(format!("{hash}.whl"))
            .is_file()
    );
    // Switched off per project.
    std::fs::write(
        a.join("pyproject.toml"),
        with_groups.replacen(
            "[tool.odx.groups]",
            "[tool.odx.cache]\nprefetch_wheels = false\n\n[tool.odx.groups]",
            1,
        ),
    )
    .unwrap();
    let (_, out, _) = machine.odx(&a, &["sync"]);
    assert!(!out.contains("wheels"), "{out}");
}
