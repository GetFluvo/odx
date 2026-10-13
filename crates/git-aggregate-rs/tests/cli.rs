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

#[test]
fn binary_reports_pull_requests_on_no_forge() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("repos.yaml"),
        "./x:\n  remotes:\n    x: /srv/git/x\n  merges:\n    - x 17.0\n    - x refs/pull/1/merge\n    - x refs/pull/y/head\n    - x pull/3/head\n    - x refs/merge-requests/2/head\n  target: x agg\n",
    )
    .unwrap();
    let assert = Command::cargo_bin("git-aggregate-rs")
        .unwrap()
        .current_dir(tmp.path())
        .arg("show-all-prs")
        .assert()
        .success()
        .stdout("");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("refs/merge-requests/2/head: not on a forge"),
        "{stderr}"
    );
}
