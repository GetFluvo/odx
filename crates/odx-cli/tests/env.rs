//! `odx env list`: the environment model with its preset applied.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn odx(root: &Path, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let env = MapEnv::default().with("ODX_CACHE_DIR", root.join(".cache").display().to_string());
    let exit = odx_cli::run_with_env(command_line, &OsFs, &env, root, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn project(root: &Path, environments: &str) {
    std::fs::write(
        root.join("pyproject.toml"),
        format!("[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"19.0\"\n{environments}"),
    )
    .unwrap();
}

#[test]
fn presets_and_the_current_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(
        root,
        "[tool.odx.environments]\npreset = \"major-branches\"\nbranch_specific = [\".template-answers.yml\"]\n\
         [tool.odx.environments.env.prod]\nrepo = \"https://example.com/prod.git\"\n",
    );
    git(root, &["init", "-q", "-b", "19-test"]);
    let (exit, out, err) = odx(root, &["env", "list"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        "preset major-branches\n\
         \x20 prod  prod  19.0  19-prod  https://example.com/prod.git\n\
         * test  test  19.0  19-test\n\
         \x20 uat   uat   19.0  19-uat   (disposable)\n\
         promotion: test → uat, test → prod, uat → prod, prod → test\n\
         branch-specific: .template-answers.yml\n"
    );
    let (exit, out, _) = odx(root, &["--json", "env", "list"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["schema"], 1);
    assert_eq!(value["current"], "test");
    assert_eq!(
        value["environments"][0]["repo"],
        "https://example.com/prod.git"
    );
    assert_eq!(value["environments"][2]["disposable"], true);
    assert_eq!(value["promotion"][0], serde_json::json!(["test", "uat"]));
    // A branch of no environment, and explicit environments without edges
    // or roles.
    git(root, &["checkout", "-q", "-b", "feature"]);
    project(
        root,
        "[tool.odx.environments.env.staging]\nbranch = \"{series}-staging\"\n",
    );
    let (exit, out, _) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "preset explicit\n\
         \x20 staging  -  19.0  19.0-staging\n\
         promotion: none\n\
         the current branch feature is no environment's\n"
    );
}

#[test]
fn no_environments_and_invalid_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root, "");
    let (exit, out, err) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(out, "");
    assert_eq!(
        err,
        "error: no environments configured: add [tool.odx.environments] to pyproject.toml\n"
    );
    project(
        root,
        "[tool.odx.environments]\n[tool.odx.environments.env.qa]\nbranch = \"{x}\"\n",
    );
    let (exit, _, err) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Usage);
    assert!(
        err.contains("tool.odx.environments.env.qa.branch: unknown placeholder {x}"),
        "{err}"
    );
    // Not a git checkout: no current branch to mark.
    project(
        root,
        "[tool.odx.environments]\npreset = \"main-production\"\n",
    );
    let (exit, out, _) = odx(root, &["env", "list"]);
    assert_eq!(exit, Exit::Success);
    assert!(
        !out.contains('*') && !out.contains("current branch"),
        "{out}"
    );
}

fn commit_all(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=Odx",
            "-c",
            "user.email=odx@example.com",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn manifest(version: &str, extra: &str) -> String {
    format!("{{\n    'name': 'X',\n    'version': '{version}',{extra}\n}}\n")
}

fn uv_lock(packages: &[(&str, &str, &str)]) -> String {
    let mut text = String::from(
        "version = 1\n\n[[package]]\nname = \"p\"\nversion = \"0.1\"\nsource = { virtual = \".\" }\n",
    );
    for (name, version, source) in packages {
        let _ = write!(
            text,
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {source}\n"
        );
    }
    text
}

fn odx_lock(odoo: char, tree: char) -> String {
    let sha = |c: char| c.to_string().repeat(40);
    format!(
        "version = 1\n\n[odoo]\nurl = \"https://github.com/OCA/OCB.git\"\nrev = \"{}\"\n\n\
         [[repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nbase = \"19.0\"\n\
         base_sha = \"{}\"\nresult_tree = \"{}\"\n",
        sha(odoo),
        sha('1'),
        sha(tree)
    )
}

const ENVIRONMENTS: &str = "[tool.odx.groups]\nproject_addons = [\"modules/custom\"]\n\n\
    [tool.odx.environments]\npreset = \"major-branches\"\nbranch_specific = [\".template-answers.yml\"]\n";

const INDEX: &str = "{ registry = \"https://pypi.org/simple\" }";

/// `19-prod`, and `19-test` branched from it with one change of every kind.
fn two_branches(root: &Path) {
    git(root, &["init", "-q", "-b", "19-prod"]);
    project(root, ENVIRONMENTS);
    write(
        &root.join("modules/custom/a/__manifest__.py"),
        &manifest("19.0.1.0.0", ""),
    );
    write(
        &root.join("modules/custom/b/__manifest__.py"),
        &manifest("19.0.1.0.0", ""),
    );
    write(
        &root.join("modules/custom/gone/__manifest__.py"),
        &manifest("19.0.1.0.0", ""),
    );
    write(
        &root.join("modules/custom/same/__openerp__.py"),
        &manifest("19.0.1.0.0", ""),
    );
    write(&root.join("modules/custom/README.md"), "not an addon\n");
    write(
        &root.join("modules/custom/docs/index.md"),
        "not an addon either\n",
    );
    write(&root.join(".template-answers.yml"), "env: prod\n");
    write(
        &root.join("uv.lock"),
        &uv_lock(&[
            ("odoo-addon-x", "19.0.1.0.0", INDEX),
            ("odoo-addon-y", "19.0.1.0.0", INDEX),
        ]),
    );
    write(&root.join("odx.lock"), &odx_lock('a', '2'));
    commit_all(root, "prod");
    git(root, &["checkout", "-q", "-b", "19-test"]);
    write(
        &root.join("modules/custom/a/__manifest__.py"),
        &manifest("19.0.1.1.0", ""),
    );
    write(
        &root.join("modules/custom/b/__manifest__.py"),
        &manifest("19.0.1.0.0", "\n    'summary': 'edited',"),
    );
    std::fs::remove_dir_all(root.join("modules/custom/gone")).unwrap();
    write(
        &root.join("modules/custom/c/__manifest__.py"),
        &manifest("19.0.1.0.0", ""),
    );
    write(&root.join(".template-answers.yml"), "env: test\n");
    write(
        &root.join("uv.lock"),
        &uv_lock(&[
            ("odoo-addon-x", "19.0.1.1.0", INDEX),
            (
                "odoo-addon-y",
                "19.0.1.0.0",
                "{ git = \"https://github.com/OCA/y?rev=1#0123456789abcdef0123456789abcdef01234567\" }",
            ),
            ("odoo-addon-z", "19.0.1.0.0", INDEX),
        ]),
    );
    write(&root.join("odx.lock"), &odx_lock('b', '3'));
    commit_all(root, "test");
}

#[test]
fn differences_between_two_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    two_branches(root);
    let (exit, out, err) = odx(root, &["env", "diff", "test", "prod"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    let tree = |branch: &str, path: &str| {
        let out = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--short=7", &format!("{branch}:{path}")])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let (a_test, a_prod) = (
        tree("19-test", "modules/custom/a"),
        tree("19-prod", "modules/custom/a"),
    );
    let (b_test, b_prod) = (
        tree("19-test", "modules/custom/b"),
        tree("19-prod", "modules/custom/b"),
    );
    let c = tree("19-test", "modules/custom/c");
    let gone = tree("19-prod", "modules/custom/gone");
    assert_eq!(
        out,
        format!(
            "test (refs/heads/19-test) and prod (refs/heads/19-prod)\n\
             newer in test:\n\
             \x20 a               19.0.1.1.0 tree {a_test} | 19.0.1.0.0 tree {a_prod}\n\
             \x20 odoo-addon-x    19.0.1.1.0 | 19.0.1.0.0\n\
             changed:\n\
             \x20 odoo            rev bbbbbbb | rev aaaaaaa\n\
             \x20 b               19.0.1.0.0 tree {b_test} | 19.0.1.0.0 tree {b_prod}\n\
             \x20 repository pos  tree 3333333 | tree 2222222\n\
             from another source:\n\
             \x20 odoo-addon-y    git https://github.com/OCA/y 19.0.1.0.0 0123456 | index 19.0.1.0.0\n\
             only in test:\n\
             \x20 c               19.0.1.0.0 tree {c} | -\n\
             \x20 odoo-addon-z    19.0.1.0.0 | -\n\
             only in prod:\n\
             \x20 gone            - | 19.0.1.0.0 tree {gone}\n\
             branch-specific files that differ (never promoted):\n\
             \x20 .template-answers.yml\n\
             9 differ, 1 identical\n"
        )
    );
    // A branch names its environment; JSON has every entry.
    let (exit, out, _) = odx(root, &["--json", "env", "diff", "19-test", "prod"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["a"]["env"], "test");
    assert_eq!(value["a"]["branch"], "19-test");
    assert_eq!(value["series_differ"], false);
    assert_eq!(value["entries"].as_array().unwrap().len(), 10);
    assert_eq!(value["entries"][0]["kind"], "odoo");
    assert_eq!(value["entries"][0]["change"], "changed");
    assert_eq!(value["branch_specific"][0], ".template-answers.yml");
    // The remote's branch is preferred.
    git(
        root,
        &["update-ref", "refs/remotes/origin/19-prod", "19-test"],
    );
    let (exit, out, _) = odx(root, &["env", "diff", "test", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "test (refs/heads/19-test) and prod (refs/remotes/origin/19-prod)\nidentical (9 pinned)\n"
    );
}

#[test]
fn an_environment_in_its_own_repository() {
    let tmp = tempfile::tempdir().unwrap();
    let prod = tmp.path().join("prod");
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&prod).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    // Production: flat layout, its addon at the root, no lockfiles.
    git(&prod, &["init", "-q", "-b", "main"]);
    project(&prod, "");
    write(&prod.join("a/__manifest__.py"), &manifest("19.0.1.0.0", ""));
    commit_all(&prod, "prod");
    let environments = format!(
        "[tool.odx.environments]\npreset = \"main-production\"\n\
         [tool.odx.environments.env.prod]\nbranch = \"main\"\nrepo = \"{}\"\n\
         [tool.odx.environments.env.test]\nbranch = \"develop\"\n",
        prod.display().to_string().replace('\\', "/")
    );
    git(&root, &["init", "-q", "-b", "develop"]);
    project(&root, &environments);
    write(&root.join("a/__manifest__.py"), &manifest("19.0.1.0.0", ""));
    commit_all(&root, "test");
    let (exit, out, err) = odx(&root, &["env", "diff", "test", "prod"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        "test (refs/heads/develop) and prod (refs/odx/env/prod)\nidentical (1 pinned)\n"
    );
    // A repository that cannot be fetched.
    std::fs::remove_dir_all(&prod).unwrap();
    let (exit, _, err) = odx(&root, &["env", "diff", "test", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.starts_with("error: cannot fetch main of "), "{err}");
}

/// A project whose test environment runs the next series.
fn mixed_series(root: &Path) {
    project(
        root,
        "[tool.odx.environments]\npreset = \"main-production\"\n\
         [tool.odx.environments.env.test]\nseries = \"20.0\"\n",
    );
}

#[test]
fn vendored_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    mixed_series(root);
    let record = |tree: &str| {
        format!(
            "version = 1\n\n[[addon]]\nname = \"a\"\ngroup = \"oca\"\npackage = \"odoo-addon-a\"\n\
             source = \"19.0.1.0.0\"\nsource_version = \"19.0.1.0.0\"\nwritten_version = \"19.0.1.0.0\"\n\
             source_content = \"x\"\ntree = \"{tree}\"\n"
        )
    };
    write(&root.join("test.lock"), &record("t1"));
    write(&root.join("prod.lock"), &record("t2"));
    let (exit, out, err) = odx(
        root,
        &[
            "env",
            "diff",
            "test",
            "prod",
            "--vendored",
            "test.lock",
            "prod.lock",
        ],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert_eq!(
        out,
        "test (test.lock) and prod (prod.lock)\n\
         series differ: test runs 20.0, prod runs 19.0\n\
         changed:\n\
         \x20 a  19.0.1.0.0 tree t1 | 19.0.1.0.0 tree t2\n\
         1 differ, 0 identical\n"
    );
    write(&root.join("broken.lock"), "version = 9\n");
    for (file, message) in [
        ("missing.lock", "missing.lock: "),
        ("broken.lock", "broken.lock: vendor manifest version 9"),
    ] {
        let (exit, _, err) = odx(
            root,
            &[
                "env",
                "diff",
                "test",
                "prod",
                "--vendored",
                file,
                "prod.lock",
            ],
        );
        assert_eq!(exit, Exit::Failure);
        assert!(err.contains(message), "{err}");
    }
}

#[test]
fn installed_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    mixed_series(root);
    // Installed wheels: one from git, one from an index, one changed file.
    let dist = |site: &str, name: &str, record: &str, commit: Option<&str>| {
        let info = root.join(site).join(format!("{name}-1.0.dist-info"));
        write(
            &info.join("METADATA"),
            &format!("Name: {name}\nVersion: 1.0\n"),
        );
        write(&info.join("RECORD"), record);
        if let Some(commit) = commit {
            write(
                &info.join("direct_url.json"),
                &format!("{{\"vcs_info\": {{\"commit_id\": \"{commit}\"}}}}"),
            );
        }
    };
    dist("a", "odoo_addon_g", "", Some("c1"));
    dist("b", "odoo_addon_g", "", Some("c1"));
    dist(
        "a",
        "odoo_addon_w",
        "w/x.py,sha256=AAA,1\nw/RECORD,,\n",
        None,
    );
    dist(
        "b",
        "odoo_addon_w",
        "w/x.py,sha256=BBB,1\nw/RECORD,,\n",
        None,
    );
    let (exit, out, err) = odx(
        root,
        &["env", "diff", "test", "prod", "--installed", "a", "b"],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""));
    assert!(
        out.contains("changed:\n  odoo-addon-w  1.0 sha256:"),
        "{out}"
    );
    assert!(out.ends_with("1 differ, 1 identical\n"), "{out}");
    let (exit, _, err) = odx(
        root,
        &["env", "diff", "test", "prod", "--installed", "a", "nowhere"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("nowhere: no site-packages"), "{err}");
}

#[test]
fn what_cannot_be_compared() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    two_branches(root);
    for args in [["env", "diff", "qa", "prod"], ["env", "diff", "prod", "qa"]] {
        let (exit, _, err) = odx(root, &args);
        assert_eq!(exit, Exit::Usage, "{args:?}");
        assert!(
            err.contains("qa is no environment, nor a branch of one"),
            "{err}"
        );
    }
    // The uat branch exists nowhere.
    let (exit, _, err) = odx(root, &["env", "diff", "uat", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: uat: neither origin/19-uat nor 19-uat is here; fetch it first\n"
    );
    // Broken files on a branch.
    let broken = |path: &str, text: &str, message: &str| {
        git(root, &["checkout", "-q", "-B", "19-uat", "19-prod"]);
        write(&root.join(path), text);
        commit_all(root, "broken");
        git(root, &["checkout", "-q", "19-test"]);
        let (exit, _, err) = odx(root, &["env", "diff", "uat", "prod"]);
        assert_eq!(exit, Exit::Failure, "{path}");
        assert!(err.starts_with(message), "{err}");
    };
    broken("uv.lock", "package = 3\n", "error: uv.lock of uat: ");
    broken("odx.lock", "version = 99\n", "error: odx.lock of uat: ");
    broken(
        "pyproject.toml",
        "[project]\nname = \"p\"\n",
        "error: pyproject.toml of uat: no [tool.odx]",
    );
    broken(
        "pyproject.toml",
        "[tool.odx]\nodoo_version = 3\n",
        "error: pyproject.toml of uat: ",
    );
    git(root, &["checkout", "-q", "-B", "19-uat", "19-prod"]);
    git(root, &["rm", "-q", "pyproject.toml"]);
    commit_all(root, "no pyproject");
    git(root, &["checkout", "-q", "19-test"]);
    let (exit, _, err) = odx(root, &["env", "diff", "uat", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: pyproject.toml of uat: missing on refs/heads/19-uat\n"
    );
    // No environments, and no project.
    project(root, "");
    let (exit, _, err) = odx(root, &["env", "diff", "test", "prod"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no environments configured"), "{err}");
    let elsewhere = tempfile::tempdir().unwrap();
    let (exit, _, _) = odx(elsewhere.path(), &["env", "diff", "test", "prod"]);
    assert_ne!(exit, Exit::Success);
    // A pattern names no branch.
    project(root, "[tool.odx.environments]\npreset = \"odoo-sh\"\n");
    let (exit, _, err) = odx(root, &["env", "diff", "uat", "prod"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: the branch of uat is a pattern (staging/*): name one of its branches instead\n"
    );
}
