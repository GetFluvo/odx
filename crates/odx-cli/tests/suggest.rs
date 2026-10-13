//! `odx suggest` on a project aggregating a repository with three series.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Jane")
        .env("GIT_AUTHOR_EMAIL", "jane@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
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

fn merged(dir: &Path, series: &str, n: u32, files: &[(&str, &str)], message: &str) {
    git(dir, &["checkout", "--quiet", "-b", &format!("pr{n}")]);
    change(dir, files, message);
    git(dir, &["checkout", "--quiet", series]);
    git(
        dir,
        &[
            "merge",
            "--quiet",
            "--no-ff",
            &format!("pr{n}"),
            "-m",
            &format!("Merge PR #{n} into {series}"),
        ],
    );
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    url: String,
    env: MapEnv,
}

impl Project {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=16.0"]);
        change(
            &up,
            &[
                ("pos_x/__manifest__.py", "{'version': '16.0'}"),
                ("pos_x/a.py", "a\n"),
            ],
            "[MIG] pos_x: 16.0",
        );
        merged(
            &up,
            "16.0",
            1,
            &[("pos_x/a.py", "a\nfix\n")],
            "[FIX] pos_x: a",
        );
        merged(&up, "16.0", 2, &[("pos_x/b.py", "b\n")], "[FIX] pos_x: b");
        merged(&up, "16.0", 3, &[("pos_x/c.py", "c\n")], "[IMP] pos_x: c");
        merged(&up, "16.0", 4, &[("other/x.py", "x\n")], "[FIX] other");
        git(&up, &["checkout", "--quiet", "--orphan", "17.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[
                ("pos_x/__manifest__.py", "{'version': '17.0'}"),
                ("pos_x/a.py", "a\n"),
            ],
            "[MIG] pos_x: 17.0",
        );
        change(&up, &[("pos_x/a.py", "a\nfix\n")], "[FW] pos_x: a");
        change(
            &up,
            &[(
                ".oca/oca-port/blacklist/pos_x.json",
                "{\"pull_requests\": {}}",
            )],
            "[BL] pos_x",
        );
        git(&up, &["checkout", "--quiet", "--orphan", "18.0"]);
        git(&up, &["rm", "-r", "--quiet", "-f", "."]);
        change(
            &up,
            &[
                ("pos_x/__manifest__.py", "{'version': '18.0'}"),
                ("pos_x/a.py", "a\nfix\n"),
            ],
            "[MIG] pos_x: 18.0",
        );
        merged(&up, "18.0", 9, &[("pos_x/d.py", "d\n")], "[IMP] pos_x: d");
        git(&up, &["checkout", "--quiet", "17.0"]);
        let url = file_url(&up);
        // Blacklist #3 on 17.0, naming the repository as odx does.
        let blacklist = format!(
            "{{\n  \"pull_requests\": {{\n    \"{url}#3\": \"not wanted on 17.0\"\n  }}\n}}\n"
        );
        change(
            &up,
            &[(".oca/oca-port/blacklist/pos_x.json", &blacklist)],
            "[BL] pos_x: #3",
        );
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("pyproject.toml"),
            format!(
                "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.environments.env.prod]\n\n[tool.odx.environments.env.next]\nseries = \"18.0\"\n\n[tool.odx.index.renames]\n\"17.0\" = {{ old_x = \"pos_x\" }}\n\n[tool.odx.index.overrides]\nweb_y = {{ \"17.0\" = \"{url}\", \"16.0\" = \"{gone}\" }}\nzz_q = {{ \"17.0\" = \"{url}\" }}\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{url}\"\naddons = [\"pos_x\"]\nmerges = []\n",
                gone = file_url(&tmp.path().join("gone"))
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("uv.lock"),
            "version = 1\n\n[[package]]\nname = \"odoo-addon-web-y\"\nversion = \"17.0.1.0.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n\n[[package]]\nname = \"odoo-addon-zz-q\"\nversion = \"17.0.1.0.0\"\nsource = { registry = \"https://pypi.org/simple\" }\n",
        )
        .unwrap();
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                tmp.path().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                tmp.path().join("config").display().to_string(),
            );
        Self {
            _tmp: tmp,
            root,
            url,
            env,
        }
    }

    fn odx(&self, args: &[&str]) -> (Exit, String, String) {
        let mut command_line = vec!["odx"];
        command_line.extend_from_slice(args);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(
            command_line,
            &OsFs,
            &self.env,
            &self.root,
            &mut out,
            &mut err,
        );
        (
            exit,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }
}

#[test]
fn suggestions_from_the_series_below_and_above() {
    let project = Project::new();
    let (exit, _, err) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Success, "{err}");
    let (exit, out, err) = project.odx(&["suggest", "--env", "prod", "--addon", "pos_*"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    let url = &project.url;
    assert_eq!(
        out,
        format!(
            "pos_x on 17.0:\n  \
             {url}#2 from 16.0: portable - [FIX] pos_x: b (1 commits, Jane)\n  \
             orphaned_commits from 16.0: migration commit - [MIG] pos_x: 16.0 (1 commits, Jane)\n  \
             {url}#9 from 18.0: portable - [IMP] pos_x: d (1 commits, Jane)\n  \
             orphaned_commits from 18.0: migration commit - [MIG] pos_x: 18.0 (1 commits, Jane)\n\
             4 suggestions (1 blacklisted upstream)\n"
        )
    );
    let (_, out, _) = project.odx(&[
        "suggest",
        "--series",
        "17.0",
        "--from",
        "16.0",
        "--show-muted",
    ]);
    assert!(out.contains(&format!("{url}#3 from 16.0: blacklisted - [IMP] pos_x: c (1 commits, Jane); not wanted on 17.0\n")), "{out}");
    let (_, out, _) = project.odx(&[
        "--json", "suggest", "--from", "16.0", "--env", "prod", "--addon", "pos_x",
    ]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["suggestions"][0]["class"], "blacklisted");
    assert_eq!(value["suggestions"][1]["pr"], 2);
    assert_eq!(value["suggestions"][1]["commits"][0]["author"], "Jane");
    // Every environment: 18.0 (next) looks on 17.0 and 19.0.
    let (_, out, err) = project.odx(&["--offline", "suggest"]);
    assert!(out.contains("pos_x on 18.0:\n"), "{out}{err}");
    let (_, out, _) = project.odx(&["suggest", "--addon", "nothing_*"]);
    assert_eq!(out, "0 suggestions (0 blacklisted upstream)\n");
    // Nothing new on the series itself.
    let (_, out, _) = project.odx(&[
        "suggest", "--series", "17.0", "--from", "17.0", "--addon", "pos_x",
    ]);
    assert_eq!(out, "0 suggestions (0 blacklisted upstream)\n");
}

#[test]
fn suggest_failures() {
    let project = Project::new();
    project.odx(&["aggregate"]);
    let (exit, _, err) = project.odx(&["suggest", "--env", "nope"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no environment named \"nope\""), "{err}");
    // A package whose 16.0 repository is gone, one without a 16.0 home,
    // and a series the repository does not have.
    let (_, _, err) = project.odx(&["suggest", "--series", "17.0", "--from", "16.0"]);
    assert!(err.contains("warning: web_y on 16.0: "), "{err}");
    assert!(!err.contains("zz_q"), "{err}");
    // A series the repository does not have.
    let (exit, _, err) = project.odx(&["suggest", "--series", "17.0", "--from", "15.0"]);
    assert_eq!(exit, Exit::Success);
    assert!(err.contains("warning: pos_x on 15.0: "), "{err}");
    let pyproject = std::fs::read_to_string(project.root.join("pyproject.toml")).unwrap();
    std::fs::write(
        project.root.join("pyproject.toml"),
        pyproject.replacen(
            "[tool.odx.index.renames]",
            "[tool.odx.port]\nmigration_commit_patterns = [\"(\"]\n\n[tool.odx.index.renames]",
            1,
        ),
    )
    .unwrap();
    let (exit, _, err) = project.odx(&["suggest"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("migration_commit_patterns"), "{err}");
    let (exit, _, _) = project.odx(&["-C", "..", "suggest"]);
    assert_eq!(exit, Exit::Usage);
    // Without environments, the project's series; sources from the config.
    std::fs::write(
        project.root.join("pyproject.toml"),
        format!(
            "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.port]\ndefault_from = [\"16.0\"]\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}\"\naddons = [\"pos_x\"]\nmerges = []\n",
            project.url
        ),
    )
    .unwrap();
    let (_, out, _) = project.odx(&["suggest"]);
    assert!(
        out.ends_with("2 suggestions (1 blacklisted upstream)\n"),
        "{out}"
    );
}
