//! End-to-end tests of the `git-aggregate-rs` binary.

use assert_cmd::Command;

#[test]
fn binary_fails_without_a_configuration() {
    let tmp = tempfile::tempdir().unwrap();
    let assert = Command::cargo_bin("git-aggregate-rs")
        .unwrap()
        .current_dir(tmp.path())
        .args(["-c", "missing.yaml"])
        .assert()
        .code(1);
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.starts_with("error: cannot read "), "{stderr}");
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
