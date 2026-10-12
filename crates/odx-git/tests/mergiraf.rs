//! mergiraf resolves a conflict git cannot: two methods added at the same
//! place of a Python class, as when two pull requests extend one Odoo model.
//!
//! Skipped (with a message) where mergiraf is not installed locally; on CI
//! (`CI` set) a missing mergiraf fails the test instead.

#![expect(clippy::expect_used, reason = "test helpers should fail loudly")]

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use odx_core::system::OsRunner;
use odx_git::{Aggregation, Merger, Step, StepOutcome};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn mergiraf_resolves_additions_at_the_same_place() {
    let available = Command::new("mergiraf")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !available {
        assert!(
            std::env::var_os("CI").is_none(),
            "mergiraf must be installed on CI"
        );
        eprintln!("skipped: mergiraf is not installed");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let up = tmp.path().join("up");
    fs::create_dir_all(&up).expect("mkdir");
    git(&up, &["init", "--quiet", "--initial-branch=base"]);
    let base = "class SaleOrder(models.Model):\n    _inherit = \"sale.order\"\n\n    def base(self):\n        return 1\n";
    fs::write(up.join("code.py"), base).expect("write");
    git(&up, &["add", "."]);
    git(&up, &["commit", "--quiet", "-m", "base"]);
    for (name, body) in [
        ("c", "c(self):\n        return 3\n"),
        ("d", "d(self):\n        return 4\n"),
    ] {
        git(&up, &["checkout", "--quiet", "-b", name, "base"]);
        fs::write(up.join("code.py"), format!("{base}\n    def {body}")).expect("write");
        git(&up, &["commit", "--quiet", "-am", name]);
    }
    let cache = tmp.path().join("cache.git");
    git(
        tmp.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            up.to_str().expect("utf-8"),
            cache.to_str().expect("utf-8"),
        ],
    );
    let step = |name: &str| Step {
        label: name.to_owned(),
        sha: git(&cache, &["rev-parse", name]),
        skip_on_conflict: false,
    };
    let plan = Aggregation {
        cache_repo: cache.clone(),
        target_dir: tmp.path().join("work"),
        base: git(&cache, &["rev-parse", "base"]),
        steps: vec![step("c"), step("d")],
        mergiraf: Some("mergiraf".into()),
        keep_conflicts: false,
        force: false,
    };
    let result = Merger::new(Arc::new(OsRunner))
        .aggregate(&plan)
        .expect("mergiraf resolves the conflict");
    assert_eq!(result.steps[1].1, StepOutcome::Merged);
    let code = fs::read_to_string(plan.target_dir.join("code.py"))
        .expect("read")
        .replace("\r\n", "\n");
    assert!(
        code.contains("def c(self):") && code.contains("def d(self):") && !code.contains("<<<<<<<"),
        "{code}"
    );
}
