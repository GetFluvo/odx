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
            strategy: Strategy::Merge,
            mergiraf: None,
            keep_conflicts: false,
            force: false,
        }
    }

    fn rebase(&self, steps: &[(&str, bool)]) -> Aggregation {
        Aggregation {
            strategy: Strategy::Rebase,
            ..self.plan(steps)
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
        fs::read_to_string(work.join("text.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
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
fn rebases_keep_history_linear() {
    let fx = fixture();
    let plan = fx.rebase(&[
        ("a", false),
        ("clean", false),
        ("base", false),
        ("a", false),
    ]);
    let result = merger().aggregate(&plan).unwrap();
    assert_eq!(
        result.steps,
        [
            ("a".to_owned(), StepOutcome::Merged),
            ("clean".to_owned(), StepOutcome::Merged),
            ("base".to_owned(), StepOutcome::UpToDate),
            ("a".to_owned(), StepOutcome::UpToDate),
        ]
    );
    let work = &plan.target_dir;
    assert_eq!(git(work, &["rev-list", "--merges", "HEAD"]), "");
    assert_eq!(git(work, &["log", "--format=%s", "-2"]), "clean\na");
    assert!(work.join("new.txt").exists());
    // The trees match the merge strategy's.
    let merged = merger()
        .aggregate(&Aggregation {
            target_dir: fx.root.join("work").join("merged"),
            ..fx.plan(&[("a", false), ("clean", false)])
        })
        .unwrap();
    assert_eq!(merged.tree, result.tree);
}

#[test]
fn rebase_conflicts_return_to_the_result_so_far() {
    let fx = fixture();
    let plan = fx.rebase(&[("a", false), ("b", false)]);
    let err = merger().aggregate(&plan).unwrap_err();
    assert_eq!(err.to_string(), "b conflicts in text.txt");
    let work = &plan.target_dir;
    assert_eq!(git(work, &["status", "--porcelain"]), "");
    // The repository was built again with merge commits, which conflict too.
    assert_eq!(git(work, &["log", "-1", "--format=%s"]), "Merge a");

    let skipping = fx.rebase(&[("a", false), ("b", true), ("clean", false)]);
    let result = merger().aggregate(&skipping).unwrap();
    assert_eq!(
        result.steps[1].1,
        StepOutcome::Skipped {
            files: vec!["text.txt".to_owned()]
        }
    );
    assert_eq!(result.steps[2].1, StepOutcome::Merged);
    assert_eq!(git(work, &["log", "--format=%s", "-2"]), "clean\na");
}

#[test]
fn kept_rebase_conflicts_are_cleared_with_force() {
    let fx = fixture();
    let plan = Aggregation {
        keep_conflicts: true,
        ..fx.rebase(&[("a", false), ("b", false)])
    };
    let err = merger().aggregate(&plan).unwrap_err();
    assert_eq!(err.to_string(), "b conflicts in text.txt (left in place)");
    let forced = Aggregation {
        force: true,
        ..fx.rebase(&[("a", false), ("clean", false)])
    };
    let result = merger().aggregate(&forced).unwrap();
    assert_eq!(result.steps[1].1, StepOutcome::Merged);
    assert_eq!(
        git(&plan.target_dir, &["log", "-1", "--format=%s"]),
        "clean"
    );
}

#[test]
fn strategy_commands() {
    assert_eq!(Strategy::default(), Strategy::Rebase);
    assert_eq!(Strategy::Rebase.command(), "rebase");
    assert_eq!(Strategy::Merge.command(), "merge");
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
fn an_empty_directory_becomes_a_worktree() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false)]);
    fs::create_dir_all(&plan.target_dir).unwrap();
    merger().aggregate(&plan).unwrap();
    // Windows runners check out with autocrlf.
    assert_eq!(
        fs::read_to_string(plan.target_dir.join("text.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "one\nA\nthree\n"
    );
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
fn rebase_git_call_failures_are_reported() {
    for (needle, steps) in [
        ("merge-base", &[("a", false)][..]),
        ("checkout", &[("a", false), ("b", false)][..]),
    ] {
        let fx = fixture();
        let err = FailOn::merger(needle, 1)
            .aggregate(&fx.rebase(steps))
            .unwrap_err();
        assert!(
            err.to_string().contains("injected failure"),
            "{needle}: {err}"
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

#[test]
fn push_forces_the_result_to_a_branch() {
    let fx = fixture();
    let plan = fx.plan(&[("a", false)]);
    let result = merger().aggregate(&plan).unwrap();
    let remote = fx.root.join("remote.git");
    git(
        &fx.root,
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    merger()
        .push(&plan.target_dir, remote.to_str().unwrap(), "17.0-agg")
        .unwrap();
    assert_eq!(git(&remote, &["rev-parse", "17.0-agg"]), result.head);
    assert!(
        merger()
            .push(&plan.target_dir, "/nonexistent/remote.git", "x")
            .is_err()
    );
}

#[test]
fn outcome_statuses() {
    let skipped = StepOutcome::Skipped { files: Vec::new() };
    assert_eq!(
        [
            StepOutcome::Merged.status(),
            StepOutcome::UpToDate.status(),
            skipped.status()
        ],
        ["merged", "up-to-date", "skipped"]
    );
}

#[test]
fn describe_counts_outcomes() {
    let built = |steps: Vec<(&str, StepOutcome)>| Aggregated {
        head: String::new(),
        tree: String::new(),
        steps: steps.into_iter().map(|(l, o)| (l.to_owned(), o)).collect(),
    };
    assert_eq!(built(vec![]).describe(), "base only");
    assert_eq!(
        built(vec![
            ("#1", StepOutcome::Merged),
            ("#2", StepOutcome::Merged),
            ("#3", StepOutcome::UpToDate),
            (
                "#4",
                StepOutcome::Skipped {
                    files: vec!["a.xml".into(), "b.py".into()]
                }
            ),
        ])
        .describe(),
        "2 merged, 1 up to date, 1 skipped: #4 conflicts in a.xml, b.py"
    );
}

#[test]
fn trees_and_patch_ids() {
    let fx = fixture();
    let m = merger();
    let a = fx.sha("a");
    assert_eq!(
        m.tree(&fx.cache, &a).unwrap(),
        git(&fx.cache, &["rev-parse", "a^{tree}"])
    );
    let id = m.patch_id(&fx.cache, &fx.base, &a).unwrap();
    assert_eq!(id.len(), 40);
    assert_ne!(id, "0".repeat(40));
    // The same change on another base keeps its id.
    git(&fx.cache, &["config", "user.email", "x@example.com"]);
    git(&fx.cache, &["config", "user.name", "x"]);
    let work = fx.root.join("rebased");
    git(
        &fx.cache,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            work.to_str().unwrap(),
            "clean",
        ],
    );
    git(&work, &["cherry-pick", "a"]);
    let picked = git(&work, &["rev-parse", "HEAD"]);
    assert_eq!(
        m.patch_id(&fx.cache, &fx.sha("clean"), &picked).unwrap(),
        id
    );
    // Contained in the base: no changes.
    assert_eq!(m.patch_id(&fx.cache, &a, &fx.base).unwrap(), "0".repeat(40));
    assert!(m.patch_id(&fx.cache, &fx.base, &"0".repeat(40)).is_err());
    assert!(m.tree(&fx.cache, "nope").is_err());
    for needle in ["diff", "patch-id"] {
        assert!(
            FailOn::merger(needle, 1)
                .patch_id(&fx.cache, &fx.base, &a)
                .unwrap_err()
                .to_string()
                .contains("injected failure"),
            "{needle}"
        );
    }
}

#[test]
fn current_worktrees_need_no_build() {
    let fx = fixture();
    let plan = fx.rebase(&[("a", false)]);
    let m = merger();
    assert!(!m.is_current(&plan.target_dir, &fx.cache, "x"));
    let built = m.aggregate(&plan).unwrap();
    assert!(m.is_current(&plan.target_dir, &fx.cache, &built.tree));
    assert!(!m.is_current(&plan.target_dir, &fx.cache, &fx.base));
    fs::write(plan.target_dir.join("text.txt"), "edited").unwrap();
    assert!(!m.is_current(&plan.target_dir, &fx.cache, &built.tree));
    assert!(!FailOn::merger("--porcelain", 1).is_current(&plan.target_dir, &fx.cache, &built.tree));
}

#[test]
fn changed_and_present_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "--quiet", "--initial-branch=main"]);
    for dir in ["pos_a", "pos_b"] {
        fs::create_dir_all(repo.join(dir)).unwrap();
        fs::write(repo.join(dir).join("x.py"), dir).unwrap();
    }
    fs::write(repo.join("README.md"), "r").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", "base"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("pos_b").join("x.py"), "changed").unwrap();
    fs::write(repo.join("pos_b").join("y.py"), "new").unwrap();
    fs::write(repo.join("README.md"), "changed").unwrap();
    git(repo, &["commit", "--quiet", "-am", "change"]);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "--quiet", "-m", "add"]);
    let head = git(repo, &["rev-parse", "HEAD"]);
    let m = merger();
    assert_eq!(m.changed_dirs(repo, &base, &head).unwrap(), ["pos_b"]);
    assert_eq!(m.dirs(repo, &base).unwrap(), ["pos_a", "pos_b"]);
    assert!(m.changed_dirs(repo, &base, &"0".repeat(40)).is_err());
    assert!(m.dirs(repo, "nope").is_err());
    assert!(
        FailOn::merger("--name-only", 1)
            .changed_dirs(repo, &base, &head)
            .is_err()
    );

    let base_tree = m.tree(repo, &base).unwrap();
    assert_eq!(
        m.changed_files(repo, &base_tree, &head, None).unwrap(),
        [
            ('M', "README.md".to_owned()),
            ('M', "pos_b/x.py".to_owned()),
            ('A', "pos_b/y.py".to_owned())
        ]
    );
    assert_eq!(
        m.changed_files(repo, &base, &head, Some("pos_b/y.py"))
            .unwrap(),
        [('A', "pos_b/y.py".to_owned())]
    );
    assert_eq!(m.changed_files(repo, &head, &head, None).unwrap(), []);
    assert!(m.changed_files(repo, "nope", &head, None).is_err());
    assert_eq!(m.show(repo, &head, "pos_b/y.py").as_deref(), Some("new"));
    assert_eq!(m.show(repo, &base, "pos_b/y.py"), None);
    assert_eq!(
        m.object(repo, &format!("{head}:pos_b")).unwrap(),
        git(repo, &["rev-parse", &format!("{head}:pos_b")])
    );
    assert!(m.object(repo, &format!("{head}:nope")).is_err());
    let zip = m.archive(repo, &head, Some("pos_b")).unwrap();
    assert!(zip.starts_with(b"PK"));
    assert!(m.archive(repo, &head, None).unwrap().len() > zip.len());
    assert!(m.archive(repo, &"0".repeat(40), None).is_err());
}

/// A pull request whose history merges two branches that both changed the
/// same line: rebasing linearizes the merge and conflicts, a merge commit
/// applies the resolved result.
#[test]
#[expect(clippy::too_many_lines, reason = "one history, every fallback")]
fn steps_that_cannot_be_rebased_are_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let up = tmp.path().join("up");
    fs::create_dir_all(&up).unwrap();
    git(&up, &["init", "--quiet", "--initial-branch=base"]);
    fs::write(up.join("f.txt"), "one\n").unwrap();
    git(&up, &["add", "f.txt"]);
    git(&up, &["commit", "--quiet", "-m", "base"]);
    let base = git(&up, &["rev-parse", "HEAD"]);
    git(&up, &["checkout", "--quiet", "-b", "x"]);
    fs::write(up.join("f.txt"), "x\n").unwrap();
    git(&up, &["commit", "--quiet", "-am", "x"]);
    git(&up, &["checkout", "--quiet", "-b", "pr", "base"]);
    fs::write(up.join("f.txt"), "y\n").unwrap();
    git(&up, &["commit", "--quiet", "-am", "y"]);
    // The merge stops on the conflict; the commit below concludes it.
    let _ = Command::new("git")
        .current_dir(&up)
        .args(["merge", "--quiet", "x"])
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output();
    assert_eq!(
        git(&up, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]).len(),
        40
    );
    fs::write(up.join("f.txt"), "xy\n").unwrap();
    git(&up, &["commit", "--quiet", "-am", "resolved"]);
    git(&up, &["checkout", "--quiet", "-b", "other", "base"]);
    fs::write(up.join("g.txt"), "g\n").unwrap();
    git(&up, &["add", "g.txt"]);
    git(&up, &["commit", "--quiet", "-m", "other"]);
    let cache = tmp.path().join("cache.git");
    git(
        tmp.path(),
        &[
            "clone",
            "--quiet",
            "--bare",
            up.to_str().unwrap(),
            cache.to_str().unwrap(),
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
        base,
        steps: vec![step("other"), step("pr")],
        strategy: Strategy::Rebase,
        mergiraf: None,
        keep_conflicts: false,
        force: false,
    };
    let built = merger().aggregate(&plan).unwrap();
    assert_eq!(built.steps[0].1, StepOutcome::Merged);
    assert_eq!(built.steps[1].1, StepOutcome::MergeCommit);

    // After `x` is rebased, `pr` (which contains the original `x`) conflicts
    // even as a merge: the repository is built again with merge commits.
    let both = Aggregation {
        target_dir: tmp.path().join("both"),
        steps: vec![step("other"), step("x"), step("pr")],
        ..plan.clone()
    };
    let rebuilt = merger().aggregate(&both).unwrap();
    assert_eq!(
        rebuilt
            .steps
            .iter()
            .map(|(_, o)| o.clone())
            .collect::<Vec<_>>(),
        [
            StepOutcome::MergeCommit,
            StepOutcome::MergeCommit,
            StepOutcome::MergeCommit
        ]
    );
    assert_eq!(
        fs::read_to_string(both.target_dir.join("f.txt"))
            .unwrap()
            .trim(),
        "xy"
    );
    // Kept conflicts are for inspection: no second build.
    let kept = Aggregation {
        target_dir: tmp.path().join("kept"),
        keep_conflicts: true,
        ..both
    };
    assert!(merger().aggregate(&kept).is_err());
    assert_eq!(StepOutcome::MergeCommit.status(), "merge-commit");
    assert_eq!(
        fs::read_to_string(plan.target_dir.join("f.txt"))
            .unwrap()
            .trim(),
        "xy"
    );
    assert_eq!(
        built.describe(),
        "1 merged, 1 merged with a merge commit (could not be rebased): pr"
    );

    // A failing abort while falling back is reported.
    let fresh = Aggregation {
        target_dir: tmp.path().join("work2"),
        ..plan
    };
    let err = FailOn::merger("--abort", 1).aggregate(&fresh).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");
    let fresh = Aggregation {
        target_dir: tmp.path().join("work3"),
        ..fresh
    };
    let err = FailOn::merger("--no-ff", 1).aggregate(&fresh).unwrap_err();
    assert!(err.to_string().contains("injected failure"), "{err}");
}

#[test]
fn commits_ancestry_counts_and_modifications() {
    let fx = fixture();
    let m = merger();
    let a = fx.sha("a");
    assert_eq!(m.commit(&fx.cache, "a").as_deref(), Some(a.as_str()));
    assert_eq!(m.commit(&fx.cache, "nope"), None);
    assert!(m.contains(&fx.cache, &a, &fx.base));
    assert!(!m.contains(&fx.cache, &fx.base, &a));
    assert!(!m.contains(&fx.cache, &a, &"0".repeat(40)));
    assert_eq!(m.count(&fx.cache, &fx.base, &a), Some(1));
    assert_eq!(m.count(&fx.cache, &fx.base, "nope"), None);
    assert_eq!(m.modified(&fx.root.join("nowhere")), Vec::<String>::new());
    m.aggregate(&fx.plan(&[("a", false)])).unwrap();
    let work = fx.root.join("work").join("pos");
    assert_eq!(m.modified(&work), Vec::<String>::new());
    fs::write(work.join("text.txt"), "mine\n").unwrap();
    assert_eq!(m.modified(&work), [" M text.txt"]);
}
