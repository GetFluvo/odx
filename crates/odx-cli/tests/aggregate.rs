//! `odx aggregate --fetch-only` against real upstream repositories.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::fake::{MapEnv, MemoryFs};
use odx_core::system::OsFs;
use odx_core::{Exit, FileSystem};

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

/// An upstream with branch 17.0 and pull request 1; returns (url, pr sha).
fn upstream(root: &Path, name: &str) -> (String, String) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet", "--initial-branch=17.0"]);
    std::fs::write(dir.join("a.txt"), name).unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "--quiet", "-m", "a"]);
    std::fs::write(dir.join("b.txt"), name).unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "--quiet", "-m", "b"]);
    let pr = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", "refs/pull/1/head", &pr]);
    git(&dir, &["reset", "--quiet", "--hard", "HEAD~1"]);
    (file_url(&dir), pr)
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    env: MapEnv,
    pr: String,
    upstream: String,
}

fn project(extra: &str) -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let (upstream, pr) = upstream(tmp.path(), "pos");
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let text = format!(
        "[tool.odx]\nodoo_version = \"17.0\"\n{extra}\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{upstream}\"\nmerges = [{{ pr = 1 }}]\n"
    );
    std::fs::write(root.join("pyproject.toml"), text).unwrap();
    let env = MapEnv::default().with(
        "ODX_CACHE_DIR",
        tmp.path().join("cache").display().to_string(),
    );
    Project {
        root,
        env,
        pr,
        upstream,
        _tmp: tmp,
    }
}

fn run(project: &Project, args: &[&str]) -> (Exit, String, String) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let exit = odx_cli::run_with_env(
        command_line,
        &OsFs,
        &project.env,
        &project.root,
        &mut out,
        &mut err,
    );
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn fetch_only_fills_the_cache() {
    let project = project("");
    let (exit, out, err) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!(
        (exit, out.as_str(), err.as_str()),
        (Exit::Success, "fetched pos: 2 refs\n", "")
    );
    let (exit, out, _) = run(
        &project,
        &["--jobs", "1", "aggregate", "--fetch-only", "--repo", "pos"],
    );
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "fetched pos: 2 refs\n")
    );
}

#[test]
fn locked_commits_come_from_the_mirror() {
    let tmp = tempfile::tempdir().unwrap();
    let mirror = tmp.path().join("mirror.git");
    let base = project("");
    git(
        tmp.path(),
        &["init", "--quiet", "--bare", mirror.to_str().unwrap()],
    );
    let upstream_dir = base.upstream.trim_start_matches("file://").to_owned();
    let upstream_dir = if cfg!(windows) {
        upstream_dir.trim_start_matches('/').to_owned()
    } else {
        upstream_dir
    };
    git(
        Path::new(&upstream_dir),
        &[
            "push",
            "--quiet",
            mirror.to_str().unwrap(),
            &format!("{0}:refs/odx/{0}", base.pr),
        ],
    );
    let text = format!(
        "[tool.odx]\nodoo_version = \"17.0\"\njobs = 2\n[tool.odx.http]\nper_host_concurrency = 1\n[tool.odx.mirror]\nurl = \"{}\"\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}\"\nmerges = [{{ sha = \"{}\" }}]\n",
        file_url(&mirror),
        base.upstream,
        base.pr
    );
    std::fs::write(base.root.join("pyproject.toml"), text).unwrap();
    let (exit, out, _) = run(&base, &["aggregate", "--fetch-only", "--prefer-mirror"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "fetched pos: 2 refs (1 from the mirror)\n")
    );
}

#[test]
fn failures_and_usage_errors() {
    let broken = project(
        "[[tool.odx.repositories]]\nname = \"gone\"\nurl = \"file:///nonexistent/odx/gone\"\n",
    );
    let (exit, out, err) = run(&broken, &["aggregate", "--fetch-only"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(out, "fetched pos: 2 refs\n");
    assert!(
        err.starts_with("error: fetching failed for\n  gone: git fetch failed: "),
        "{err}"
    );

    let project = project("");
    let (exit, _, err) = run(&project, &["aggregate", "--fetch-only", "--repo", "nope"]);
    assert_eq!(
        (exit, err.as_str()),
        (
            Exit::Usage,
            "error: no repository named \"nope\" in [tool.odx]\n"
        )
    );
    let (exit, _, err) = run(
        &project,
        &["--config", "repos.yaml", "aggregate", "--fetch-only"],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("arrives with the drop-in engine"), "{err}");
}

#[test]
fn configuration_errors() {
    let env = MapEnv::default();
    let fs = MemoryFs::default();
    let mut err = Vec::new();
    let exit = odx_cli::run_with_env(
        ["odx", "aggregate", "--fetch-only"],
        &fs,
        &env,
        Path::new("/w"),
        &mut Vec::new(),
        &mut err,
    );
    assert_eq!(exit, Exit::Usage);
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("no pyproject.toml")
    );

    fs.write(
        Path::new("/w/pyproject.toml"),
        b"[tool.odx]\nodoo_version = \"17.0\"\n",
    )
    .unwrap();
    fs.fail_next(std::io::ErrorKind::PermissionDenied);
    let exit = odx_cli::run_with_env(
        ["odx", "aggregate", "--fetch-only"],
        &fs,
        &env,
        Path::new("/w"),
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(exit, Exit::Failure);
}

#[test]
fn warnings_are_shown() {
    let project = project(
        "[tool.odx.mirror]\nurl = \"https://gitlab.example.com/vault.git\"\nref_style = \"refs\"\n",
    );
    let (exit, _, err) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        err.starts_with("warning: tool.odx.mirror.ref_style: GitLab"),
        "{err}"
    );
}

#[test]
fn local_clones_from_the_user_configuration() {
    let mut project = project("");
    let tmp = project.root.parent().unwrap().to_path_buf();
    let clones = tmp.join("OCA");
    std::fs::create_dir_all(&clones).unwrap();
    git(&clones, &["clone", "--quiet", &project.upstream, "pos"]);
    let config_dir = tmp.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[clones]\ndirs = [\"{}\"]\nmode = \"update\"\n",
            clones.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    project.env = project
        .env
        .clone()
        .with("ODX_CONFIG_DIR", config_dir.display().to_string());
    let (exit, out, err) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(out, "fetched pos: 2 refs (1 through the local clone)\n");

    std::fs::write(config_dir.join("config.toml"), "[[repositories]]\n").unwrap();
    let (exit, _, err) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("unknown field `repositories`"), "{err}");
}
