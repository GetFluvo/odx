//! Golden lockfiles: `odx lock` on fixed upstream histories writes exactly
//! the `odx.lock` in the snapshots. Commits are made with fixed authors and
//! dates, so every commit id, tree and patch id in the snapshots is stable;
//! a change to any of them is a change to the lockfile format or to what
//! odx locks.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

/// Run git in `dir` with a fixed identity, date and no user configuration.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        // A file that does not exist: no user configuration, on every platform.
        .env(
            "GIT_CONFIG_GLOBAL",
            std::env::temp_dir().join("odx-corpus-no-gitconfig"),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
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

/// A scratch directory with upstream repositories and a project.
struct Corpus {
    tmp: tempfile::TempDir,
}

impl Corpus {
    fn new() -> Self {
        Self {
            tmp: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// A repository `name` with branch 17.0 holding `base.txt`.
    fn upstream(&self, name: &str) -> Upstream {
        let dir = self.root().join("upstream").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "--quiet", "--initial-branch=17.0"]);
        let upstream = Upstream { dir };
        upstream.commit("base.txt", name);
        upstream
    }

    /// A clone of `from` named `name`, as a fork on another remote.
    fn fork(&self, from: &Upstream, name: &str) -> Upstream {
        let dir = self.root().join("upstream").join(name);
        git(
            self.root(),
            &[
                "clone",
                "--quiet",
                &from.dir.display().to_string(),
                &dir.display().to_string(),
            ],
        );
        Upstream { dir }
    }

    /// `odx lock` with `pyproject`, where `<name>` stands for the URL of
    /// upstream `name`; returns `odx.lock` with the scratch path replaced.
    fn lock(&self, pyproject: &str) -> String {
        let project = self.root().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut text = pyproject.to_owned();
        for entry in std::fs::read_dir(self.root().join("upstream")).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            text = text.replace(&format!("<{name}>"), &file_url(&path));
        }
        std::fs::write(project.join("pyproject.toml"), text).unwrap();
        let env = MapEnv::default()
            .with(
                "ODX_CACHE_DIR",
                self.root().join("cache").display().to_string(),
            )
            .with(
                "ODX_CONFIG_DIR",
                self.root().join("config").display().to_string(),
            );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let exit = odx_cli::run_with_env(
            ["odx", "lock", "--no-uv"],
            &OsFs,
            &env,
            &project,
            &mut out,
            &mut err,
        );
        assert_eq!(exit, Exit::Success, "{}", String::from_utf8_lossy(&err));
        std::fs::read_to_string(project.join("odx.lock"))
            .unwrap()
            .replace(&file_url(self.root()), "file://<corpus>")
    }
}

struct Upstream {
    dir: PathBuf,
}

impl Upstream {
    /// Commit `file` with `content` on the branch checked out.
    fn commit(&self, file: &str, content: &str) -> String {
        std::fs::write(self.dir.join(file), content).unwrap();
        git(&self.dir, &["add", file]);
        git(
            &self.dir,
            &["commit", "--quiet", "-m", &format!("{file}: {content}")],
        );
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    /// Pull request `n`: one commit of `file` with `content` on top of 17.0.
    fn pull_request(&self, n: u32, file: &str, content: &str) -> String {
        git(&self.dir, &["checkout", "--quiet", "--detach", "17.0"]);
        let sha = self.commit(file, content);
        git(
            &self.dir,
            &["update-ref", &format!("refs/pull/{n}/head"), &sha],
        );
        git(&self.dir, &["checkout", "--quiet", "17.0"]);
        sha
    }

    /// Branch `name`: one commit of `file` with `content` on top of 17.0.
    fn branch(&self, name: &str, file: &str, content: &str) -> String {
        git(&self.dir, &["checkout", "--quiet", "-b", name, "17.0"]);
        let sha = self.commit(file, content);
        git(&self.dir, &["checkout", "--quiet", "17.0"]);
        sha
    }
}

#[test]
fn base_only() {
    let corpus = Corpus::new();
    corpus.upstream("pos");
    let lock = corpus.lock(
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"<pos>\"\n",
    );
    insta::assert_snapshot!(lock);
}

#[test]
fn pull_request_commit_and_branch_on_another_remote() {
    let corpus = Corpus::new();
    let pos = corpus.upstream("pos");
    pos.pull_request(1, "one.txt", "pull request one");
    let commit = pos.branch("17.0-wip", "two.txt", "a commit");
    let fork = corpus.fork(&pos, "fork");
    fork.branch("17.0-fix", "three.txt", "a fix");
    let lock = corpus.lock(&format!(
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"<pos>\"\nmerges = [\n  {{ pr = 1 }},\n  {{ sha = \"{commit}\" }},\n  {{ branch = \"17.0-fix\", remote = \"<fork>\" }},\n]\n"
    ));
    insta::assert_snapshot!(lock);
}

#[test]
fn a_skipped_conflict_is_recorded() {
    let corpus = Corpus::new();
    let pos = corpus.upstream("pos");
    pos.pull_request(1, "same.txt", "first");
    pos.pull_request(2, "same.txt", "second");
    let lock = corpus.lock(
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"<pos>\"\nmerges = [{ pr = 1 }, { pr = 2, on_conflict = \"skip\" }]\n",
    );
    insta::assert_snapshot!(lock);
}

#[test]
fn repositories_are_sorted_and_unnamed_ones_keyed_by_url() {
    let corpus = Corpus::new();
    corpus.upstream("web");
    corpus.upstream("pos");
    let accounting = corpus.upstream("accounting");
    accounting.pull_request(7, "seven.txt", "pull request seven");
    let lock = corpus.lock(
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.repositories]]\nname = \"web\"\nurl = \"<web>\"\n\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"<pos>\"\n\n[[tool.odx.repositories]]\nurl = \"<accounting>\"\nmerges = [{ pr = 7 }]\n",
    );
    insta::assert_snapshot!(lock);
}
