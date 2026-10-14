//! `odx verdict` with two projects sharing a verdict repository, and
//! verdicts and mutes suppressing `odx suggest`.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Jane")
        .env("GIT_AUTHOR_EMAIL", "jane@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn change(dir: &Path, files: &[(&str, &str)], message: &str) {
    for (file, content) in files {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "--quiet", "-m", message]);
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn odx(env: &MapEnv, dir: &Path, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = odx_cli::run_with_env(command_line, &OsFs, env, dir, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

struct Machine {
    tmp: tempfile::TempDir,
    up: PathBuf,
    url: String,
    /// The suggestion key of pull request 2.
    key: String,
    env: MapEnv,
}

impl Machine {
    /// An upstream with pull request 2 on 16.0 that 17.0 lacks, and a
    /// shared verdict repository.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=16.0"]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '16'}")],
            "[MIG] pos_x: 16.0",
        );
        git(&up, &["checkout", "--quiet", "-b", "pr2"]);
        change(&up, &[("pos_x/b.py", "b\n")], "[FIX] pos_x: b");
        git(&up, &["checkout", "--quiet", "16.0"]);
        git(
            &up,
            &[
                "merge",
                "--quiet",
                "--no-ff",
                "pr2",
                "-m",
                "Merge PR #2 into 16.0",
            ],
        );
        git(&up, &["checkout", "--quiet", "--orphan", "17.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[("pos_x/__manifest__.py", "{'version': '17'}")],
            "[MIG] pos_x: 17.0",
        );
        let verdicts = tmp.path().join("verdicts.git");
        git(
            tmp.path(),
            &["init", "--quiet", "--bare", verdicts.to_str().unwrap()],
        );
        let config = tmp.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!("[verdicts]\nrepo = \"{}\"\n", file_url(&verdicts)),
        )
        .unwrap();
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with("ODX_CONFIG_DIR", config.display().to_string());
        let url = file_url(&up);
        let parent = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let key = format!("{parent}/up#2");
        Self {
            tmp,
            up,
            url,
            key,
            env,
        }
    }

    /// A project on 17.0 aggregating the upstream.
    fn project(&self, name: &str) -> PathBuf {
        let root = self.tmp.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            format!(
                "[tool.odx]\nodoo_version = \"17.0\"\nproject_id = \"{name}\"\n\n[tool.odx.port]\ndefault_from = [\"16.0\"]\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}\"\naddons = [\"pos_x\"]\nmerges = []\n",
                self.url
            ),
        )
        .unwrap();
        let (exit, _, err) = odx(&self.env, &root, &["aggregate"]);
        assert_eq!(exit, Exit::Success, "{err}");
        root
    }

    fn run(&self, dir: &Path, args: &[&str]) -> (Exit, String, String) {
        odx(&self.env, dir, args)
    }
}

#[test]
fn an_organisation_verdict_covers_every_project_until_the_code_changes() {
    let machine = Machine::new();
    let a = machine.project("a");
    let b = machine.project("b");
    let key = machine.key.clone();
    let (_, out, _) = machine.run(&a, &["suggest"]);
    assert!(
        out.contains(&format!("  {key} from 16.0: portable")),
        "{out}"
    );

    let (exit, out, err) = machine.run(
        &a,
        &[
            "verdict",
            "add",
            &key,
            "--addon",
            "pos_x",
            "--kind",
            "not-needed",
            "--scope",
            "organisation",
            "--reason",
            "17.0 has no such bug",
        ],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(out.starts_with("recorded v:17.0:pos_x:"), "{out}");
    let id = out.split_whitespace().nth(1).unwrap().to_owned();
    // Pushed to the verdict repository.
    let bare = machine.tmp.path().join("verdicts.git");
    assert!(
        git(&bare, &["log", "--format=%s"]).starts_with(&format!("verdict: {id} (not-needed)"))
    );

    for project in [&a, &b] {
        let (_, out, _) = machine.run(project, &["suggest"]);
        assert!(!out.contains(&key), "{out}");
        assert!(
            out.ends_with(
                "1 suggestions (0 blacklisted upstream, 1 suppressed by verdicts and mutes)
"
            ),
            "{out}"
        );
    }
    let (_, out, _) = machine.run(&b, &["suggest", "--show-suppressed"]);
    assert!(
        out.contains(&format!(
            "; suppressed by {id} (not-needed): 17.0 has no such bug"
        )),
        "{out}"
    );
    let (_, out, _) = machine.run(&b, &["verdict", "list"]);
    assert!(
        out.starts_with(&format!(
            "{id} not-needed pos_x on 17.0 {key}: 17.0 has no such bug\n"
        )),
        "{out}"
    );
    let (_, out, _) = machine.run(&b, &["verdict", "show", &id]);
    assert!(out.contains("kind = \"not-needed\""), "{out}");
    assert!(out.contains("[verdict.recheck]"), "{out}");

    let (_, out, _) = machine.run(&a, &["--json", "suggest"]);
    assert!(
        out.contains(&format!("\"verdict\":{{\"id\":\"{id}\"")),
        "{out}"
    );
    stale_confirmed_and_revoked(&machine, &a, &b, &id);
    // A project mute in the verdict repository stays with its project.
    let (exit, out, _) = machine.run(
        &a,
        &[
            "verdict",
            "add",
            &key,
            "--addon",
            "pos_x",
            "--kind",
            "not-wanted",
            "--reason",
            "a only",
        ],
    );
    assert_eq!(exit, Exit::Success, "{out}");
    let (_, out, _) = machine.run(&a, &["suggest"]);
    assert!(
        out.ends_with("1 suppressed by verdicts and mutes)\n"),
        "{out}"
    );
    let (_, out, _) = machine.run(&b, &["suggest"]);
    assert!(
        out.ends_with("0 suppressed by verdicts and mutes)\n"),
        "{out}"
    );
    // A file in the repository that is not one of verdicts is reported.
    let bad = machine
        .tmp
        .path()
        .join("cache/verdicts/verdicts/17.0/bad.toml");
    std::fs::write(&bad, "verdict = 1\n").unwrap();
    let (_, _, err) = machine.run(&b, &["--offline", "suggest"]);
    assert!(err.contains("warning: verdicts: "), "{err}");
}

/// After a verdict `id` recorded from `a`: the target code changes, the
/// verdict is confirmed, then revoked from `b` after someone else pushed.
fn stale_confirmed_and_revoked(machine: &Machine, a: &Path, b: &Path, id: &str) {
    let bare = machine.tmp.path().join("verdicts.git");
    // The touched file changes on 17.0: the suggestion resurfaces.
    change(
        &machine.up,
        &[("pos_x/b.py", "b, the 17.0 way\n")],
        "[IMP] pos_x: b",
    );
    let (_, out, _) = machine.run(a, &["--fresh", "suggest"]);
    assert!(
        out.contains(
            "; previously declined (not-needed): 17.0 has no such bug (target code changed since "
        ),
        "{out}"
    );
    let (_, out, _) = machine.run(a, &["verdict", "list", "--stale"]);
    assert!(out.ends_with("[stale: target code changed]\n"), "{out}");
    let (exit, out, _) = machine.run(a, &["verdict", "confirm", id]);
    assert_eq!(
        (exit, out),
        (
            Exit::Success,
            format!("confirmed {id} against the current 17.0 code\n")
        )
    );
    let (_, out, _) = machine.run(a, &["verdict", "list", "--stale"]);
    assert_eq!(out, "");
    let (_, out, _) = machine.run(a, &["suggest"]);
    assert!(
        out.ends_with("1 suppressed by verdicts and mutes)\n"),
        "{out}"
    );

    // Someone else pushed meanwhile: the revocation rebases on it.
    let other = machine.tmp.path().join("other");
    git(
        machine.tmp.path(),
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    change(&other, &[("README.md", "verdicts\n")], "readme");
    git(&other, &["push", "--quiet", "origin", "HEAD"]);
    let (exit, out, err) = machine.run(
        b,
        &[
            "--offline",
            "verdict",
            "revoke",
            id,
            "--reason",
            "wrong call",
        ],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    let (_, out, _) = machine.run(
        b,
        &["verdict", "list", "--addon", "pos_x", "--series", "17.0"],
    );
    assert!(out.ends_with("[revoked]\n"), "{out}");
    let (_, out, _) = machine.run(a, &["suggest"]);
    assert!(
        out.ends_with("0 suppressed by verdicts and mutes)\n"),
        "{out}"
    );
    let (_, out, _) = machine.run(a, &["verdict", "sync"]);
    assert!(out.ends_with("is up to date\n"), "{out}");
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario, step by step"
)]
fn project_mutes_without_a_verdict_repository_and_failures() {
    let machine = Machine::new();
    let a = machine.project("a");
    let key = machine.key.clone();
    let local = machine.env.clone().with(
        "ODX_CONFIG_DIR",
        machine.tmp.path().join("none").display().to_string(),
    );
    let add = |extra: &[&str]| {
        let mut args = vec![
            "verdict",
            "add",
            &key,
            "--addon",
            "pos_x",
            "--reason",
            "not for us",
        ];
        args.extend_from_slice(extra);
        odx(&local, &a, &args)
    };
    let (exit, _, err) = add(&["--kind", "not-needed"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("organisation verdicts need a verdict repository"),
        "{err}"
    );
    let (exit, _, err) = add(&["--kind", "not-wanted", "--scope", "environment"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("needs --env"), "{err}");
    let (exit, out, _) = add(&[
        "--kind",
        "deferred",
        "--scope",
        "environment",
        "--env",
        "uat",
        "--until",
        "2099-01-01",
    ]);
    assert_eq!(exit, Exit::Success, "{out}");
    // An environment mute does not hide it elsewhere.
    let (_, out, _) = odx(&local, &a, &["suggest"]);
    assert!(
        out.ends_with("0 suppressed by verdicts and mutes)\n"),
        "{out}"
    );
    let (exit, out, _) = add(&["--kind", "not-wanted", "--no-recheck"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.starts_with("recorded m:17.0:pos_x:"), "{out}");
    let mutes = std::fs::read_to_string(a.join(".odx/mutes.toml")).unwrap();
    assert_eq!(mutes.matches("[[mute]]").count(), 2, "{mutes}");
    let (_, out, _) = odx(&local, &a, &["suggest"]);
    assert!(
        out.ends_with("1 suppressed by verdicts and mutes)\n"),
        "{out}"
    );
    let (_, out, _) = odx(&local, &a, &["verdict", "sync"]);
    assert!(out.starts_with("no verdict repository configured"), "{out}");

    let (exit, _, err) = odx(
        &local,
        &a,
        &[
            "verdict", "add", "nope#1", "--addon", "pos_x", "--kind", "declined", "--reason", "x",
        ],
    );
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("no suggestion nope#1 for pos_x on 17.0"),
        "{err}"
    );
    for args in [
        &[
            "verdict", "add", "x", "--addon", "pos_x", "--kind", "declined", "--reason", " ",
        ][..],
        &["verdict", "show", "nope"],
        &["verdict", "revoke", "nope", "--reason", "x"],
        &["verdict", "revoke", "nope", "--reason", " "],
        &["verdict", "confirm", "nope"],
        &["verdict", "upstream", "nope", "--public-reason", "x"],
    ] {
        let (exit, _, _) = odx(&local, &a, args);
        assert_eq!(exit, Exit::Usage, "{args:?}");
    }
    let (exit, _, _) = odx(
        &local,
        &machine.tmp.path().join("none"),
        &["verdict", "list"],
    );
    assert_eq!(exit, Exit::Usage);
    // A mute recorded without a re-check cannot be confirmed.
    let mutes = odx_verdicts::read(&a.join(".odx/mutes.toml")).unwrap();
    let (exit, _, err) = odx(&local, &a, &["verdict", "confirm", &mutes[1].id]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("is not in the cache"), "{err}");
    let (exit, _, err) = odx(
        &local,
        &a,
        &["verdict", "upstream", &mutes[1].id, "--public-reason", "x"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("does not record its repository"), "{err}");
    let (exit, out, _) = odx(
        &local,
        &a,
        &["verdict", "revoke", &mutes[0].id, "--reason", "done"],
    );
    assert_eq!(
        (exit, out),
        (Exit::Success, format!("revoked {}\n", mutes[0].id))
    );
    let pyproject = std::fs::read_to_string(a.join("pyproject.toml")).unwrap();
    std::fs::write(
        a.join("pyproject.toml"),
        pyproject.replace(
            "default_from",
            "migration_commit_patterns = [\"(\"]\ndefault_from",
        ),
    )
    .unwrap();
    let (exit, _, _) = odx(
        &local,
        &a,
        &[
            "verdict", "add", &key, "--addon", "pos_x", "--kind", "declined", "--reason", "x",
        ],
    );
    assert_eq!(exit, Exit::Usage);
    std::fs::write(a.join("pyproject.toml"), pyproject).unwrap();
    std::fs::write(a.join(".odx/mutes.toml"), "mute = 1\n").unwrap();
    let (_, _, err) = odx(&local, &a, &["suggest"]);
    assert!(err.contains("warning: verdicts: "), "{err}");
    // A user configuration that is not TOML.
    std::fs::create_dir_all(machine.tmp.path().join("bad")).unwrap();
    std::fs::write(machine.tmp.path().join("bad/config.toml"), "[verdicts\n").unwrap();
    let bad = machine.env.clone().with(
        "ODX_CONFIG_DIR",
        machine.tmp.path().join("bad").display().to_string(),
    );
    let (exit, _, _) = odx(&bad, &a, &["verdict", "list"]);
    assert_eq!(exit, Exit::Usage);
}

/// `odx verdict add` of an organisation verdict on pull request 2; its id.
fn organisation_verdict(machine: &Machine, project: &Path, key: &str, reason: &str) -> String {
    machine.run(project, &["suggest"]);
    let (exit, out, err) = machine.run(
        project,
        &[
            "verdict",
            "add",
            key,
            "--addon",
            "pos_x",
            "--kind",
            "not-needed",
            "--scope",
            "organisation",
            "--reason",
            reason,
        ],
    );
    assert_eq!(exit, Exit::Success, "{err}");
    out.split_whitespace().nth(1).unwrap().to_owned()
}

#[test]
fn verdicts_go_upstream_as_blacklist_entries() {
    let machine = Machine::new();
    let a = machine.project("a");
    let id = organisation_verdict(&machine, &a, &machine.key, "client X does not use it");
    let upstream = |extra: &[&str]| {
        let mut args = vec!["verdict", "upstream", id.as_str(), "--public-reason"];
        args.extend_from_slice(extra);
        machine.run(&a, &args)
    };
    let (exit, _, err) = upstream(&[" "]);
    assert_eq!(exit, Exit::Usage, "{err}");
    let (exit, out, err) = upstream(&["fixed differently on 17.0"]);
    assert_eq!(exit, Exit::Success, "{err}");
    assert!(
        out.contains(": [FW] pos_x: blacklist 1 port suggestion(s)\n"),
        "{out}"
    );
    assert!(
        out.contains("  public reason: fixed differently on 17.0\n"),
        "{out}"
    );
    assert!(
        out.contains("  private reason, which stays private: client X does not use it\n"),
        "{out}"
    );
    // Only the public reason is written.
    let sanitized: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let worktree = machine.tmp.path().join("cache/upstream").join(sanitized);
    let file =
        std::fs::read_to_string(worktree.join(".oca/oca-port/blacklist/pos_x.json")).unwrap();
    assert!(
        file.contains(&format!(
            "\"{}\": \"fixed differently on 17.0\"",
            machine.key
        )),
        "{file}"
    );
    assert!(!file.contains("client X"), "{file}");
    // Nothing leaves without --yes; a repository off the forges cannot
    // take a pull request.
    let (exit, out, _) = upstream(&["fixed", "--open-pr"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        out.ends_with("run again with --yes to push it and open the pull request\n"),
        "{out}"
    );
    let (exit, _, err) = upstream(&["fixed", "--open-pr", "--yes"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("is not on a forge"), "{err}");
    // A target branch where the blacklist cannot be written.
    change(&machine.up, &[(".oca", "")], "no blacklists");
    machine.run(&a, &["--fresh", "suggest"]);
    let (exit, _, err) = upstream(&["fixed"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("blacklist"), "{err}");
    // Without the target branch in the cache, run odx suggest first.
    let common = git(&worktree, &["rev-parse", "--git-common-dir"]);
    let common = worktree.join(common);
    let refs = git(&common, &["for-each-ref", "--format=%(refname)"]);
    for name in refs.lines().filter(|name| name.ends_with("/17.0")) {
        git(&common, &["update-ref", "-d", name]);
    }
    let (exit, _, err) = upstream(&["fixed"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("run odx suggest first"), "{err}");
}

/// A forge on localhost answering from `routes` by method and path, 404
/// otherwise.
fn routed(routes: Vec<(&'static str, u16, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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
            let text = String::from_utf8_lossy(&request).into_owned();
            let mut words = text.split_whitespace();
            let method = words.next().unwrap_or("GET");
            let path = words.next().unwrap_or("/").split('?').next().unwrap_or("/");
            let key = format!("{method} {path}");
            let (status, body) = routes
                .iter()
                .find(|(route, _, _)| *route == key)
                .map_or((404, String::new()), |(_, status, body)| {
                    (*status, body.clone())
                });
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
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario, step by step"
)]
fn verdicts_open_their_upstream_pull_request() {
    let machine = Machine::new();
    let api = routed(vec![
        ("GET /api/v3/meta", 200, "{}".to_owned()),
        (
            "GET /api/v3/repos/OCA/pos",
            200,
            r#"{"permissions":{"push":true}}"#.to_owned(),
        ),
        (
            "POST /api/v3/repos/OCA/pos/pulls",
            201,
            r#"{"number":8,"html_url":"https://forge.example/OCA/pos/pull/8"}"#.to_owned(),
        ),
    ]);
    let project = machine.tmp.path().join("forged");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        format!(
            "[tool.odx]\nodoo_version = \"17.0\"\nproject_id = \"forged\"\n\n[tool.odx.port]\ndefault_from = [\"16.0\"]\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{api}/OCA/pos.git\"\naddons = [\"pos_x\"]\nmerges = []\n"
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_odx"));
        command
            .current_dir(&project)
            .args(args)
            .env("GIT_CONFIG_COUNT", "1")
            .env(
                "GIT_CONFIG_KEY_0",
                format!("url.{}.insteadOf", file_url(&machine.up)),
            )
            .env("GIT_CONFIG_VALUE_0", format!("{api}/OCA/pos.git"));
        for var in ["ODX_CACHE_DIR", "ODX_CONFIG_DIR"] {
            command.env(var, odx_core::Env::var(&machine.env, var).unwrap());
        }
        let out = command.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (ok, _, err) = run(&["aggregate"]);
    assert!(ok, "{err}");
    let (ok, out, err) = run(&["suggest"]);
    assert!(ok, "{err}");
    assert!(out.contains("OCA/pos#2 from 16.0: portable"), "{out}");
    let (ok, out, err) = run(&[
        "verdict",
        "add",
        "OCA/pos#2",
        "--addon",
        "pos_x",
        "--kind",
        "not-needed",
        "--scope",
        "organisation",
        "--reason",
        "client X",
    ]);
    assert!(ok, "{err}");
    let id = out.split_whitespace().nth(1).unwrap().to_owned();
    let (ok, out, err) = run(&[
        "verdict",
        "upstream",
        &id,
        "--public-reason",
        "fixed differently on 17.0",
        "--open-pr",
        "--yes",
    ]);
    assert!(ok, "{err}");
    assert!(
        out.ends_with(&format!(
            "pushed to {api}/OCA/pos.git\nopened https://forge.example/OCA/pos/pull/8 as a draft\n\
             recorded on {id}: blacklisted upstream in OCA/pos#8\n"
        )),
        "{out}"
    );
    git(
        &machine.up,
        &["rev-parse", "--verify", "--quiet", "17.0-blacklist-pos_x"],
    );
    let (_, out, _) = run(&["verdict", "show", &id]);
    assert!(out.contains("pr = \"OCA/pos#8\""), "{out}");
    let again = [
        "verdict",
        "upstream",
        &id,
        "--public-reason",
        "again",
        "--open-pr",
        "--yes",
    ];
    // The branch upstream has another commit: the push is refused.
    let (ok, _, err) = run(&again);
    assert!(!ok);
    assert!(err.contains("git push failed"), "{err}");
    // Proposed, but the verdict repository cannot record it.
    git(&machine.up, &["branch", "-D", "17.0-blacklist-pos_x"]);
    let verdicts = machine.tmp.path().join("verdicts.git");
    std::fs::rename(&verdicts, machine.tmp.path().join("gone.git")).unwrap();
    let (ok, out, err) = run(&again);
    assert!(!ok);
    assert_eq!(out, "");
    assert!(
        err.contains("opened https://forge.example/OCA/pos/pull/8, but could not record it on "),
        "{err}"
    );
}

/// A forge on localhost that calls every repository `answer` (a JSON
/// body, or a 404 when empty).
fn forge(answer: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0_u8; 2048];
            let n = stream.read(&mut request).unwrap_or(0);
            let text = String::from_utf8_lossy(request.get(..n).unwrap_or_default()).into_owned();
            let path = text.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let body = if path == "/api/v3/meta" { "{}" } else { answer };
            let status = if body.is_empty() { 404 } else { 200 };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario, step by step"
)]
fn a_public_verdict_repository_is_refused() {
    let machine = Machine::new();
    let a = machine.project("a");
    let url = format!("{}/odoo/verdicts.git", forge(r#"{"private": false}"#));
    let config = machine.tmp.path().join("public");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!("[verdicts]\nrepo = \"{url}\"\n"),
    )
    .unwrap();
    let env = machine
        .env
        .clone()
        .with("ODX_CONFIG_DIR", config.display().to_string());
    // A clone of it without commits yet.
    let clone = machine.tmp.path().join("cache").join("verdicts");
    std::fs::create_dir_all(&clone).unwrap();
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["remote", "add", "origin", &url]);
    let key = machine.key.clone();
    let (exit, _, err) = odx(
        &env,
        &a,
        &[
            "verdict",
            "add",
            &key,
            "--addon",
            "pos_x",
            "--kind",
            "not-needed",
            "--reason",
            "x",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("is not private"), "{err}");
    // A private repository that cannot be pushed to, and one the forge
    // does not show.
    for (answer, message) in [
        (r#"{"private": true}"#, "error: "),
        ("", "cannot check that"),
    ] {
        let url = format!("{}/odoo/verdicts.git", forge(answer));
        std::fs::write(
            config.join("config.toml"),
            format!("[verdicts]\nrepo = \"{url}\"\n"),
        )
        .unwrap();
        git(&clone, &["remote", "set-url", "origin", &url]);
        let (exit, _, err) = odx(
            &env,
            &a,
            &[
                "--offline",
                "verdict",
                "add",
                &key,
                "--addon",
                "pos_x",
                "--kind",
                "deferred",
                "--scope",
                "project",
                "--reason",
                "x",
            ],
        );
        assert_eq!(exit, Exit::Failure, "{err}");
        assert!(err.contains(message), "{err}");
    }
    // The clone holds the entry the private repository refused: revoking
    // it cannot be published either; a project directory in the way stops
    // the next entry; online, a remote that is gone stops syncing.
    let mutes = odx_verdicts::read(&clone.join("projects/a/mutes.toml")).unwrap();
    let (exit, _, _) = odx(&env, &a, &["--offline", "verdict", "confirm", &mutes[0].id]);
    assert_eq!(exit, Exit::Failure);
    let (exit, _, _) = odx(
        &env,
        &a,
        &[
            "--offline",
            "verdict",
            "revoke",
            &mutes[0].id,
            "--reason",
            "r",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    std::fs::remove_dir_all(clone.join("projects")).unwrap();
    std::fs::write(clone.join("projects"), "").unwrap();
    let (exit, _, err) = odx(
        &env,
        &a,
        &[
            "--offline",
            "verdict",
            "add",
            &key,
            "--addon",
            "pos_x",
            "--kind",
            "declined",
            "--reason",
            "x",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("mutes.toml"), "{err}");
    let (exit, _, _) = odx(
        &env,
        &a,
        &[
            "verdict", "add", &key, "--addon", "pos_x", "--kind", "declined", "--reason", "x",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    // Offline, a missing clone is no error; online, a clone that fails is.
    std::fs::remove_dir_all(&clone).unwrap();
    std::fs::write(
        config.join("config.toml"),
        format!(
            "[verdicts]\nrepo = \"{}\"\n",
            file_url(&machine.tmp.path().join("gone.git"))
        ),
    )
    .unwrap();
    let (exit, _, err) = odx(&env, &a, &["--offline", "verdict", "sync"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    let (exit, _, err) = odx(&env, &a, &["verdict", "sync"]);
    assert_eq!(exit, Exit::Failure, "{err}");
    let (exit, _, _) = odx(&env, &a, &["verdict", "revoke", "x", "--reason", "r"]);
    assert_eq!(exit, Exit::Failure);
    let (_, _, err) = odx(&env, &a, &["suggest"]);
    assert!(err.contains("warning: verdicts: "), "{err}");
}
