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

/// Every leaf command path, e.g. `["port", "pr"]`, with required arguments.
const STUBS: &[(&[&str], u8)] = &[
    (&["dashboard"], 7),
    (&["suggest"], 7),
    (&["verdict", "list"], 7),
    (&["check-deps"], 7),
    (&["upgrade", "--target", "19.0"], 7),
    (&["port", "pr", "OCA/contract#1476", "--to", "19.0"], 8),
    (&["promote", "contract"], 10),
    (&["env", "diff", "19-test", "19-prod"], 10),
    (&["bot", "run"], 11),
    (&["lint"], 9),
];

#[test]
fn stubbed_commands_report_their_phase_with_usage_exit() {
    for (args, phase) in STUBS {
        let mut argv = vec!["odx"];
        argv.extend_from_slice(args);
        let (exit, out, err) = run(&argv);
        let name = args[0];
        assert_eq!(exit, Exit::Usage, "{argv:?}");
        assert_eq!(out, "", "{argv:?}");
        assert_eq!(
            err,
            format!("odx {name}: not implemented yet (roadmap phase {phase})\n"),
        );
    }
}

#[test]
fn git_aggregate_subcommand_uses_the_drop_in() {
    let (exit, _, err) = run(&["odx", "git-aggregate", "show-all-prs", "-c", "repos.yaml"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "odx git-aggregate show-all-prs: not implemented yet (roadmap phase 7)\n"
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
        odx_cli::run(["odx", "suggest"], &mut Broken, &mut Broken),
        Exit::Failure
    );
}

#[test]
fn version_is_printed_to_stdout() {
    let (exit, out, _) = run(&["odx", "--version"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(out, format!("odx {}\n", env!("CARGO_PKG_VERSION")));
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
    Command::cargo_bin("odx")
        .unwrap()
        .arg("suggest")
        .assert()
        .code(2)
        .stderr("odx suggest: not implemented yet (roadmap phase 7)\n");
}
