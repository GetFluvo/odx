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
    // Fetched moments ago: served from the cache.
    let (exit, out, _) = run(
        &project,
        &["--jobs", "1", "aggregate", "--fetch-only", "--repo", "pos"],
    );
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "fetched pos: 2 refs (2 already cached)\n")
    );
    let (_, out, _) = run(&project, &["--fresh", "aggregate", "--fetch-only"]);
    assert_eq!(out, "fetched pos: 2 refs\n");
    let (_, out, _) = run(&project, &["--no-refresh", "aggregate", "--fetch-only"]);
    assert_eq!(out, "fetched pos: 2 refs (2 already cached)\n");
    // With a TTL of zero, every run fetches.
    write_config(
        &project,
        "[tool.odx.cache]\nttl = { refs = \"0s\" }",
        "{ pr = 1 }",
    );
    let (_, out, _) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!(out, "fetched pos: 2 refs\n");
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
fn commits_come_from_the_users_mirror() {
    for prefer in [true, false] {
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
        write_config(&base, "", &format!("{{ sha = \"{}\" }}", base.pr));
        // Keyed by the directory that holds the upstream repository.
        let (prefix, _) = base.upstream.rsplit_once('/').unwrap();
        std::fs::create_dir_all(tmp.path().join("config")).unwrap();
        std::fs::write(
            tmp.path().join("config").join("config.toml"),
            format!(
                "[mirrors]\n\"{prefix}\" = {{ url = \"{}\", prefer = {prefer} }}\n",
                file_url(&mirror)
            ),
        )
        .unwrap();
        let project = Project {
            env: base.env.clone().with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            ),
            ..base
        };
        let (exit, out, err) = run(&project, &["aggregate", "--fetch-only"]);
        let expected = if prefer {
            "fetched pos: 2 refs (1 from the mirror)\n"
        } else {
            "fetched pos: 2 refs\n"
        };
        assert_eq!(
            (exit, out.as_str(), err.as_str()),
            (Exit::Success, expected, "")
        );
    }
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
        err.starts_with("error: aggregation failed for\n  gone: git fetch failed: "),
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
    assert!(err.contains("use git-aggregate-rs"), "{err}");
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

/// Add pull request `n` to the upstream behind `project`, changing `b.txt`.
fn add_pr(project: &Project, n: u32, content: &str) {
    let dir = PathBuf::from(project.upstream.trim_start_matches("file://"));
    let dir = if cfg!(windows) {
        PathBuf::from(dir.to_string_lossy().trim_start_matches('/'))
    } else {
        dir
    };
    git(&dir, &["checkout", "--quiet", "--detach", "17.0"]);
    std::fs::write(dir.join("b.txt"), content).unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "--quiet", "-m", content]);
    let sha = git(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", &format!("refs/pull/{n}/head"), &sha]);
    git(&dir, &["checkout", "--quiet", "17.0"]);
}

/// Replace commit ids (7 to 40 hex characters) so snapshots are stable.
fn redact(text: &str) -> String {
    text.split_inclusive(|c: char| !c.is_ascii_hexdigit())
        .map(|piece| {
            let hex = piece.trim_end_matches(|c: char| !c.is_ascii_hexdigit());
            if hex.len() >= 7 && hex.chars().any(|c| c.is_ascii_digit()) {
                piece.replacen(hex, "<sha>", 1)
            } else {
                piece.to_owned()
            }
        })
        .collect()
}

fn write_config(project: &Project, extra: &str, merges: &str) {
    let text = format!(
        "[tool.odx]\nodoo_version = \"17.0\"\n{extra}\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}\"\nmerges = [{merges}]\n",
        project.upstream
    );
    std::fs::write(project.root.join("pyproject.toml"), text).unwrap();
}

#[test]
fn aggregate_builds_the_working_tree() {
    let project = project("mergiraf = \"never\"");
    let (exit, out, err) = run(&project, &["aggregate"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    insta::assert_snapshot!(redact(&out));
    assert!(project.root.join("vendor/pos/b.txt").exists());
    let (exit, out, _) = run(&project, &["--json", "aggregate", "--update"]);
    assert_eq!(exit, Exit::Success);
    insta::assert_snapshot!(redact(&out));
}

#[test]
fn conflicts_stop_or_are_skipped() {
    let project = project("");
    add_pr(&project, 2, "conflicting");
    write_config(&project, "mergiraf = \"never\"", "{ pr = 1 }, { pr = 2 }");
    let (exit, out, err) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(out, "fetched pos: 3 refs\n");
    assert_eq!(
        err,
        "error: aggregation failed for\n  pos: #2 conflicts in b.txt\n"
    );

    write_config(
        &project,
        "mergiraf = \"never\"",
        "{ pr = 1 }, { pr = 2, on_conflict = \"skip\" }",
    );
    let (exit, out, _) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        redact(&out),
        "fetched pos: 3 refs (3 already cached)\nbuilt pos in vendor/pos: 1 merged, 1 skipped: #2 conflicts in b.txt (<sha>)\nlocked pos at <sha>\n"
    );
    let (_, json, _) = run(&project, &["--json", "aggregate", "--update"]);
    assert!(
        json.contains("\"status\":\"skipped\",\"files\":[\"b.txt\"]")
            || json.contains("\"files\":[\"b.txt\"],\"label\":\"#2\",\"status\":\"skipped\""),
        "{json}"
    );

    write_config(&project, "mergiraf = \"never\"", "{ pr = 1 }, { pr = 2 }");
    let (exit, _, err) = run(&project, &["aggregate", "--keep-conflicts"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("(left in place)"), "{err}");
    let (exit, _, err) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("has local changes"), "{err}");
    write_config(&project, "mergiraf = \"never\"", "{ pr = 1 }");
    let (exit, _, _) = run(&project, &["aggregate", "--force"]);
    assert_eq!(exit, Exit::Success);
}

#[test]
fn mergiraf_modes() {
    let mut project = project("mergiraf = \"always\"");
    project.env = project
        .env
        .clone()
        .with("ODX_MERGIRAF", "odx-surely-missing-mergiraf");
    let (exit, _, err) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: mergiraf = \"always\" is configured, but odx-surely-missing-mergiraf cannot be run\n"
    );
    write_config(&project, "mergiraf = \"auto\"", "{ pr = 1 }");
    assert_eq!(run(&project, &["aggregate"]).0, Exit::Success);
    project.env = project.env.clone().with("ODX_MERGIRAF", "git");
    write_config(&project, "mergiraf = \"always\"", "{ pr = 1 }");
    assert_eq!(run(&project, &["aggregate"]).0, Exit::Success);
    write_config(&project, "mergiraf = \"auto\"", "{ pr = 1 }");
    assert_eq!(run(&project, &["aggregate"]).0, Exit::Success);
}

#[test]
fn fetch_failures_skip_the_build() {
    let project = project(
        "mergiraf = \"never\"\n[[tool.odx.repositories]]\nname = \"gone\"\nurl = \"file:///nonexistent/odx/gone\"\n",
    );
    let (exit, out, err) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("built pos"), "{out}");
    assert!(
        err.starts_with("error: aggregation failed for\n  gone: git fetch failed"),
        "{err}"
    );
    let (_, json, _) = run(&project, &["--json", "aggregate", "--fetch-only"]);
    assert!(
        json.contains("\"error\"") && !json.contains("\"built\""),
        "{json}"
    );
}

#[test]
fn up_to_date_merges_base_only_repositories_and_json_errors() {
    let project = project("");
    let dir = PathBuf::from(project.upstream.trim_start_matches("file://"));
    let dir = if cfg!(windows) {
        PathBuf::from(dir.to_string_lossy().trim_start_matches('/'))
    } else {
        dir
    };
    let base = git(&dir, &["rev-parse", "17.0"]);
    write_config(
        &project,
        "mergiraf = \"never\"",
        &format!("{{ sha = \"{base}\" }}"),
    );
    let (exit, out, _) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.contains("built pos in vendor/pos: 1 up to date ("),
        "{out}"
    );
    let (_, json, _) = run(&project, &["--json", "aggregate", "--update"]);
    assert!(json.contains("\"status\":\"up-to-date\""), "{json}");

    write_config(&project, "mergiraf = \"never\"", "");
    let (_, out, _) = run(&project, &["aggregate"]);
    assert!(
        out.contains("built pos in vendor/pos: base only ("),
        "{out}"
    );

    add_pr(&project, 2, "conflicting");
    write_config(&project, "mergiraf = \"never\"", "{ pr = 1 }, { pr = 2 }");
    let (exit, json, _) = run(&project, &["--json", "aggregate", "--force"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        json.contains("\"error\":\"#2 conflicts in b.txt\""),
        "{json}"
    );
}

#[test]
fn git_aggregate_builds_repos_yaml_targets() {
    let project = project("");
    std::fs::write(
        project.root.join("repos.yaml"),
        format!(
            "./vendor/pos:\n  remotes:\n    oca: {}\n  merges:\n    - oca 17.0\n    - oca refs/pull/1/head\n",
            project.upstream
        ),
    )
    .unwrap();
    let (exit, out, err) = run(&project, &["git-aggregate", "--json"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json["repos"][0]["dir"], "./vendor/pos");
    assert_eq!(
        git(
            &project.root.join("vendor").join("pos"),
            &["rev-parse", "HEAD^2"]
        ),
        project.pr
    );
}

#[test]
fn merges_are_rebased_unless_configured() {
    let rebased = project("");
    let (exit, _, err) = run(&rebased, &["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let work = rebased.root.join("vendor").join("pos");
    assert_eq!(git(&work, &["rev-list", "--merges", "HEAD"]), "");
    assert_eq!(
        git(&work, &["rev-parse", "HEAD"]),
        rebased.pr,
        "fast-forward"
    );

    let merged = project("merge_strategy = \"merge\"");
    let (exit, _, err) = run(&merged, &["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let work = merged.root.join("vendor").join("pos");
    assert_eq!(git(&work, &["rev-parse", "HEAD^2"]), merged.pr);
}

fn upstream_dir(project: &Project) -> PathBuf {
    let dir = PathBuf::from(project.upstream.trim_start_matches("file://"));
    if cfg!(windows) {
        PathBuf::from(dir.to_string_lossy().trim_start_matches('/'))
    } else {
        dir
    }
}

fn read_lock(project: &Project) -> String {
    std::fs::read_to_string(project.root.join("odx.lock")).unwrap()
}

#[test]
fn lock_then_aggregate_from_the_lock() {
    let project = project("mergiraf = \"never\"");
    let (exit, out, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("resolved pos: 1 merged ("), "{out}");
    assert!(out.contains("locked pos at "), "{out}");
    assert!(
        !project.root.join("vendor").exists(),
        "lock does not build the project"
    );
    let locked = read_lock(&project);
    assert!(
        locked.contains(&format!("sha = \"{}\"", project.pr)),
        "{locked}"
    );

    // Nothing changed upstream: nothing to resolve, the file stays the same.
    let (exit, out, _) = run(&project, &["lock"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, ""));
    assert_eq!(read_lock(&project), locked);
    assert_eq!(run(&project, &["lock", "--check"]).0, Exit::Success);

    let (exit, out, err) = run(&project, &["aggregate", "--locked"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("built pos in vendor/pos"), "{out}");
    assert_eq!(read_lock(&project), locked);

    // The working tree is what the lock describes: no fetch, no merge.
    let (exit, out, _) = run(&project, &["aggregate"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "up to date pos in vendor/pos (locked)\n")
    );
    let (_, json, _) = run(&project, &["--json", "aggregate"]);
    assert!(
        json.contains("\"current\":[{\"name\":\"pos\",\"target_dir\":\"vendor/pos\"}]"),
        "{json}"
    );
}

#[test]
fn moved_heads_stay_pinned_until_updated() {
    let project = project("mergiraf = \"never\"");
    assert_eq!(run(&project, &["aggregate"]).0, Exit::Success);
    // The pull request gets different changes upstream.
    add_pr(&project, 1, "changed");
    let (exit, out, _) = run(&project, &["aggregate"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "up to date pos in vendor/pos (locked)\n")
    );
    assert_eq!(run(&project, &["lock", "--check"]).0, Exit::Success);
    let (exit, out, _) = run(&project, &["lock", "--update"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        redact(&out).contains("updated pos #1: <sha> -> <sha> (changed)"),
        "{out}"
    );
    let (exit, out, _) = run(&project, &["aggregate", "--locked"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("built pos"), "{out}");
    let b = std::fs::read_to_string(project.root.join("vendor/pos/b.txt")).unwrap();
    assert_eq!(b.trim(), "changed");

    // The base moves and the pull request is rebased with the same change.
    let up = upstream_dir(&project);
    let pr = git(&up, &["rev-parse", "refs/pull/1/head"]);
    git(&up, &["checkout", "--quiet", "17.0"]);
    std::fs::write(up.join("c.txt"), "c").unwrap();
    git(&up, &["add", "c.txt"]);
    git(&up, &["commit", "--quiet", "-m", "c"]);
    git(&up, &["checkout", "--quiet", "--detach", "17.0"]);
    git(&up, &["cherry-pick", &pr]);
    let rebased = git(&up, &["rev-parse", "HEAD"]);
    git(&up, &["update-ref", "refs/pull/1/head", &rebased]);
    git(&up, &["checkout", "--quiet", "17.0"]);
    let (exit, out, _) = run(&project, &["--json", "aggregate", "--update"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("\"change\":\"rebased (same patch)\""), "{out}");
    assert!(out.contains("\"item\":\"base 17.0\""), "{out}");
}

#[test]
fn configuration_changes_invalidate_the_lock() {
    let project = project("mergiraf = \"never\"");
    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    let locked = read_lock(&project);
    write_config(
        &project,
        "mergiraf = \"never\"",
        "{ pr = 1 }, { branch = \"17.0\" }",
    );
    let (exit, _, err) = run(&project, &["lock", "--check"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("odx.lock is not up to date; run `odx lock`"),
        "{err}"
    );
    assert_eq!(read_lock(&project), locked);
    let (exit, _, err) = run(&project, &["aggregate", "--locked"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("odx.lock does not lock pos as configured; run `odx lock`"),
        "{err}"
    );

    // Without repositories, the entry goes away.
    std::fs::write(
        project.root.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\n",
    )
    .unwrap();
    let (exit, out, _) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Success);
    assert!(redact(&out).contains("unlocked pos (was <sha>)"), "{out}");
}

#[test]
fn lock_errors() {
    let project = project("mergiraf = \"never\"");
    let (exit, _, err) = run(&project, &["lock", "--repo", "nope"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no repository named \"nope\""), "{err}");
    let (exit, _, err) = run(&project, &["aggregate", "--repo", "nope"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no repository named \"nope\""), "{err}");

    std::fs::write(project.root.join("odx.lock"), "version = 1\n[[bogus]]\n").unwrap();
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("odx.lock"), "{err}");
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();

    write_config(
        &project,
        "mergiraf = \"never\"\nlockfile = \"missing/odx.lock\"",
        "{ pr = 1 }",
    );
    let (exit, _, err) = run(&project, &["lock", "--repo", "pos"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock"), "{err}");
    let (exit, _, err) = run(&project, &["aggregate"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock"), "{err}");

    // A failed build locks nothing.
    add_pr(&project, 2, "conflicting");
    write_config(&project, "mergiraf = \"never\"", "{ pr = 1 }, { pr = 2 }");
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("#2 conflicts in b.txt"), "{err}");
    assert!(!project.root.join("odx.lock").exists());
}

#[test]
fn unnamed_repositories_are_locked_by_url() {
    let mut project = project("mergiraf = \"never\"");
    std::fs::write(
        project.root.join("pyproject.toml"),
        format!(
            "[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"never\"\n[[tool.odx.repositories]]\nurl = \"{}\"\nmerges = [{{ pr = 1 }}]\n",
            project.upstream
        ),
    )
    .unwrap();
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(read_lock(&project).contains(&format!("name = \"{}\"", project.upstream)));
    assert_eq!(run(&project, &["lock", "--check"]).0, Exit::Success);

    write_config(&project, "mergiraf = \"always\"", "{ pr = 1 }");
    project.env = project
        .env
        .clone()
        .with("ODX_MERGIRAF", "odx-surely-missing-mergiraf");
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("cannot be run"), "{err}");
}

/// A bare vault next to the project; returns its path.
fn vault_dir(project: &Project) -> PathBuf {
    let vault = project.root.parent().unwrap().join("vault.git");
    git(
        project.root.parent().unwrap(),
        &["init", "--quiet", "--bare", vault.to_str().unwrap()],
    );
    vault
}

fn vault_config(project: &Project, mirror: &str) {
    write_config(
        project,
        &format!("mergiraf = \"never\"\n[tool.odx.mirror]\n{mirror}"),
        "{ pr = 1 }",
    );
}

fn file_url_of(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[test]
fn lock_vaults_commits_and_aggregate_restores_from_the_vault() {
    let project = project("");
    let vault = vault_dir(&project);
    vault_config(&project, &format!("url = \"{}\"\n", file_url_of(&vault)));
    let (exit, out, err) = run(&project, &["--json", "lock"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.contains("\"vaulted\":[{\"commits\":2,\"repository\":\"pos\"}]"),
        "{out}"
    );
    assert!(read_lock(&project).contains(&format!("mirror_ref = \"refs/odx/{}\"", project.pr)));
    assert_eq!(
        git(&vault, &["rev-parse", &format!("refs/odx/{}", project.pr)]),
        project.pr
    );
    // Everything is vaulted: nothing to push.
    let (exit, out, _) = run(&project, &["lock"]);
    assert_eq!((exit, out.as_str()), (Exit::Success, ""));

    // The pull request is force-pushed upstream and the old commit collected.
    add_pr(&project, 1, "rewritten");
    let up = upstream_dir(&project);
    git(&up, &["reflog", "expire", "--expire=now", "--all"]);
    git(&up, &["gc", "--quiet", "--prune=now"]);
    assert_ne!(
        Command::new("git")
            .current_dir(&up)
            .args(["cat-file", "-e", &project.pr])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    std::fs::remove_dir_all(project.root.parent().unwrap().join("cache")).unwrap();
    let (exit, out, err) = run(&project, &["aggregate", "--locked"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("from the mirror"), "{out}");
    // In CI the vault is asked first.
    std::fs::remove_dir_all(project.root.join("vendor")).unwrap();
    let mut ci = project;
    ci.env = ci.env.clone().with("CI", "true");
    let (exit, out, err) = run(&ci, &["aggregate", "--locked"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("already cached"), "{out}");
    let project = ci;
    assert_eq!(
        git(
            &project.root.join("vendor/pos"),
            &["log", "-1", "--format=%s"]
        ),
        "b"
    );
}

#[test]
fn vault_ref_styles_and_opt_outs() {
    let project = project("");
    let vault = vault_dir(&project);
    let url = file_url_of(&vault);
    vault_config(
        &project,
        &format!("url = \"{url}\"\nref_style = \"tags\"\n"),
    );
    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    assert!(read_lock(&project).contains("mirror_ref = \"refs/tags/odx/"));

    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    vault_config(
        &project,
        &format!("url = \"{url}\"\nref_prefix = \"refs/keep/\"\n"),
    );
    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    assert!(read_lock(&project).contains("mirror_ref = \"refs/keep/"));

    for (mirror, args) in [
        (format!("url = \"{url}\"\n"), &["lock", "--no-push"][..]),
        (
            format!("url = \"{url}\"\npush_on_lock = false\n"),
            &["lock"][..],
        ),
    ] {
        std::fs::remove_file(project.root.join("odx.lock")).unwrap();
        vault_config(&project, &mirror);
        assert_eq!(run(&project, args).0, Exit::Success);
        assert!(!read_lock(&project).contains("mirror_ref"));
    }

    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    vault_config(&project, "url = \"file:///nonexistent/odx/vault.git\"\n");
    let (exit, out, err) = run(&project, &["--json", "lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(out.contains("vaulting failed"), "{out} {err}");
    // The lock is written; the commits are vaulted on the next run.
    assert!(!read_lock(&project).contains("mirror_ref"));
}

/// Answer HTTP requests by path prefix until the test ends; returns the
/// base URL.
fn forge_server(routes: &'static [(&'static str, u16, &'static str)]) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend(buffer.iter().take(n)),
                }
            }
            let text = String::from_utf8_lossy(&request);
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let (status, body) = routes
                .iter()
                .find(|(prefix, _, _)| path.starts_with(prefix))
                .map_or((404, ""), |(_, status, body)| (*status, *body));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

#[test]
fn vault_visibility_is_checked_on_the_forge() {
    let project = project("");
    let public = forge_server(&[
        ("/api/v1/version", 200, "{\"version\":\"1\"}"),
        ("/api/v1/repos/odoo/vault", 200, "{\"private\":false}"),
    ]);
    vault_config(&project, &format!("url = \"{public}/odoo/vault.git\"\n"));
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("is public on Gitea/Forgejo; odx vaults commits only in private repositories"),
        "{err}"
    );
    assert!(err.contains("odx lock --no-push"), "{err}");

    // allow_public lets it through; the push itself then fails.
    vault_config(
        &project,
        &format!("url = \"{public}/odoo/vault.git\"\nallow_public = true\n"),
    );
    let (exit, out, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("vaulting failed"), "{out} {err}");

    let gitlab = forge_server(&[
        ("/api/v4/version", 401, "{}"),
        (
            "/api/v4/projects/odoo%2Fvault",
            200,
            "{\"visibility\":\"private\"}",
        ),
    ]);
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    vault_config(
        &project,
        &format!("url = \"{gitlab}/odoo/vault.git\"\nref_style = \"refs\"\n"),
    );
    let (_, _, err) = run(&project, &["lock"]);
    assert!(
        err.contains("the vault protects tags, not custom refs"),
        "{err}"
    );

    // A missing vault is created, private and with protected tags.
    let creating = forge_server(&[
        ("/api/v1/version", 200, "{}"),
        ("/api/v1/repos/odoo/vault/tag_protections", 201, "{}"),
        ("/api/v1/repos/odoo/vault", 404, ""),
        ("/api/v1/orgs/odoo/repos", 201, "{}"),
    ]);
    let _ = std::fs::remove_file(project.root.join("odx.lock"));
    vault_config(
        &project,
        &format!("url = \"{creating}/odoo/vault.git\"\nauto_create = true\nref_style = \"refs\"\n"),
    );
    let (_, out, err) = run(&project, &["lock"]);
    assert!(out.contains("created the private vault"), "{out}{err}");
    assert!(err.contains("the vault protects tags"), "{err}");
    let refusing = forge_server(&[
        ("/api/v1/version", 200, "{}"),
        ("/api/v1/repos/odoo/vault", 404, ""),
        ("/api/v1/orgs/odoo/repos", 403, ""),
    ]);
    vault_config(
        &project,
        &format!("url = \"{refusing}/odoo/vault.git\"\nauto_create = true\n"),
    );
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("answered HTTP 403"), "{err}");

    let unprotected = forge_server(&[
        ("/api/v1/version", 200, "{}"),
        ("/api/v1/repos/odoo/vault/tag_protections", 403, ""),
        ("/api/v1/repos/odoo/vault", 404, ""),
        ("/api/v1/orgs/odoo/repos", 201, "{}"),
    ]);
    vault_config(
        &project,
        &format!("url = \"{unprotected}/odoo/vault.git\"\nauto_create = true\n"),
    );
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("answered HTTP 403"), "{err}");

    let nothing = forge_server(&[]);
    vault_config(&project, &format!("url = \"{nothing}/odoo/vault.git\"\n"));
    let (exit, _, err) = run(&project, &["lock"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("does not look like GitHub, GitLab, Gitea or Forgejo"),
        "{err}"
    );
    let missing = forge_server(&[("/api/v1/version", 200, "{}")]);
    vault_config(&project, &format!("url = \"{missing}/odoo/vault.git\"\n"));
    let (_, _, err) = run(&project, &["lock"]);
    assert!(err.contains("cannot see"), "{err}");
}

#[test]
fn without_a_cache_directory_the_project_keeps_one() {
    let mut project = project("mergiraf = \"never\"");
    project.env = MapEnv::default();
    assert_eq!(
        run(&project, &["aggregate", "--fetch-only"]).0,
        Exit::Success
    );
    assert!(project.root.join(".odx").join("cache").is_dir());
}

#[test]
fn verify_checks_the_lock_and_the_working_trees() {
    let project = project("mergiraf = \"never\"");
    let (exit, _, err) = run(&project, &["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock is missing; run `odx lock`"), "{err}");
    assert!(err.contains("pos: not locked as configured"), "{err}");

    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    let (exit, _, err) = run(&project, &["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("pos: vendor/pos is missing; run `odx aggregate --locked`"),
        "{err}"
    );

    assert_eq!(run(&project, &["aggregate", "--locked"]).0, Exit::Success);
    let (exit, out, err) = run(&project, &["verify"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert_eq!(
        out,
        "verified: odx.lock matches the configuration and 1 working trees\n"
    );
    let (_, json, _) = run(&project, &["--json", "verify"]);
    assert_eq!(json, "{\"ok\":true,\"problems\":[],\"schema\":1}\n");

    std::fs::write(project.root.join("vendor/pos/a.txt"), "edited").unwrap();
    let (exit, _, err) = run(&project, &["verify"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("pos: vendor/pos is not the locked result"),
        "{err}"
    );

    std::fs::write(
        project.root.join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"17.0\"\n",
    )
    .unwrap();
    let (_, json, _) = run(&project, &["--json", "verify"]);
    assert!(
        json.contains("pos: locked but no longer configured"),
        "{json}"
    );
}

#[test]
fn verify_finds_uncommitted_addons() {
    let project = project(
        "mergiraf = \"never\"\n[tool.odx.groups]\nproject_addons = [\"addons\", \"missing\"]\n",
    );
    for addon in ["committed", "stray"] {
        std::fs::create_dir_all(project.root.join("addons").join(addon)).unwrap();
        std::fs::write(
            project
                .root
                .join("addons")
                .join(addon)
                .join("__manifest__.py"),
            "{}",
        )
        .unwrap();
    }
    std::fs::create_dir_all(project.root.join("addons").join("not_an_addon")).unwrap();
    let (_, _, err) = run(&project, &["verify"]);
    assert!(err.contains("not a git checkout"), "{err}");
    git(&project.root, &["init", "--quiet"]);
    git(&project.root, &["add", "addons/committed"]);
    let (_, _, err) = run(&project, &["verify"]);
    assert!(
        err.contains("addons/stray: addon directory not committed"),
        "{err}"
    );
    assert!(!err.contains("addons/committed"), "{err}");
    assert!(!err.contains("not_an_addon"), "{err}");
}

#[test]
fn lock_migrate_rewrites_the_format() {
    let project = project("mergiraf = \"never\"");
    let (exit, _, err) = run(&project, &["lock", "--migrate"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock"), "{err}");
    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    let (_, out, _) = run(&project, &["lock", "--migrate"]);
    assert!(out.contains("is already in the current format"), "{out}");
    let canonical = std::fs::read_to_string(project.root.join("odx.lock")).unwrap();
    std::fs::write(
        project.root.join("odx.lock"),
        canonical.replace("# Generated by odx. Do not edit.\n", ""),
    )
    .unwrap();
    let (_, out, _) = run(&project, &["lock", "--migrate"]);
    assert!(out.contains("rewrote"), "{out}");
    assert_eq!(
        std::fs::read_to_string(project.root.join("odx.lock")).unwrap(),
        canonical
    );
    std::fs::write(project.root.join("odx.lock"), "version = 0\n").unwrap();
    let (exit, _, err) = run(&project, &["lock", "--migrate"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("which no odx release wrote"), "{err}");

    std::fs::write(project.root.join("repos.yaml"), "{}\n").unwrap();
    let (exit, _, err) = run(&project, &["-c", "repos.yaml", "lock", "--migrate"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("repos.yaml projects have no odx.lock"),
        "{err}"
    );
    std::fs::remove_file(project.root.join("pyproject.toml")).unwrap();
    std::fs::remove_file(project.root.join("repos.yaml")).unwrap();
    assert_eq!(run(&project, &["lock", "--migrate"]).0, Exit::Usage);
    assert_eq!(run(&project, &["verify"]).0, Exit::Usage);
}

#[test]
fn lock_migrate_reports_write_failures() {
    let project = project("mergiraf = \"never\"");
    assert_eq!(run(&project, &["lock"]).0, Exit::Success);
    let file = project.root.join("odx.lock");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("# Generated by odx. Do not edit.\n", ""),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&file, permissions).unwrap();
    let (exit, _, err) = run(&project, &["lock", "--migrate"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock"), "{err}");
}

#[test]
fn credentials_in_urls_never_reach_output() {
    let project = project(
        "mergiraf = \"never\"\n[[tool.odx.repositories]]\nurl = \"https://bot:s3cr3t-token@127.0.0.1:1/odoo/x.git\"\n",
    );
    let (exit, out, err) = run(&project, &["aggregate", "--fetch-only"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        !out.contains("s3cr3t") && !err.contains("s3cr3t"),
        "{out}{err}"
    );
    assert!(err.contains("https://***@127.0.0.1:1/odoo/x.git"), "{err}");
    let (_, json, _) = run(&project, &["--json", "aggregate", "--fetch-only"]);
    assert!(!json.contains("s3cr3t"), "{json}");
}

#[test]
fn bundles_carry_the_locked_commits() {
    let project = project("mergiraf = \"never\"");
    let text = std::fs::read_to_string(project.root.join("pyproject.toml")).unwrap();
    std::fs::write(
        project.root.join("pyproject.toml"),
        format!(
            "{text}[[tool.odx.repositories]]\nname = \"pos-copy\"\nurl = \"{}\"\ntarget_dir = \"vendor/pos-copy\"\n",
            project.upstream
        ),
    )
    .unwrap();
    let (exit, out, err) = run(&project, &["--json", "lock", "--bundle", "bundles"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("\"commits\":2"), "{out}");
    let (exit, out, _) = run(&project, &["lock", "--update", "--bundle", "bundles"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("bundled 2 commits of "), "{out}");

    // Upstream and cache are gone: the bundle is all there is.
    let up = upstream_dir(&project);
    std::fs::rename(&up, up.with_extension("gone")).unwrap();
    std::fs::remove_dir_all(project.root.parent().unwrap().join("cache")).unwrap();
    let (exit, out, err) = run(
        &project,
        &["aggregate", "--locked", "--bundle-dir", "bundles"],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(out.contains("from the mirror"), "{out}");
    assert!(project.root.join("vendor/pos/b.txt").exists());

    // The commits are not in the (new) cache: nothing to bundle from.
    std::fs::remove_dir_all(project.root.parent().unwrap().join("cache")).unwrap();
    let (exit, _, err) = run(&project, &["lock", "--bundle", "bundles"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("bundling failed"), "{err}");

    // Nowhere to write.
    std::fs::rename(up.with_extension("gone"), &up).unwrap();
    assert_eq!(
        run(&project, &["aggregate", "--locked", "--fetch-only"]).0,
        Exit::Success
    );
    std::fs::write(project.root.join("blocked"), "").unwrap();
    let (exit, _, err) = run(&project, &["lock", "--bundle", "blocked"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("bundling failed"), "{err}");
}
