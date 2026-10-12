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
fn help_snapshot() {
    let output = Command::cargo_bin("git-aggregate-rs")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    insta::assert_snapshot!(String::from_utf8(output.stdout).unwrap());
}
