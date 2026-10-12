//! End-to-end tests of the `git-aggregate-rs` binary.

use assert_cmd::Command;

#[test]
fn binary_reports_not_implemented_with_usage_exit() {
    Command::cargo_bin("git-aggregate-rs")
        .unwrap()
        .arg("aggregate")
        .assert()
        .code(2)
        .stderr("git-aggregate-rs aggregate: not implemented yet (roadmap phase 3)\n");
}

#[test]
fn binary_prints_help() {
    Command::cargo_bin("git-aggregate-rs")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
}

/// Rendered in-process with a fixed program name, so the snapshot does not
/// depend on the platform's executable suffix.
#[test]
fn help_snapshot() {
    let mut out = Vec::new();
    let exit = git_aggregate_rs::run(["git-aggregate-rs", "--help"], &mut out, &mut Vec::new());
    assert_eq!(exit.code(), 0);
    insta::assert_snapshot!(String::from_utf8(out).unwrap());
}
