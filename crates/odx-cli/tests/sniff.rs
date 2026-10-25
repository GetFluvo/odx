//! `odx sniff`, the migration report at the end of `odx aggregate`, and
//! `odx changelog`.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::path::{Path, PathBuf};
use std::process::Command;

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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn manifest(version: &str) -> String {
    format!("{{\n    \"name\": \"X\",\n    \"version\": \"{version}\",\n}}\n")
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
    tmp: tempfile::TempDir,
    root: PathBuf,
    url: String,
    env: MapEnv,
    pr1: String,
}

impl Project {
    /// Upstream `pos` on 17.0 with `pos_x` (and an old migration script)
    /// and `pos_y`; pull request 1 adds a pre-migration script to `pos_x`
    /// with a version bump, bumps `pos_y` without scripts, adds an
    /// OpenUpgrade script and removes the README; pull request 2 (on top of
    /// 1) adds a post script to `pos_y` and fixes the one of `pos_x`.
    fn new(extra: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let up = tmp.path().join("up");
        std::fs::create_dir_all(&up).unwrap();
        git(&up, &["init", "--quiet", "--initial-branch=17.0"]);
        write(&up.join("pos_x/__manifest__.py"), &manifest("17.0.1.0.0"));
        write(&up.join("pos_x/migrations/17.0.1.0.0/post-old.py"), "old\n");
        write(&up.join("pos_y/__manifest__.py"), &manifest("17.0.1.0.0"));
        write(&up.join("README.md"), "readme\n");
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "base"]);
        std::fs::remove_file(up.join("README.md")).unwrap();
        git(&up, &["checkout", "--quiet", "-b", "pr"]);
        write(&up.join("pos_x/__manifest__.py"), &manifest("17.0.1.1.0"));
        write(
            &up.join("pos_x/migrations/17.0.1.1.0/pre-migrate.py"),
            "pre\n",
        );
        write(&up.join("pos_y/__manifest__.py"), &manifest("17.0.1.0.1"));
        write(
            &up.join("openupgrade_scripts/scripts/sale/17.0.1.0/end-migration.py"),
            "end\n",
        );
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "pr 1"]);
        let pr1 = git(&up, &["rev-parse", "HEAD"]);
        write(&up.join("pos_y/migrations/17.0.1.0.1/post-y.py"), "post\n");
        write(
            &up.join("pos_x/migrations/17.0.1.1.0/pre-migrate.py"),
            "pre, fixed\n",
        );
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "pr 2"]);
        let pr2 = git(&up, &["rev-parse", "HEAD"]);
        git(&up, &["update-ref", "refs/pull/1/head", &pr1]);
        git(&up, &["update-ref", "refs/pull/2/head", &pr2]);
        git(&up, &["checkout", "--quiet", "17.0"]);
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "--quiet"]);
        let url = file_url(&up);
        let env = MapEnv::default().with(
            "ODX_CACHE_DIR",
            tmp.path().join("cache").display().to_string(),
        );
        let project = Self {
            tmp,
            root,
            url,
            env,
            pr1,
        };
        project.configure(extra, "{ pr = 1 }");
        project
    }

    fn configure(&self, extra: &str, merges: &str) {
        write(
            &self.root.join("pyproject.toml"),
            &format!(
                "[tool.odx]\nodoo_version = \"17.0\"\nmerge_strategy = \"merge\"\n{extra}\n\
                 [[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{}\"\nmerges = [{merges}]\n",
                self.url
            ),
        );
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

    fn commit(&self) {
        git(&self.root, &["add", "pyproject.toml", "odx.lock"]);
        git(&self.root, &["commit", "--quiet", "-m", "lock"]);
    }
}

const PR1: &str = "\
migrations in pos (repository):
  pos_x 17.0.1.1.0 (was 17.0.1.0.0)
    pre  added    pos_x/migrations/17.0.1.1.0/pre-migrate.py
  pos_y 17.0.1.0.1 (was 17.0.1.0.0): version changed without migration scripts
  sale
    end  added    openupgrade_scripts/scripts/sale/17.0.1.0/end-migration.py
";

#[test]
fn aggregate_and_sniff_report_new_migration_scripts() {
    let project = Project::new("");
    let (exit, out, err) = project.odx(&["aggregate"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    assert!(out.contains(PR1), "{out}");
    assert!(!out.contains("post-old"), "{out}");

    let (exit, out, err) = project.odx(&["sniff"]);
    assert_eq!((exit, out.as_str(), err.as_str()), (Exit::Success, PR1, ""));
    let (exit, _, err) = project.odx(&["sniff", "--fail-on-new"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(err, "error: 2 migration scripts (fail_on_new)\n");

    let (_, out, _) = project.odx(&["--json", "sniff"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["scripts"], 2);
    let addons = &value["sources"][0]["addons"];
    assert_eq!(addons[0]["scripts"][0]["phase"], "pre");
    assert_eq!(addons[0]["scripts"][0]["change"], "added");
    assert_eq!(addons[1]["bumped_without_scripts"], true);

    // Against the committed lockfile: only what pull request 2 adds.
    project.commit();
    project.configure("", "{ pr = 1 }, { pr = 2 }");
    let (exit, out, _) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Success, "{out}");
    let (exit, out, err) = project.odx(&["sniff", "--since", "HEAD"]);
    assert_eq!(
        (exit, out.as_str(), err.as_str()),
        (
            Exit::Success,
            "migrations in pos (repository):\n  \
             pos_x 17.0.1.1.0\n    pre  modified pos_x/migrations/17.0.1.1.0/pre-migrate.py\n  \
             pos_y 17.0.1.0.1\n    post added    pos_y/migrations/17.0.1.0.1/post-y.py\n",
            ""
        )
    );
}

#[test]
fn sniff_compares_packages_and_new_repositories() {
    let project = Project::new("");
    project.odx(&["aggregate"]);
    let lock = std::fs::read_to_string(project.root.join("odx.lock")).unwrap();
    let base = lock
        .lines()
        .find_map(|line| line.strip_prefix("base_sha = \""))
        .unwrap()
        .trim_end_matches('"')
        .to_owned();
    let package = |sha: &str| {
        format!(
            "\n[packages.odoo-addon-pos-y]\nurl = \"{}\"\nsource = {{ pr = 2 }}\nsubdirectory = \"pos_y\"\nsha = \"{sha}\"\n",
            project.url
        )
    };
    // The previous state knew neither the repository nor this commit.
    let empty = "# Generated by odx. Do not edit.\nversion = 1\n";
    write(
        &project.root.join("odx.lock"),
        &format!("{empty}{}", package(&base)),
    );
    git(&project.root, &["add", "odx.lock"]);
    git(&project.root, &["commit", "--quiet", "-m", "old"]);
    write(
        &project.root.join("odx.lock"),
        &format!("{lock}{}", package(&project.pr1)),
    );
    let (exit, out, err) = project.odx(&["sniff", "--since", "HEAD"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert!(out.starts_with(PR1), "{out}");
    assert!(
        out.ends_with(
            "migrations in odoo-addon-pos-y (package):\n  pos_y 17.0.1.0.1 (was 17.0.1.0.0): version changed without migration scripts\n"
        ),
        "{out}"
    );
    // Unchanged since the given revision: nothing to report.
    git(&project.root, &["add", "odx.lock"]);
    git(&project.root, &["commit", "--quiet", "-m", "new"]);
    let (exit, out, _) = project.odx(&["sniff", "--since", "HEAD"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "no migration scripts\n")
    );
}

#[test]
fn sniff_failures() {
    let project = Project::new("[tool.odx.sniff]\nfail_on_new = true\n");
    let (exit, _, err) = project.odx(&["sniff"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock is missing; run `odx lock`"), "{err}");
    let elsewhere = project.tmp.path().join("up").display().to_string();
    let (exit, _, err) = project.odx(&["-C", &elsewhere, "sniff"]);
    assert_eq!(exit, Exit::Usage, "{err}");

    // fail_on_new fails the aggregation, which still writes odx.lock.
    let (exit, out, err) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Failure, "{out}");
    assert!(err.contains("2 migration scripts (fail_on_new)"), "{err}");
    let (exit, _, _) = project.odx(&["sniff"]);
    assert_eq!(exit, Exit::Failure);

    let (exit, _, err) = project.odx(&["sniff", "--since", "nope"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("cannot read odx.lock at nope"), "{err}");
    write(&project.root.join("odx.lock"), "version = 1\n");
    git(&project.root, &["add", "odx.lock"]);
    git(&project.root, &["commit", "--quiet", "-m", "x"]);
    write(&project.root.join("odx.lock"), "nonsense");
    git(&project.root, &["add", "odx.lock"]);
    git(&project.root, &["commit", "--quiet", "-m", "y"]);
    write(&project.root.join("odx.lock"), "version = 1\n");
    let (exit, _, err) = project.odx(&["sniff", "--since", "HEAD"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock at HEAD: "), "{err}");

    // Locked commits that are not in the cache cannot be compared.
    project.configure("", "{ pr = 1 }");
    let (exit, out, _) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Success, "{out}");
    let lock = std::fs::read_to_string(project.root.join("odx.lock")).unwrap();
    let field = |name: &str| {
        lock.lines()
            .find_map(|line| line.strip_prefix(&format!("{name} = \"")))
            .unwrap()
            .trim_end_matches('"')
            .to_owned()
    };
    let (base, tree) = (field("base_sha"), field("result_tree"));
    let package = |sha: &str| {
        format!(
            "\n[packages.odoo-addon-pos-y]\nurl = \"{}\"\nsource = {{ pr = 2 }}\nsha = \"{sha}\"\n",
            project.url
        )
    };
    let missing = "1".repeat(40);
    // Before: a result tree and a package commit the cache never saw.
    write(
        &project.root.join("odx.lock"),
        &format!(
            "{}{}",
            lock.replace(&tree, &"2".repeat(40)),
            package(&project.pr1)
        ),
    );
    git(&project.root, &["add", "odx.lock"]);
    git(&project.root, &["commit", "--quiet", "-m", "z"]);
    write(
        &project.root.join("odx.lock"),
        &format!("{}{}", lock.replace(&base, &missing), package(&missing)),
    );
    let (exit, out, err) = project.odx(&["sniff"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Failure, "no migration scripts\n")
    );
    assert!(err.starts_with("error: pos: cannot compare: "), "{err}");
    let (exit, out, err) = project.odx(&["--json", "sniff"]);
    assert_eq!((exit, err.as_str()), (Exit::Failure, ""));
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["errors"].as_array().unwrap().len(), 1);
    // With --since, both the repository and the package fail to compare.
    let (exit, _, err) = project.odx(&["sniff", "--since", "HEAD"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odoo-addon-pos-y: cannot compare"), "{err}");
    assert!(err.contains("pos: cannot compare"), "{err}");
    drop(project.tmp);
}

#[test]
fn changelog_between_lockfile_states() {
    let project = Project::new("");
    project.odx(&["aggregate"]);
    let (exit, _, err) = project.odx(&["changelog"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("cannot read odx.lock at HEAD"), "{err}");
    project.commit();
    let committed = std::fs::read_to_string(project.root.join("odx.lock")).unwrap();

    project.configure("", "{ pr = 1 }, { pr = 2 }");
    let (exit, out, _) = project.odx(&["aggregate"]);
    assert_eq!(exit, Exit::Success, "{out}");
    let pr2 = git(
        &project.tmp.path().join("up"),
        &["rev-parse", "refs/pull/2/head"],
    );
    let (exit, out, err) = project.odx(&["changelog"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert!(
        out.starts_with(&format!(
            "# Changelog: HEAD → working tree\n\n## pos (repository)\n\n- added #2 at `{}`\n\n## Migrations\n\n```text\nmigrations in pos (repository):\n",
            &pr2[..7]
        )),
        "{out}"
    );
    assert!(out.ends_with("post-y.py\n```\n"), "{out}");
    let (_, out, _) = project.odx(&["--json", "changelog"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["to"], "working tree");
    let item = &value["sections"][0]["items"][0];
    assert_eq!(
        (&item["change"], &item["source"]["pr"]),
        (&"added".into(), &2.into())
    );
    assert_eq!(value["migrations"][0]["name"], "pos");

    // The same pull request at a newer commit, counted in the cache.
    let updated = committed.replace(&project.pr1, &pr2);
    write(&project.root.join("odx.lock"), &updated);
    let (_, out, _) = project.odx(&["changelog"]);
    assert!(
        out.contains(&format!(
            "- updated #1: `{}` → `{}` (1 commit)\n",
            &project.pr1[..7],
            &pr2[..7]
        )),
        "{out}"
    );
    let (_, out, _) = project.odx(&["--json", "changelog"]);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["sections"][0]["items"][0]["commits"], 1);

    // A result the cache does not have: migrations are not compared.
    let tree = committed
        .lines()
        .find_map(|line| line.strip_prefix("result_tree = \""))
        .unwrap()
        .trim_end_matches('"')
        .to_owned();
    write(
        &project.root.join("odx.lock"),
        &committed.replace(&tree, &"d".repeat(40)),
    );
    let (exit, _, err) = project.odx(&["changelog"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        err.starts_with("warning: migrations: pos: cannot compare"),
        "{err}"
    );

    let (exit, out, _) = project.odx(&["changelog", "--to", "HEAD"]);
    assert_eq!(
        (exit, out.as_str()),
        (Exit::Success, "# Changelog: HEAD → HEAD\n\nNo changes.\n")
    );
    let (exit, _, err) = project.odx(&["changelog", "--to", "nope"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("cannot read odx.lock at nope"), "{err}");
    std::fs::remove_file(project.root.join("odx.lock")).unwrap();
    let (exit, _, err) = project.odx(&["changelog"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx.lock is missing; run `odx lock`"), "{err}");
    let elsewhere = project.tmp.path().join("up").display().to_string();
    let (exit, _, err) = project.odx(&["-C", &elsewhere, "changelog"]);
    assert_eq!(exit, Exit::Usage, "{err}");
}
