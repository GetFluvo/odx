use std::process::Command;

use odx_core::system::OsRunner;
use odx_core::{Invocation, Output, ProcessRunner};

use super::*;

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
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Commits on top of the base, by branch name.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
    base: String,
}

impl Fixture {
    fn sha(&self, branch: &str) -> String {
        git(&self.cache, &["rev-parse", branch])
    }

    fn plan(&self, steps: &[(&str, bool)]) -> Aggregation {
        Aggregation {
            cache_repo: self.cache.clone(),
            target_dir: self.root.join("work").join("pos"),
            base: self.base.clone(),
            steps: steps
                .iter()
                .map(|(branch, skip)| Step {
                    label: (*branch).to_owned(),
                    sha: self.sha(branch),
                    skip_on_conflict: *skip,
                })
                .collect(),
            mergiraf: None,
            keep_conflicts: false,
            force: false,
        }
    }
}

const CODE: &str = "def base():\n    return 1\n";

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let up = root.join("up");
    fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=base"]);
    fs::write(up.join("text.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(up.join("code.py"), CODE).unwrap();
    fs::write(up.join("logo.bin"), [0_u8, 1, 2, 3, 0, 255]).unwrap();
    fs::write(up.join("gone.txt"), "keep me\n").unwrap();
    git(&up, &["add", "."]);
    git(&up, &["commit", "--quiet", "-m", "base"]);
    let branch = |name: &str, change: &dyn Fn(&Path) -> std::io::Result<()>| {
        git(&up, &["checkout", "--quiet", "-b", name, "base"]);
        change(&up).unwrap();
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", name]);
    };
    branch("a", &|d| fs::write(d.join("text.txt"), "one\nA\nthree\n"));
    branch("b", &|d| fs::write(d.join("text.txt"), "one\nB\nthree\n"));
    branch("clean", &|d| fs::write(d.join("new.txt"), "new\n"));
    branch("c", &|d| {
        fs::write(
            d.join("code.py"),
            format!("{CODE}\n\ndef c():\n    return 3\n"),
        )
    });
    branch("d", &|d| {
        fs::write(
            d.join("code.py"),
            format!("{CODE}\n\ndef d():\n    return 4\n"),
        )
    });
    branch("e", &|d| fs::write(d.join("logo.bin"), [0_u8, 9, 9, 0]));
    branch("f", &|d| fs::write(d.join("logo.bin"), [0_u8, 8, 8, 0]));
    branch("g", &|d| fs::remove_file(d.join("gone.txt")));
    branch("h", &|d| fs::write(d.join("gone.txt"), "changed\n"));
    let cache = root.join("cache.git");
    git(
        &root,
        &[
            "clone",
            "--quiet",
            "--bare",
            up.to_str().unwrap(),
            cache.to_str().unwrap(),
        ],
    );
    let base = git(&cache, &["rev-parse", "base"]);
    Fixture {
        _tmp: tmp,
        root,
        cache,
        base,
    }
}

fn merger() -> Merger {
    Merger::new(Arc::new(OsRunner))
}

#[test]
fn clean_merges_build_a_detached_worktree() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false), ("clean", false), ("base", false)]);
    let result = merger().aggregate(&plan).unwrap();
    assert_eq!(
        result.steps,
        [
            ("a".to_owned(), StepOutcome::Merged),
            ("clean".to_owned(), StepOutcome::Merged),
            ("base".to_owned(), StepOutcome::UpToDate),
        ]
    );
    let work = &plan.target_dir;
    assert_eq!(
        fs::read_to_string(work.join("text.txt")).unwrap(),
        "one\nA\nthree\n"
    );
    assert_eq!(git(work, &["rev-parse", "HEAD"]), result.head);
    assert_eq!(
        git(work, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD",
        "detached"
    );
    assert_eq!(git(work, &["log", "-1", "--format=%s"]), "Merge clean");

    // Running again resets the worktree and yields the same tree.
    fs::write(work.join("untracked.txt"), "x").unwrap();
    let forced = Aggregation {
        force: true,
        ..plan.clone()
    };
    let again = merger().aggregate(&forced).unwrap();
    assert_eq!(again.tree, result.tree);
    assert!(!work.join("untracked.txt").exists());
    // A clean worktree needs no force.
    assert_eq!(merger().aggregate(&plan).unwrap().tree, result.tree);
}

#[test]
fn textual_conflicts_abort_cleanly() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false), ("b", false)]);
    let err = merger().aggregate(&plan).unwrap_err();
    assert_eq!(err.to_string(), "b conflicts in text.txt");
    let work = &plan.target_dir;
    assert_eq!(git(work, &["status", "--porcelain"]), "");
    assert!(!work.join(".git").is_dir(), "a worktree has a .git file");
}

#[test]
fn skipped_conflicts_continue() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false), ("b", true), ("clean", false)]);
    let result = merger().aggregate(&plan).unwrap();
    assert_eq!(
        result.steps[1].1,
        StepOutcome::Skipped {
            files: vec!["text.txt".to_owned()]
        }
    );
    assert_eq!(result.steps[2].1, StepOutcome::Merged);
}

#[test]
fn binary_and_delete_modify_conflicts() {
    let fx = fixture();
    let err = merger()
        .aggregate(&fx.plan(&[("e", false), ("f", false)]))
        .unwrap_err();
    assert_eq!(err.to_string(), "f conflicts in logo.bin");
    let err = merger()
        .aggregate(&fx.plan(&[("g", false), ("h", false)]))
        .unwrap_err();
    assert_eq!(err.to_string(), "h conflicts in gone.txt");
}

#[test]
fn kept_conflicts_need_force_afterwards() {
    let fx = fixture();
    let plan = Aggregation {
        keep_conflicts: true,
        ..fx.plan(&[("a", false), ("b", false)])
    };
    let err = merger().aggregate(&plan).unwrap_err();
    assert_eq!(err.to_string(), "b conflicts in text.txt (left in place)");
    let git_dir = git(&plan.target_dir, &["rev-parse", "--git-dir"]);
    assert!(Path::new(&git_dir).join("MERGE_HEAD").exists());
    let again = merger().aggregate(&fx.plan(&[("a", false)])).unwrap_err();
    assert!(
        again
            .to_string()
            .ends_with("has local changes; commit or discard them, or use --force"),
        "{again}"
    );
    let forced = Aggregation {
        force: true,
        ..fx.plan(&[("a", false)])
    };
    assert!(merger().aggregate(&forced).is_ok());
}

#[test]
fn foreign_directories_are_replaced_only_with_force() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false)]);
    fs::create_dir_all(&plan.target_dir).unwrap();
    fs::write(plan.target_dir.join("old.txt"), "from git-aggregator").unwrap();
    let err = merger().aggregate(&plan).unwrap_err();
    assert!(
        err.to_string().ends_with(
            "exists and is not managed by odx; move it away, or use --force to replace it"
        )
    );
    let forced = Aggregation {
        force: true,
        ..plan
    };
    merger().aggregate(&forced).unwrap();
    assert!(!forced.target_dir.join("old.txt").exists());
}

#[test]
fn mergiraf_is_registered_as_merge_driver() {
    let fx = fixture();
    // A shell snippet stands in for mergiraf: the driver runs and reports a
    // conflict; the real resolution is tested in tests/mergiraf.rs.
    let plan = Aggregation {
        mergiraf: Some("exit 1 #".into()),
        ..fx.plan(&[("c", false), ("d", false)])
    };
    let err = merger().aggregate(&plan).unwrap_err();
    assert_eq!(err.to_string(), "d conflicts in code.py");
    assert_eq!(
        fs::read_to_string(fx.cache.join("odx-mergiraf.attributes")).unwrap(),
        "* merge=mergiraf\n"
    );
}

#[test]
fn configured_identity_is_used() {
    let fx = fixture();
    git(&fx.cache, &["config", "user.email", "dev@example.com"]);
    git(&fx.cache, &["config", "user.name", "Dev"]);
    let plan = fx.plan(&[("a", false)]);
    merger().aggregate(&plan).unwrap();
    assert_eq!(
        git(&plan.target_dir, &["log", "-1", "--format=%ae"]),
        "dev@example.com"
    );
}

#[test]
fn git_failures() {
    let fx = fixture();
    let mut plan = fx.plan(&[]);
    plan.steps.push(Step {
        label: "ghost".into(),
        sha: "0".repeat(40),
        skip_on_conflict: false,
    });
    let err = merger().aggregate(&plan).unwrap_err();
    assert!(err.to_string().starts_with("git merge failed: "), "{err}");

    let missing_cache = Aggregation {
        cache_repo: fx.root.join("nope"),
        ..fx.plan(&[])
    };
    assert!(
        merger()
            .aggregate(&missing_cache)
            .unwrap_err()
            .to_string()
            .starts_with("git worktree failed")
    );

    let bad_base = Aggregation {
        base: "0".repeat(40),
        target_dir: fx.root.join("work").join("fresh"),
        ..fx.plan(&[])
    };
    assert!(
        merger()
            .aggregate(&bad_base)
            .unwrap_err()
            .to_string()
            .starts_with("git worktree failed")
    );

    // A worktree that cannot be checked out at the new base.
    let plan = fx.plan(&[]);
    merger().aggregate(&plan).unwrap();
    let bad = Aggregation {
        base: "0".repeat(40),
        ..plan
    };
    assert!(
        merger()
            .aggregate(&bad)
            .unwrap_err()
            .to_string()
            .starts_with("git checkout failed")
    );
}

#[test]
fn a_file_in_the_way_of_the_attributes_is_an_error() {
    let fx = fixture();
    fs::create_dir_all(fx.cache.join("odx-mergiraf.attributes")).unwrap();
    let plan = Aggregation {
        mergiraf: Some("exit 1 #".into()),
        ..fx.plan(&[("a", false)])
    };
    assert!(
        merger()
            .aggregate(&plan)
            .unwrap_err()
            .to_string()
            .starts_with("cannot run git")
    );
}

#[test]
fn merge_commits_get_an_identity_when_git_has_none() {
    let fx = fixture();
    let empty = fx.root.join("empty.gitconfig");
    fs::write(&empty, "").unwrap();
    let hermetic = merger()
        .env("GIT_CONFIG_GLOBAL", &empty)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let plan = fx.plan(&[("a", false)]);
    hermetic.aggregate(&plan).unwrap();
    assert_eq!(
        git(&plan.target_dir, &["log", "-1", "--format=%an <%ae>"]),
        "odx <odx@localhost>"
    );
}

/// Runs git for real, but fails the `nth` (1-based) invocation whose
/// arguments contain `needle`.
#[derive(Debug)]
struct FailOn {
    needle: &'static str,
    nth: usize,
    seen: std::sync::atomic::AtomicUsize,
}

impl FailOn {
    fn merger(needle: &'static str, nth: usize) -> Merger {
        Merger::new(Arc::new(Self {
            needle,
            nth,
            seen: std::sync::atomic::AtomicUsize::new(0),
        }))
    }
}

impl ProcessRunner for FailOn {
    fn run(&self, invocation: &Invocation) -> std::io::Result<Output> {
        if invocation
            .arguments()
            .iter()
            .any(|a| a.to_string_lossy() == self.needle)
            && self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == self.nth
        {
            return Err(std::io::Error::other(format!(
                "injected failure of {}",
                self.needle
            )));
        }
        OsRunner.run(invocation)
    }
}

#[test]
fn every_git_call_failure_is_reported() {
    for (needle, nth, steps) in [
        ("config", 1, &[("a", false)][..]),
        ("rev-parse", 1, &[("a", false)][..]),
        ("rev-parse", 2, &[("a", false)][..]),
        ("rev-parse", 1, &[][..]),
        ("HEAD^{tree}", 1, &[][..]),
        ("--no-ff", 1, &[("a", false)][..]),
        ("--diff-filter=U", 1, &[("a", false), ("b", false)][..]),
        ("--abort", 1, &[("a", false), ("b", false)][..]),
    ] {
        let fx = fixture();
        let err = FailOn::merger(needle, nth)
            .aggregate(&fx.plan(steps))
            .unwrap_err();
        assert!(
            err.to_string().contains("injected failure"),
            "{needle} #{nth}: {err}"
        );
    }
}

#[test]
fn failures_while_resetting_an_existing_worktree() {
    for needle in ["--porcelain", "--hard", "-fdxq"] {
        let fx = fixture();
        let plan = fx.plan(&[]);
        merger().aggregate(&plan).unwrap();
        let err = FailOn::merger(needle, 1).aggregate(&plan).unwrap_err();
        assert!(
            err.to_string().contains("injected failure"),
            "{needle}: {err}"
        );
    }
}

#[test]
fn a_file_in_place_of_the_target_cannot_be_replaced() {
    let fx = fixture();
    let plan = Aggregation {
        force: true,
        ..fx.plan(&[])
    };
    fs::create_dir_all(plan.target_dir.parent().unwrap()).unwrap();
    fs::write(&plan.target_dir, "a file, not a directory").unwrap();
    assert!(
        merger()
            .aggregate(&plan)
            .unwrap_err()
            .to_string()
            .starts_with("cannot run git")
    );
}
