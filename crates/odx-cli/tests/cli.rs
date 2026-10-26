//! End-to-end tests of the `odx` binary and its command tree.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use assert_cmd::Command;
use clap::CommandFactory;
use odx_cli::cli::Cli;
use odx_core::Exit;

fn run(args: &[&str]) -> (Exit, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = odx_cli::run(args, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn git_aggregate_subcommand_uses_the_drop_in() {
    let (exit, _, err) = run(&["odx", "git-aggregate", "show-all-prs", "-c", "nowhere.yaml"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.starts_with("error: cannot read "), "{err}");
    // Merges that are no pull requests, and one on no forge.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("repos.yaml"),
        "./x:\n  remotes:\n    x: /srv/git/x\n  merges:\n    - x 17.0\n    - x refs/pull/1/merge\n    - x refs/pull/y/head\n    - x refs/merge-requests/2/head\n  target: x agg\n",
    )
    .unwrap();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(
        ["odx", "git-aggregate", "show-closed-prs"],
        &odx_core::system::OsFs,
        &odx_core::fake::MapEnv::default(),
        tmp.path(),
        &mut out,
        &mut err,
    );
    assert_eq!(exit, Exit::Success);
    assert_eq!(String::from_utf8(out).unwrap(), "");
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("refs/merge-requests/2/head: not on a forge"),
    );
}

#[test]
fn early_dispatched_commands_have_names_and_phases() {
    use clap::Parser;
    for (args, name, phase) in [
        (&["odx", "completions", "bash"][..], "completions", 0),
        (&["odx", "git-aggregate"][..], "git-aggregate", 3),
        (&["odx", "import"][..], "import", 1),
        (&["odx", "export"][..], "export", 1),
        (&["odx", "aggregate", "--fetch-only"][..], "aggregate", 3),
        (&["odx", "add", "OCA/pos#1"][..], "add", 1),
        (&["odx", "drop", "OCA/pos#1"][..], "drop", 1),
        (&["odx", "lock"][..], "lock", 4),
        (&["odx", "verify"][..], "verify", 4),
        (&["odx", "stats"][..], "stats", 5),
        (&["odx", "sniff"][..], "sniff", 5),
        (&["odx", "vendor"][..], "vendor", 5),
        (&["odx", "build"][..], "build", 5),
        (&["odx", "dev"][..], "dev", 6),
        (&["odx", "doctor"][..], "doctor", 6),
        (&["odx", "auth", "status"][..], "auth", 6),
        (&["odx", "sync"][..], "sync", 6),
        (&["odx", "cache", "info"][..], "cache", 6),
        (&["odx", "daemon", "status"][..], "daemon", 6),
        (&["odx", "track"][..], "status", 7),
        (&["odx", "upgrade"][..], "upgrade", 7),
        (&["odx", "check-deps"][..], "check-deps", 7),
        (&["odx", "suggest"][..], "suggest", 7),
        (&["odx", "verdict", "sync"][..], "verdict", 7),
        (&["odx", "dashboard"][..], "dashboard", 7),
        (&["odx", "migrate", "--abort"][..], "migrate", 8),
        (&["odx", "port", "--abort"][..], "port", 8),
        (&["odx", "lint"][..], "lint", 9),
        (&["odx", "env", "list"][..], "env", 10),
        (&["odx", "promote", "a", "--to", "prod"][..], "promote", 10),
        (&["odx", "bot", "plan"][..], "bot", 11),
        (&["odx", "changelog"][..], "changelog", 12),
        (&["odx", "diff"][..], "diff", 12),
        (&["odx", "glue"][..], "glue", 12),
        (&["odx", "wanted"][..], "wanted", 12),
    ] {
        let cli = Cli::try_parse_from(args).unwrap();
        assert_eq!((cli.command.name(), cli.command.phase()), (name, phase));
    }
}

#[test]
fn completions_are_generated_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let (exit, out, err) = run(&["odx", "completions", shell]);
        assert_eq!(exit, Exit::Success, "{shell}");
        assert!(out.contains("odx"), "{shell}");
        assert_eq!(err, "", "{shell}");
    }
}

#[test]
fn no_arguments_prints_help_as_usage_error() {
    let (exit, out, err) = run(&["odx"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(out, "");
    assert!(err.contains("Usage: odx"));
}

#[test]
fn conflicting_global_flags_are_rejected() {
    let (exit, _, err) = run(&["odx", "--fresh", "--offline", "status"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("--fresh"));
}

#[test]
fn write_failure_maps_to_failure() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(
        odx_cli::run(["odx", "bot", "run"], &mut Broken, &mut Broken),
        Exit::Failure
    );
}

#[test]
fn version_is_printed_to_stdout() {
    let (exit, out, _) = run(&["odx", "--version"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        format!(
            "odx {} (odoo-lint {})\n",
            env!("CARGO_PKG_VERSION"),
            odx_lint::odoo_lint_version!()
        )
    );
}

/// Snapshot `--help` of the root and every (nested) subcommand.
#[test]
fn help_snapshots() {
    fn walk(cmd: &mut clap::Command, path: &mut Vec<String>) {
        let name = if path.is_empty() {
            "odx".to_owned()
        } else {
            format!("odx-{}", path.join("-"))
        };
        insta::assert_snapshot!(name, cmd.render_long_help().to_string());
        for sub in cmd.get_subcommands_mut().filter(|s| s.get_name() != "help") {
            path.push(sub.get_name().to_owned());
            walk(sub, path);
            path.pop();
        }
    }
    let mut cmd = Cli::command();
    cmd.build();
    walk(&mut cmd, &mut Vec::new());
}

#[test]
fn binary_exit_codes() {
    Command::cargo_bin("odx")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
    // The bot reads its requests from the dashboard issue, nowhere else.
    Command::cargo_bin("odx")
        .unwrap()
        .args(["bot", "act"])
        .assert()
        .code(2);
}
