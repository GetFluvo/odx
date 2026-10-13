//! `odx daemon`: the scheduled job's files, its installation through the
//! scheduler's programs, and its status.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};

use odx_core::fake::MapEnv;
use odx_core::system::OsFs;
use odx_core::{Env as _, Exit};

fn odx(env: &MapEnv, dir: &Path, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(command_line, &OsFs, env, dir, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

struct Machine {
    tmp: tempfile::TempDir,
    env: MapEnv,
}

impl Machine {
    fn new(scheduler: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::default()
            .with("HOME", tmp.path().join("home").display().to_string())
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            )
            .with("ODX_SCHEDULER", scheduler)
            // A program that fails on every scheduler command.
            .with("ODX_SYSTEMCTL", "odx-test-no-such-program")
            .with("ODX_LAUNCHCTL", "odx-test-no-such-program")
            .with("ODX_SCHTASKS", "odx-test-no-such-program");
        Self { tmp, env }
    }

    /// `relative`, written with `/`, under the machine's directory and
    /// with the platform's separators.
    fn path(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.tmp.path().to_path_buf(), |path, part| path.join(part))
    }

    fn odx(&self, args: &[&str]) -> (Exit, String, String) {
        odx(&self.env, self.tmp.path(), args)
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.path("config/config.toml")).unwrap_or_default()
    }
}

#[test]
fn dry_runs_show_each_scheduler_job() {
    let machine = Machine::new("systemd");
    let (exit, out, err) = machine.odx(&["daemon", "install", "--dry-run"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    let units = machine.path("home/.config/systemd/user");
    assert!(
        out.starts_with(&format!(
            "# {}\n[Unit]",
            units.join("odx-sync.service").display()
        )),
        "{out}"
    );
    assert!(out.contains("OnUnitActiveSec=1800\n"), "{out}");
    assert!(
        out.ends_with("$ systemctl --user enable --now odx-sync.timer\n"),
        "{out}"
    );
    assert!(!units.exists());
    assert_eq!(machine.config(), "");

    let xdg = machine
        .env
        .clone()
        .with("XDG_CONFIG_HOME", machine.path("xdg").display().to_string());
    let (_, out, _) = odx(
        &xdg,
        machine.tmp.path(),
        &["daemon", "install", "--dry-run", "--interval", "2h"],
    );
    assert!(
        out.contains(&machine.path("xdg/systemd/user").display().to_string()),
        "{out}"
    );
    assert!(out.contains("OnUnitActiveSec=7200\n"), "{out}");

    let launchd = machine.env.clone().with("ODX_SCHEDULER", "launchd");
    let (_, out, _) = odx(
        &launchd,
        machine.tmp.path(),
        &["daemon", "install", "--dry-run"],
    );
    assert!(out.contains("Library"), "{out}");
    assert!(
        out.contains("<key>StartInterval</key>\n\t<integer>1800</integer>"),
        "{out}"
    );

    std::fs::create_dir_all(machine.path("config")).unwrap();
    std::fs::write(
        machine.path("config/config.toml"),
        "[sync]\ninterval = \"15m\"\nnice = false\npause_on_battery = true\nmax_duration = \"5m\"\n",
    )
    .unwrap();
    let tasks = machine.env.clone().with("ODX_SCHEDULER", "schtasks");
    let (_, out, _) = odx(
        &tasks,
        machine.tmp.path(),
        &["daemon", "install", "--dry-run"],
    );
    assert!(out.contains("<Interval>PT15M</Interval>"), "{out}");
    assert!(
        out.contains("<ExecutionTimeLimit>PT10M</ExecutionTimeLimit>"),
        "{out}"
    );
    assert!(out.contains("<Priority>5</Priority>"), "{out}");
    assert!(
        out.contains("$ schtasks /Create /F /TN odx-sync /XML "),
        "{out}"
    );
}

#[test]
fn usage_errors() {
    let machine = Machine::new("cron");
    for args in [
        &["daemon", "install"][..],
        &["daemon", "uninstall"],
        &["daemon", "status"],
    ] {
        let (exit, _, err) = machine.odx(args);
        assert_eq!(exit, Exit::Usage, "{args:?}");
        assert!(err.contains("unknown scheduler \"cron\""), "{err}");
    }
    let systemd = machine.env.clone().with("ODX_SCHEDULER", "systemd");
    for command in ["install", "run"] {
        let (exit, _, err) = odx(
            &systemd,
            machine.tmp.path(),
            &["daemon", command, "--interval", "often"],
        );
        assert_eq!(exit, Exit::Usage, "{command}");
        assert!(err.contains("invalid duration"), "{err}");
    }
    // No home and no configuration directory for the job files.
    for scheduler in ["systemd", "launchd", "schtasks"] {
        let bare = MapEnv::default().with("ODX_SCHEDULER", scheduler);
        let (exit, _, err) = odx(&bare, machine.tmp.path(), &["daemon", "install"]);
        assert_eq!(exit, Exit::Usage, "{scheduler}");
        assert!(err.contains("no directory for the job files"), "{err}");
    }
    // No cache to report on.
    let (exit, _, err) = odx(
        &MapEnv::default(),
        machine.tmp.path(),
        &["daemon", "status"],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("set ODX_CACHE_DIR"), "{err}");
    // A user configuration that is not TOML.
    std::fs::create_dir_all(machine.path("config")).unwrap();
    std::fs::write(machine.path("config/config.toml"), "[sync\n").unwrap();
    let (exit, _, _) = odx(&systemd, machine.tmp.path(), &["daemon", "install"]);
    assert_eq!(exit, Exit::Usage);
}

#[test]
fn scheduler_failures() {
    let machine = Machine::new("launchd");
    let (exit, out, _) = machine.odx(&["daemon", "status"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!(
            "job: launchd agent (not installed)\ninterval: 30m\nlast sync: never\ncache: {} (0 B)\n",
            machine.path("cache").display()
        )
    );
    let (_, out, _) = machine.odx(&["daemon", "uninstall"]);
    assert_eq!(out, "background sync is not installed\n");

    // launchctl fails: the files are written, the job is not loaded.
    let (exit, _, err) = machine.odx(&["daemon", "install"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("launchctl load -w"), "{err}");
    let (_, out, _) = machine.odx(&["daemon", "status"]);
    assert!(
        out.starts_with("job: launchd agent (installed, not active)\n"),
        "{out}"
    );
    // With the platform's own launchctl, a harmless query.
    let mut own = MapEnv::default();
    for key in ["HOME", "ODX_CACHE_DIR", "ODX_CONFIG_DIR", "ODX_SCHEDULER"] {
        own = own.with(key, machine.env.var(key).unwrap());
    }
    let (_, out, _) = odx(&own, machine.tmp.path(), &["daemon", "status"]);
    assert!(out.starts_with("job: launchd agent ("), "{out}");
    let (_, out, _) = machine.odx(&["--json", "daemon", "status"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["state"], "installed, not active");

    // Removing the job still works when the scheduler cannot be asked.
    let (exit, out, _) = machine.odx(&["daemon", "uninstall"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "removed the launchd agent\n")
    );
    assert!(
        machine.config().contains("enabled = false"),
        "{}",
        machine.config()
    );
    assert!(
        !machine
            .path("home/Library/LaunchAgents/com.getfluvo.odx.sync.plist")
            .exists()
    );

    // A job file that cannot be removed or written.
    let plist = machine.path("home/Library/LaunchAgents/com.getfluvo.odx.sync.plist");
    std::fs::create_dir_all(plist.join("x")).unwrap();
    let (exit, _, err) = machine.odx(&["daemon", "uninstall"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("com.getfluvo.odx.sync.plist"), "{err}");
    let (exit, _, _) = machine.odx(&["daemon", "install"]);
    assert_eq!(exit, Exit::Failure);
    std::fs::remove_dir_all(&plist).unwrap();

    // A configuration directory that cannot be written.
    let (exit, _, _) = machine.odx(&["daemon", "install"]);
    assert_eq!(exit, Exit::Failure);
    let blocked = machine.path("blocked");
    std::fs::write(&blocked, "").unwrap();
    let env = machine
        .env
        .clone()
        .with("ODX_CONFIG_DIR", blocked.join("odx").display().to_string());
    let (exit, _, err) = odx(&env, machine.tmp.path(), &["daemon", "uninstall"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("config.toml"), "{err}");
}

/// A scheduler program that records its arguments and succeeds, except
/// for the commands listed in `fail`.
#[cfg(unix)]
fn fake_program(dir: &Path, name: &str, fail: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    let log = dir.join(format!("{name}.log"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in\n  {fail}) exit 1 ;;\nesac\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn install_status_and_uninstall_through_systemd() {
    let machine = Machine::new("systemd");
    let systemctl = fake_program(machine.tmp.path(), "systemctl", "never");
    let env = machine
        .env
        .clone()
        .with("ODX_SYSTEMCTL", systemctl.display().to_string());
    let (exit, out, err) = odx(
        &env,
        machine.tmp.path(),
        &["daemon", "install", "--interval", "45m"],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(
        out.starts_with("installed the systemd user timer: `odx sync --all` every 45m\n"),
        "{out}"
    );
    let units = machine.path("home/.config/systemd/user");
    let timer = std::fs::read_to_string(units.join("odx-sync.timer")).unwrap();
    assert!(timer.contains("OnUnitActiveSec=2700\n"), "{timer}");
    assert_eq!(
        machine.config(),
        "[sync]\nenabled = true\ninterval = \"45m\"\n"
    );
    let (_, out, _) = odx(&env, machine.tmp.path(), &["daemon", "status"]);
    assert!(
        out.starts_with("job: systemd user timer (active)\ninterval: 45m\n"),
        "{out}"
    );

    // Doctor sees the installed job.
    std::fs::write(
        machine.path("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\n",
    )
    .unwrap();
    let (_, out, _) = odx(&env, machine.tmp.path(), &["doctor", "--offline"]);
    assert!(
        out.contains("ok   background sync: systemd user timer installed"),
        "{out}"
    );

    let (exit, out, _) = odx(&env, machine.tmp.path(), &["daemon", "uninstall"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "removed the systemd user timer\n")
    );
    assert!(!units.join("odx-sync.timer").exists());
    let log = std::fs::read_to_string(machine.path("systemctl.log")).unwrap();
    assert_eq!(
        log,
        "--user daemon-reload\n--user enable --now odx-sync.timer\n--user is-active --quiet odx-sync.timer\n--user disable --now odx-sync.timer\n"
    );

    // The configuration cannot be written after the job is enabled.
    let blocked = machine.path("blocked");
    std::fs::write(&blocked, "").unwrap();
    let env = env.with("ODX_CONFIG_DIR", blocked.join("odx").display().to_string());
    let (exit, _, err) = odx(&env, machine.tmp.path(), &["daemon", "install"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("config.toml"), "{err}");
}
