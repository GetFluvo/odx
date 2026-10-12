//! `git-aggregate-rs aggregate` against real upstream repositories.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use git_aggregate_rs::{Context, run_with};
use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

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

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn commit(dir: &Path, file: &str, content: &str) {
    fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "--quiet", "-m", file]);
}

/// A workspace with an upstream (branch 17.0, pull request 1, tag v1) and
/// a fork (branch fix), and a scratch directory to run in.
struct Workspace {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    upstream: String,
    fork: String,
    env: MapEnv,
}

impl Workspace {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let up = root.join("up");
        fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        commit(&up, "a.txt", "a\n");
        git(&up, &["tag", "v1"]);
        commit(&up, "pr.txt", "pr\n");
        let pr = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/1/head", &pr]);
        git(&up, &["reset", "--quiet", "--hard", "HEAD~1"]);
        commit(&up, "b.txt", "b\n");
        let fork = root.join("fork");
        git(&root, &["clone", "--quiet", &file_url(&up), "fork"]);
        git(&fork, &["checkout", "--quiet", "-b", "fix"]);
        commit(&fork, "fix.txt", "fix\n");
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        Self {
            upstream: file_url(&up),
            fork: file_url(&fork),
            work,
            root,
            env: MapEnv::default(),
            _tmp: tmp,
        }
    }

    fn repos(&self, yaml: &str) {
        let yaml = yaml
            .replace("UPSTREAM", &self.upstream)
            .replace("FORK", &self.fork);
        fs::write(self.work.join("repos.yaml"), yaml).unwrap();
    }

    fn run(&self, args: &[&str]) -> (Exit, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut command_line = vec!["git-aggregate-rs"];
        command_line.extend_from_slice(args);
        let ctx = Context {
            fs: &OsFs,
            env: &self.env,
            dir: self.work.clone(),
        };
        let exit = run_with(command_line, &ctx, &mut out, &mut err);
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.work.join(path)).unwrap()
    }
}

/// `POS` with a `vault` remote at `vault` and `target: <target>`.
fn with_vault(vault: &Path, target: &str) -> String {
    let remotes = format!("    me: FORK\n    vault: {}\n", file_url(vault));
    format!("{POS}  target: {target}\n").replace("    me: FORK\n", &remotes)
}

const POS: &str = "\
./pos:
  remotes:
    oca: UPSTREAM
    me: FORK
  merges:
    - oca 17.0
    - oca refs/pull/1/head
    - remote: me
      ref: fix
";

#[test]
fn aggregates_branches_pull_requests_and_forks() {
    let ws = Workspace::new();
    ws.repos(&format!(
        "{POS}./tagged:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca v1\n./same:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n    - oca v1\n"
    ));
    let (exit, out, err) = ws.run(&[]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(out, "");
    assert!(err.contains("./pos: 2 merged ("), "{err}");
    assert!(err.contains("./tagged: base only ("), "{err}");
    for file in ["a.txt", "b.txt", "pr.txt", "fix.txt"] {
        assert!(ws.work.join("pos").join(file).exists(), "{file}");
    }
    assert!(!ws.work.join("tagged").join("b.txt").exists());
    assert!(ws.work.join(".odx").join("cache").is_dir());

    // A second run reuses the working trees.
    let (exit, out, err) = ws.run(&["--json", "-j", "2", "--log-level", "WARNING"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(err, "");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["schema"], 1);
    assert_eq!(json["repos"][0]["dir"], "./pos");
    assert_eq!(json["repos"][0]["merges"][0]["status"], "merged");
    assert_eq!(json["repos"][0]["merges"][1]["label"], "me fix");
    assert_eq!(json["repos"][0]["pushed"], serde_json::Value::Null);
    assert_eq!(json["repos"][1]["merges"], serde_json::json!([]));
    assert_eq!(json["repos"][2]["merges"][0]["status"], "up-to-date");
}

#[test]
fn dirmatch_selects_directories() {
    let ws = Workspace::new();
    ws.repos(&format!(
        "{POS}./web:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n"
    ));
    let (exit, _, err) = ws.run(&["-d", "*/web"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(err.contains("Skip ./pos"), "{err}");
    assert!(ws.work.join("web").join("b.txt").exists());
    assert!(!ws.work.join("pos").exists());
}

#[test]
fn environment_expansion() {
    let mut ws = Workspace::new();
    ws.repos("./pos:\n  remotes:\n    oca: ${UP}\n  merges:\n    - oca $BRANCH\n  shell_command_after: echo $MISSING\n");
    fs::write(ws.work.join(".env"), "UP=wrong\nBRANCH=17.0\n").unwrap();
    ws.env = MapEnv::default().with("UP", ws.upstream.clone());
    let (exit, _, err) = ws.run(&["-e", "--env-file", ".env"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        err.contains("warning: $MISSING is not set; left as written"),
        "{err}"
    );
    assert!(
        err.contains("warning: ./pos: shell_command_after not run; pass --allow-shell to run it"),
        "{err}"
    );
    assert!(ws.work.join("pos").join("b.txt").exists());

    let (exit, _, err) = ws.run(&["-e", "--env-file", "missing.env"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("missing.env: not found; ignored"), "{err}");
    assert!(err.contains("warning: $BRANCH is not set"), "{err}");

    let (exit, _, err) = ws.run(&["--env-file", ".env"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("warning: --env-file has no effect without --expand-env"),
        "{err}"
    );

    // Errors only.
    let (exit, _, err) = ws.run(&["--env-file", ".env", "--log-level", "error"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.starts_with("error: ./pos: "), "{err}");
}

#[test]
fn shell_commands_need_allow_shell() {
    let ws = Workspace::new();
    ws.repos("./pos:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n  shell_command_after:\n    - echo done > done.txt\n");
    let (exit, _, err) = ws.run(&["--allow-shell"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(ws.read("pos/done.txt").trim(), "done");

    ws.repos("./pos:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n  shell_command_after: exit 3\n");
    let (exit, _, err) = ws.run(&["--allow-shell", "-f"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("error: ./pos: shell_command_after `exit 3` failed"),
        "{err}"
    );
}

#[test]
fn push_to_the_target_remote() {
    let ws = Workspace::new();
    let vault = ws.root.join("vault.git");
    git(&ws.root, &["init", "--quiet", "--bare", "vault.git"]);
    ws.repos(&with_vault(&vault, "vault 17.0-aggregated"));
    let (exit, out, err) = ws.run(&["-p", "--json"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        err.contains("./pos: pushed to vault 17.0-aggregated"),
        "{err}"
    );
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["repos"][0]["pushed"], "vault 17.0-aggregated");
    assert_eq!(
        git(&vault, &["rev-parse", "17.0-aggregated"]),
        git(&ws.work.join("pos"), &["rev-parse", "HEAD"])
    );

    // A target without remote cannot be pushed.
    ws.repos(&format!("{POS}  target: local\n"));
    let (exit, _, err) = ws.run(&["-p"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("error: ./pos: cannot push: no target remote configured"),
        "{err}"
    );

    // A push the remote refuses.
    fs::remove_dir_all(&vault).unwrap();
    ws.repos(&with_vault(&vault, "vault x"));
    let (exit, _, err) = ws.run(&["-p"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("error: ./pos: push to vault failed"), "{err}");
}

#[test]
fn existing_directories_need_force() {
    let ws = Workspace::new();
    ws.repos(POS);
    fs::create_dir_all(ws.work.join("pos")).unwrap();
    fs::write(ws.work.join("pos").join("old.txt"), "git-aggregator clone").unwrap();
    let (exit, _, err) = ws.run(&[]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("use --force to replace it"), "{err}");
    let (exit, _, err) = ws.run(&["--force"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(!ws.work.join("pos").join("old.txt").exists());
}

#[test]
fn failures() {
    let mut ws = Workspace::new();
    ws.repos("./gone:\n  remotes:\n    oca: file:///nonexistent/odx-test\n  merges:\n    - oca 17.0\n./pos:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n    - oca refs/pull/1/head\n    - oca nonexistent-branch\n");
    let (exit, out, err) = ws.run(&["--json"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("error: ./gone: "), "{err}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        json["repos"][0]["error"]
            .as_str()
            .unwrap()
            .contains("nonexistent")
    );
    assert!(err.contains("error: ./pos: "), "{err}");

    // Conflicting merges.
    let fork = ws.root.join("fork");
    git(&fork, &["checkout", "--quiet", "-b", "clash", "17.0"]);
    commit(&fork, "pr.txt", "clash\n");
    ws.repos("./pos:\n  remotes:\n    oca: UPSTREAM\n    me: FORK\n  merges:\n    - oca 17.0\n    - oca refs/pull/1/head\n    - me clash\n");
    let (exit, _, err) = ws.run(&[]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("error: ./pos: me clash conflicts in pr.txt"),
        "{err}"
    );

    // Invalid repos.yaml.
    ws.repos("./pos:\n  merges:\n    - oca\n");
    let (exit, _, err) = ws.run(&[]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.starts_with("error: repos.yaml: invalid repos.yaml"),
        "{err}"
    );

    // mergiraf that cannot be run.
    ws.repos(POS);
    ws.env = MapEnv::default().with("ODX_MERGIRAF", "/nonexistent/mergiraf");
    let (exit, _, err) = ws.run(&["--mergiraf"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("--mergiraf is given, but mergiraf cannot be run"),
        "{err}"
    );

    // An unreadable user configuration.
    let config = ws.root.join("config");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "[bogus\n").unwrap();
    ws.env = MapEnv::default().with("ODX_CONFIG_DIR", config.display().to_string());
    let (exit, _, err) = ws.run(&[]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("config.toml"), "{err}");
}

#[test]
fn shared_cache_and_odx_entry_point() {
    let mut ws = Workspace::new();
    ws.repos(POS);
    let cache = ws.root.join("cache");
    ws.env = MapEnv::default().with("ODX_CACHE_DIR", cache.display().to_string());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let exit = odx_cli_like(&ws, &mut out, &mut err);
    assert_eq!(exit, Exit::Success, "{}", String::from_utf8_lossy(&err));
    assert!(cache.join("repos").is_dir());
    assert!(!ws.work.join(".odx").exists());
}

/// The `odx git-aggregate` entry point passes its own context and prefix.
fn odx_cli_like(ws: &Workspace, out: &mut Vec<u8>, err: &mut Vec<u8>) -> Exit {
    let cli = <git_aggregate_rs::Cli as clap::Parser>::try_parse_from(["git-aggregate"]).unwrap();
    let ctx = Context {
        fs: &OsFs,
        env: &ws.env,
        dir: ws.work.clone(),
    };
    git_aggregate_rs::execute(&cli, "odx git-aggregate", &ctx, out, err).unwrap()
}

#[cfg(not(windows))]
#[test]
fn shell_commands_that_remove_their_directory() {
    let ws = Workspace::new();
    ws.repos("./pos:\n  remotes:\n    oca: UPSTREAM\n  merges:\n    - oca 17.0\n  shell_command_after:\n    - cd .. && rm -rf pos\n    - 'true'\n");
    let (exit, _, err) = ws.run(&["--allow-shell"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("error: ./pos: shell_command_after `true` failed: "),
        "{err}"
    );
}

#[test]
fn write_failures() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let ws = Workspace::new();
    ws.repos(POS);
    let ctx = Context {
        fs: &OsFs,
        env: &ws.env,
        dir: ws.work.clone(),
    };
    let args = ["git-aggregate-rs", "--json", "-d", "nothing"];
    assert_eq!(
        run_with(args, &ctx, &mut Broken, &mut Vec::new()),
        Exit::Failure
    );
    assert_eq!(
        run_with(args, &ctx, &mut Vec::new(), &mut Broken),
        Exit::Failure
    );
    assert!(std::io::Write::flush(&mut Broken).is_ok());
}

/// The same `repos.yaml` through git-aggregator's `gitaggregate` and
/// `git-aggregate-rs` gives the same trees. Needs `gitaggregate` on `PATH`
/// (or `GITAGGREGATE`), e.g. `uv tool install git-aggregator`.
#[test]
#[ignore = "needs git-aggregator; CI runs it with --ignored"]
fn parity_with_gitaggregate() {
    let mut ws = Workspace::new();
    let fix = git(&ws.root.join("fork"), &["rev-parse", "fix"]);
    let yaml = format!(
        "\
./branches:
  remotes:
    oca: UPSTREAM
    me: FORK
  merges:
    - oca 17.0
    - me fix
./pull:
  remotes:
    oca: UPSTREAM
  merges:
    - oca 17.0
    - oca refs/pull/1/head
  target: aggregated
./tag:
  remotes:
    oca: UPSTREAM
  merges:
    - oca v1
./commit:
  remotes:
    oca: $ORIGIN_URL
    me: FORK
  merges:
    - oca 17.0
    - me {fix}
"
    );
    ws.repos(&yaml);
    ws.env = MapEnv::default().with("ORIGIN_URL", ws.upstream.clone());
    let (exit, _, err) = ws.run(&["-e"]);
    assert_eq!(exit, Exit::Success, "{err}");

    let theirs = ws.root.join("theirs");
    fs::create_dir_all(&theirs).unwrap();
    fs::copy(ws.work.join("repos.yaml"), theirs.join("repos.yaml")).unwrap();
    let program = std::env::var("GITAGGREGATE").unwrap_or_else(|_| "gitaggregate".to_owned());
    let out = Command::new(program)
        .current_dir(&theirs)
        .args(["-c", "repos.yaml", "-e"])
        .env("ORIGIN_URL", &ws.upstream)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gitaggregate: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for dir in ["branches", "pull", "tag", "commit"] {
        let tree = |root: &Path| git(&root.join(dir), &["rev-parse", "HEAD^{tree}"]);
        assert_eq!(tree(&ws.work), tree(&theirs), "{dir}");
    }
    assert_eq!(
        git(&theirs.join("pull"), &["branch", "--show-current"]),
        "aggregated"
    );
}
