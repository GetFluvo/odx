//! `odx promote`: plans of copying addons with their closure between
//! environments.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

#[cfg(unix)]
use std::collections::HashMap;
use std::fmt::Write as _;
#[cfg(unix)]
use std::io::{Read, Write as _};
#[cfg(unix)]
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
#[cfg(unix)]
use std::sync::{Arc, Mutex};

use odx_core::Exit;
use odx_core::fake::MapEnv;
use odx_core::system::OsFs;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=Odx", "-c", "user.email=odx@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn odx(root: &Path, args: &[&str]) -> (Exit, String, String) {
    odx_with(root, "true", args)
}

/// `odx` with `uv` replaced by `uv`.
fn odx_with(root: &Path, uv: &str, args: &[&str]) -> (Exit, String, String) {
    let mut command_line = vec!["odx"];
    command_line.extend_from_slice(args);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let env = MapEnv::default()
        .with("ODX_CACHE_DIR", root.join(".cache").display().to_string())
        .with("ODX_UV", uv);
    let exit = odx_cli::run_with_env(command_line, &OsFs, &env, root, &mut out, &mut err);
    (
        exit,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn manifest(version: &str, depends: &[&str], summary: &str) -> String {
    let depends: Vec<String> = depends.iter().map(|dep| format!("'{dep}'")).collect();
    format!(
        "{{\n    'name': 'X',\n    'summary': '{summary}',\n    'version': '{version}',\n    'depends': [{}],\n}}\n",
        depends.join(", ")
    )
}

fn uv_lock(packages: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "version = 1\n\n[[package]]\nname = \"p\"\nversion = \"0.1\"\nsource = { virtual = \".\" }\n",
    );
    for (name, version) in packages {
        let _ = write!(
            text,
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n"
        );
    }
    text
}

fn odx_lock(odoo: char) -> String {
    format!(
        "version = 1\n\n[odoo]\nurl = \"https://github.com/OCA/OCB.git\"\nrev = \"{}\"\n",
        odoo.to_string().repeat(40)
    )
}

fn pyproject(root: &Path, extra: &str) {
    write(
        &root.join("pyproject.toml"),
        &format!(
            "[project]\nname = \"p\"\n\n[tool.odx]\nodoo_version = \"19.0\"\n\n\
             [tool.odx.groups]\nproject_addons = [\"modules\"]\n\n\
             [tool.odx.features.invoicing]\nmodules = [\"account_due\"]\ndescription = \"Invoices sent on time\"\n\n\
             [tool.odx.environments]\npreset = \"major-branches\"\n{extra}"
        ),
    );
}

fn commit_all(root: &Path, message: &str) {
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", message]);
}

/// `19-prod`, and `19-test` with newer and older things.
fn project(root: &Path) {
    git(root, &["init", "-q", "-b", "19-prod"]);
    git(root, &["config", "user.name", "Odx"]);
    git(root, &["config", "user.email", "odx@example.com"]);
    pyproject(root, "");
    let addon = |name: &str, version: &str, depends: &[&str], summary: &str| {
        write(
            &root.join("modules").join(name).join("__manifest__.py"),
            &manifest(version, depends, summary),
        );
    };
    addon("helper", "19.0.1.0.0", &["base"], "Helps");
    addon("account_due", "19.0.1.0.0", &["account"], "Due dates");
    addon("old", "19.0.2.0.0", &[], "Old");
    addon("tweak", "19.0.1.0.0", &[], "Tweak");
    write(
        &root.join("uv.lock"),
        &uv_lock(&[
            ("odoo-addon-x", "19.0.1.0.0"),
            ("odoo-addon-y", "19.0.2.0.0"),
        ]),
    );
    write(&root.join("odx.lock"), &odx_lock('a'));
    commit_all(root, "prod");
    git(root, &["checkout", "-q", "-b", "19-test"]);
    addon(
        "sale_x",
        "19.0.1.0.0",
        &["helper", "x", "sale"],
        "Sales extras",
    );
    commit_all(root, "add sale_x");
    addon(
        "account_due",
        "19.0.1.1.0",
        &["account"],
        "Due dates on invoices",
    );
    commit_all(root, "account_due: invoices");
    addon("old", "19.0.1.0.0", &[], "Old");
    addon("tweak", "19.0.1.0.0", &[], "Tweaked");
    write(
        &root.join("uv.lock"),
        &uv_lock(&[
            ("odoo-addon-x", "19.0.1.1.0"),
            ("odoo-addon-y", "19.0.1.0.0"),
        ]),
    );
    write(&root.join("odx.lock"), &odx_lock('b'));
    commit_all(root, "test pins");
}

#[test]
fn plans_with_their_closure() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let (exit, out, err) = odx(root, &["promote", "sale_x", "--to", "prod"]);
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    let tree = |path: &str| git(root, &["rev-parse", &format!("19-test:modules/{path}")]);
    let (sale, helper) = (tree("sale_x"), tree("helper"));
    assert_eq!(
        out,
        format!(
            "promote test (refs/heads/19-test) into prod (refs/heads/19-prod), series 19.0\n\
             selection: sale_x\n\
             \x20 sale_x  new           19.0.1.0.0 {} | -\n\
             \x20 helper  identical     19.0.1.0.0 {} | 19.0.1.0.0 {}\n\
             \x20 x       newer         19.0.1.1.0 | 19.0.1.0.0\n\
             not copied (Odoo's own addons, or of aggregated repositories): base, sale\n\
             core drift: odoo bbbbbbbbbbbbbb | aaaaaaaaaaaaaa (copied with the pins; --no-with-odoo leaves it)\n\
             code on test not on prod (--with-code cherry-picks it):\n\
             \x20 {} add sale_x\n\
             2 to copy into prod; plan only (--commit or --open-pr with --yes writes it)\n",
            &sale[..14],
            &helper[..14],
            &helper[..14],
            git(
                root,
                &["log", "-1", "--format=%h", "--abbrev=7", "19-test~2"]
            ),
        )
    );
    let (exit, out, _) = odx(
        root,
        &[
            "--json",
            "promote",
            "sale_x",
            "--to",
            "prod",
            "--no-with-odoo",
        ],
    );
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["from"]["env"], "test");
    assert_eq!(value["items"][2]["addon"], "x");
    assert_eq!(value["items"][2]["from"]["kind"], "package");
    assert_eq!(value["items"][0]["to"], serde_json::Value::Null);
    assert_eq!(value["core"], serde_json::Value::Null);
    assert_eq!(value["refused"], false);
    // The same version with other content, as JSON with the core.
    let (exit, out, _) = odx(root, &["promote", "tweak", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("  tweak  changed "), "{out}");
    assert!(
        out.contains("warning: tweak differs at the same version 19.0.1.0.0: bump it"),
        "{out}"
    );
    let (exit, out, _) = odx(root, &["--json", "promote", "tweak", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["core"]["from"]["pin"], "b".repeat(40));
    assert_eq!(value["core"]["to"]["pin"], "a".repeat(40));
    // What prod has already is no promotion.
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "helper",
            "--from",
            "test",
            "--to",
            "prod",
            "--no-with-odoo",
        ],
    );
    assert_eq!(exit, Exit::Success);
    assert!(
        out.ends_with("nothing to promote: prod has all of it\n"),
        "{out}"
    );
}

#[test]
fn conflicts_refuse_unless_allowed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let (exit, out, _) = odx(root, &["promote", "y", "old", "--to", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.contains(
            "  y    older         19.0.1.0.0 | 19.0.2.0.0  refused: downgrade (--allow-downgrade)\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("refused: version conflict (sync prod into the source first)\n"),
        "{out}"
    );
    assert!(
        out.ends_with("refused: resolve the conflicts above; nothing written\n"),
        "{out}"
    );
    let (exit, out, _) = odx(root, &["promote", "y", "--to", "prod", "--allow-downgrade"]);
    assert_eq!(exit, Exit::Success, "{out}");
    assert!(out.contains("  allowed: downgrade\n"), "{out}");
    // A package that became a project addon is from another source.
    git(root, &["checkout", "-q", "19-test"]);
    write(
        &root.join("modules/x/__manifest__.py"),
        &manifest("19.0.1.1.0", &[], "X"),
    );
    commit_all(root, "x moved into the project");
    let (exit, out, _) = odx(root, &["promote", "x", "--to", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert!(
        out.contains("refused: source change (--allow-source-change)"),
        "{out}"
    );
    let (exit, _, _) = odx(
        root,
        &["promote", "x", "--to", "prod", "--allow-source-change"],
    );
    assert_eq!(exit, Exit::Success);
}

#[test]
fn features_and_searches() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let (exit, out, _) = odx(root, &["promote", "--feature", "invoicing", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("selection: account_due\n"), "{out}");
    let (exit, _, err) = odx(root, &["promote", "--feature", "nope", "--to", "prod"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: no feature \"nope\" in [tool.odx.features] on test\n"
    );
    let (exit, out, _) = odx(root, &["promote", "--search", "invoicing", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(
        out,
        "candidates on test for \"invoicing\":\n\
         \x20 feature  1015  invoicing (account_due)\n\
         \x20 addon       6  account_due\n\
         plan one with --select <name>[,<name>...]\n"
    );
    let (exit, out, _) = odx(
        root,
        &["--json", "promote", "--search", "invoicing", "--to", "prod"],
    );
    assert_eq!(exit, Exit::Success);
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["candidates"][0]["kind"], "feature");
    assert_eq!(value["candidates"][0]["modules"][0], "account_due");
    assert_eq!(value["candidates"][1]["kind"], "addon");
    let (exit, out, _) = odx(root, &["promote", "--search", "warehouse", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert_eq!(out, "nothing on test matches \"warehouse\"\n");
    // The source's own synonyms.
    pyproject(
        root,
        "[tool.odx.promote]\nsearch_synonyms = { warehouse = [\"due\"] }\n",
    );
    commit_all(root, "synonyms");
    let (exit, out, _) = odx(root, &["promote", "--search", "warehouse", "--to", "prod"]);
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("  addon"), "{out}");
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "--search",
            "invoicing",
            "--select",
            "account_due",
            "--to",
            "prod",
        ],
    );
    assert_eq!(exit, Exit::Success);
    assert!(out.contains("  account_due  newer "), "{out}");
}

#[test]
fn what_cannot_be_promoted() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let elsewhere = tempfile::tempdir().unwrap();
    let (exit, _, _) = odx(elsewhere.path(), &["promote", "a", "--to", "prod"]);
    assert_ne!(exit, Exit::Success);
    write(
        &elsewhere.path().join("pyproject.toml"),
        "[tool.odx]\nodoo_version = \"19.0\"\n",
    );
    let (exit, _, err) = odx(elsewhere.path(), &["promote", "a", "--to", "prod"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("no environments configured"), "{err}");
    let (exit, _, err) = odx(root, &["promote", "nope", "--to", "prod"]);
    assert_eq!(
        (exit, err.as_str()),
        (Exit::Usage, "error: not on test: nope\n")
    );
    let (exit, _, err) = odx(root, &["promote", "old", "--from", "prod", "--to", "uat"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: promoting prod into uat is not allowed (`promotion` in [tool.odx.environments])\n"
    );
    let (exit, _, err) = odx(root, &["promote", "old", "--to", "nowhere"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("nowhere is no environment"), "{err}");
    // Nothing promotes into test from a lasting environment but prod.
    let (exit, _, err) = odx(root, &["promote", "old", "--from", "qa", "--to", "test"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("qa is no environment"), "{err}");
    // Two lasting sources: name one.
    pyproject(
        root,
        "[tool.odx.environments.env.uat]\ndisposable = false\n",
    );
    let (exit, _, err) = odx(root, &["promote", "old", "--to", "prod"]);
    assert_eq!(exit, Exit::Usage);
    assert!(err.contains("name one with --from"), "{err}");
    // Across series.
    pyproject(
        root,
        "[tool.odx.environments.env.test]\nseries = \"20.0\"\nbranch = \"19-test\"\n",
    );
    let (exit, _, err) = odx(root, &["promote", "old", "--to", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: test runs 20.0 and prod runs 19.0: a promotion across series is refused; `odx upgrade` prepares one\n"
    );
    pyproject(root, "");
    let (exit, _, err) = odx(root, &["promote", "old", "--from", "uat", "--to", "prod"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("neither origin/19-uat nor 19-uat"), "{err}");
    let (exit, _, err) = odx(root, &["promote", "old", "--to", "uat"]);
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("neither origin/19-uat nor 19-uat"), "{err}");
}

/// `path` on `branch`.
#[cfg(unix)]
fn show(root: &Path, branch: &str, path: &str) -> String {
    git(root, &["show", &format!("{branch}:{path}")])
}

#[test]
#[cfg(unix)]
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
fn commits_the_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    let (exit, _, err) = odx(root, &["promote", "sale_x", "--to", "prod", "--commit"]);
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: --commit and --open-pr write a branch: add --yes to confirm\n"
    );
    let (exit, out, err) = odx(
        root,
        &["promote", "sale_x", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!((exit, err.as_str()), (Exit::Success, ""), "{out}");
    let head = git(root, &["rev-parse", "--short=7", "odx/promote/prod-sale_x"]);
    assert!(
        out.ends_with(&format!("committed {head} on odx/promote/prod-sale_x\n")),
        "{out}"
    );
    let branch = "odx/promote/prod-sale_x";
    assert!(
        show(root, branch, "uv.lock").contains("name = \"odoo-addon-x\"\nversion = \"19.0.1.1.0\"")
    );
    assert!(
        show(root, branch, "uv.lock").contains("name = \"odoo-addon-y\"\nversion = \"19.0.2.0.0\"")
    );
    assert!(show(root, branch, "odx.lock").contains(&"b".repeat(40)));
    assert_eq!(
        git(root, &["rev-parse", &format!("{branch}~1")]),
        git(root, &["rev-parse", "19-prod"])
    );
    let message = git(root, &["log", "-1", "--format=%B", branch]);
    assert!(
        message.starts_with("promote sale_x from test to prod\n\nPromoted with `odx promote`"),
        "{message}"
    );
    assert!(
        message.contains("| x | newer | `19.0.1.1.0` | `19.0.1.0.0` |"),
        "{message}"
    );
    assert!(
        message.contains("Odoo core: `bbbbbbbbbbbb` on test, `aaaaaaaaaaaa` on prod (copied)."),
        "{message}"
    );
    assert!(
        message
            .contains("Not copied (Odoo's own addons, or of aggregated repositories): base, sale."),
        "{message}"
    );
    // No worktree is left behind.
    assert_eq!(git(root, &["worktree", "list"]).lines().count(), 1);
    // With the code, written again on the same branch.
    let (exit, out, _) = odx(
        root,
        &[
            "--json",
            "promote",
            "sale_x",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
        ],
    );
    assert_eq!(exit, Exit::Success, "{out}");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["applied"]["picked"], 1);
    assert_eq!(value["applied"]["branch"], branch);
    assert_eq!(value["applied"]["pull_request"], serde_json::Value::Null);
    assert_eq!(value["code"][0]["subject"], "add sale_x");
    assert!(show(root, branch, "modules/sale_x/__manifest__.py").contains("Sales extras"));
    let (_, out, _) = odx(
        root,
        &[
            "promote",
            "sale_x",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
        ],
    );
    assert!(out.contains("(1 commit(s) cherry-picked)\n"), "{out}");
    // Only code to promote, and only the code.
    let (exit, _, err) = odx(
        root,
        &[
            "promote",
            "account_due",
            "--to",
            "prod",
            "--no-with-odoo",
            "--commit",
            "--yes",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: nothing to write: prod has the pins already (--with-code promotes the code)\n"
    );
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "account_due",
            "--to",
            "prod",
            "--no-with-odoo",
            "--commit",
            "--yes",
            "--with-code",
        ],
    );
    assert_eq!(exit, Exit::Success, "{out}");
    assert_eq!(
        git(
            root,
            &["log", "-1", "--format=%s", "odx/promote/prod-account_due"]
        ),
        "account_due: invoices"
    );
    // Warnings go into the message.
    let (exit, _, _) = odx(
        root,
        &["promote", "tweak", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!(exit, Exit::Success);
    let message = git(
        root,
        &["log", "-1", "--format=%B", "odx/promote/prod-tweak"],
    );
    assert!(
        message.contains("Warning: tweak differs at the same version"),
        "{message}"
    );
    let (_, out, _) = odx(
        root,
        &[
            "--json",
            "promote",
            "helper",
            "--to",
            "prod",
            "--no-with-odoo",
        ],
    );
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["noop"], true);
    // Nothing to promote writes nothing.
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "helper",
            "--to",
            "prod",
            "--no-with-odoo",
            "--commit",
            "--yes",
        ],
    );
    assert_eq!(exit, Exit::Success);
    assert!(
        out.ends_with("nothing to promote: prod has all of it\n"),
        "{out}"
    );
}

#[test]
#[cfg(unix)]
#[expect(clippy::too_many_lines, reason = "one scenario, step by step")]
fn what_stops_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    project(root);
    // The branch is checked out elsewhere.
    let other = tmp.path().join("other");
    let other_text = other.display().to_string();
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "odx/promote/prod-sale_x",
            &other_text,
            "19-prod",
        ],
    );
    let (exit, _, err) = odx(
        root,
        &["promote", "sale_x", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.starts_with("error: cannot prepare odx/promote/prod-sale_x: "),
        "{err}"
    );
    git(root, &["worktree", "remove", "--force", &other_text]);
    // uv fails, or re-resolves something else.
    let (exit, _, err) = odx_with(
        root,
        "odx-surely-missing-uv",
        &["promote", "sale_x", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(err.contains("odx-surely-missing-uv lock"), "{err}");
    let uv = tmp.path().join("uv");
    write(
        &uv,
        "#!/bin/sh\nsed 's/version = \"19.0.2.0.0\"/version = \"19.0.2.0.1\"/' uv.lock > uv.lock.new && mv uv.lock.new uv.lock\n",
    );
    std::fs::set_permissions(&uv, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let uv = uv.display().to_string();
    let (exit, _, err) = odx_with(
        root,
        &uv,
        &["promote", "sale_x", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: transitive drift: `uv lock` re-resolved odoo-addon-y (--allow-transitive keeps it)\n"
    );
    let (exit, _, _) = odx_with(
        root,
        &uv,
        &[
            "promote",
            "sale_x",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--allow-transitive",
        ],
    );
    assert_eq!(exit, Exit::Success);
    // A group the target lacks.
    pyproject(root, "");
    let text = std::fs::read_to_string(root.join("pyproject.toml")).unwrap();
    write(
        &root.join("pyproject.toml"),
        &text.replace(
            "[tool.odx]",
            "[project.optional-dependencies]\noca = [\"odoo-addon-x==19.0.1.1.0\"]\n\n[tool.odx]",
        ),
    );
    commit_all(root, "x declared");
    let (exit, _, err) = odx(
        root,
        &["promote", "sale_x", "--to", "prod", "--commit", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        "error: the target has no dependency group oca for odoo-addon-x==19.0.1.1.0\n"
    );
    // A commit that does not apply.
    git(root, &["checkout", "-q", "19-prod"]);
    write(
        &root.join("modules/account_due/__manifest__.py"),
        &manifest("19.0.1.0.1", &["account"], "Due dates, fixed"),
    );
    commit_all(root, "account_due: fix");
    git(root, &["checkout", "-q", "19-test"]);
    let (exit, _, err) = odx(
        root,
        &[
            "promote",
            "account_due",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
        ],
    );
    assert_eq!(exit, Exit::Failure);
    assert!(
        err.contains("account_due: invoices does not apply on prod: "),
        "{err}"
    );
    assert_eq!(git(root, &["worktree", "list"]).lines().count(), 1);
}

#[test]
#[cfg(unix)]
fn branch_specific_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "19-prod"]);
    git(root, &["config", "user.name", "Odx"]);
    git(root, &["config", "user.email", "odx@example.com"]);
    pyproject(root, "branch_specific = [\".answers.yml\", \"local/*\"]\n");
    write(
        &root.join("modules/a/__manifest__.py"),
        &manifest("19.0.1.0.0", &[], "A"),
    );
    write(&root.join(".answers.yml"), "env: prod\n");
    write(&root.join("local/prod.txt"), "prod only\n");
    commit_all(root, "prod");
    git(root, &["checkout", "-q", "-b", "19-test"]);
    write(
        &root.join("modules/a/__manifest__.py"),
        &manifest("19.0.1.1.0", &[], "A"),
    );
    write(&root.join(".answers.yml"), "env: test\n");
    write(&root.join("local/test.txt"), "test only\n");
    git(root, &["rm", "-q", "local/prod.txt"]);
    commit_all(root, "a: more");
    let (exit, _, err) = odx(
        root,
        &[
            "promote",
            "a",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
            "--include-branch-specific",
            "README.md",
        ],
    );
    assert_eq!(exit, Exit::Usage);
    assert_eq!(
        err,
        "error: README.md is not branch-specific (`branch_specific` in [tool.odx.environments]): nothing to include\n"
    );
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "a",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
        ],
    );
    assert_eq!(exit, Exit::Success, "{out}");
    let branch = "odx/promote/prod-a";
    assert_eq!(show(root, branch, ".answers.yml"), "env: prod");
    assert_eq!(show(root, branch, "local/prod.txt"), "prod only");
    assert!(
        git(root, &["ls-tree", "-r", "--name-only", branch])
            .lines()
            .all(|path| path != "local/test.txt")
    );
    assert!(show(root, branch, "modules/a/__manifest__.py").contains("19.0.1.1.0"));
    // Named, a branch-specific file goes along, and one the source lacks
    // goes away.
    let (exit, out, _) = odx(
        root,
        &[
            "promote",
            "a",
            "--to",
            "prod",
            "--commit",
            "--yes",
            "--with-code",
            "--include-branch-specific",
            ".answers.yml",
            "--include-branch-specific",
            "local/prod.txt",
        ],
    );
    assert_eq!(exit, Exit::Success, "{out}");
    assert!(
        out.contains("!!! branch-specific content included: .answers.yml, local/prod.txt !!!\n"),
        "{out}"
    );
    assert_eq!(show(root, branch, ".answers.yml"), "env: test");
    let tracked = git(root, &["ls-tree", "-r", "--name-only", branch]);
    assert!(
        !tracked.contains("local/prod.txt") && !tracked.contains("local/test.txt"),
        "{tracked}"
    );
    let message = git(root, &["log", "-1", "--format=%B", branch]);
    assert!(
        message.contains("**Branch-specific content included**: .answers.yml, local/prod.txt"),
        "{message}"
    );
}

#[cfg(unix)]
type Routes = Arc<Mutex<HashMap<String, (u16, String)>>>;

/// A server on localhost answering from `routes` by method and path, 404
/// otherwise; the requests it got, as `METHOD path body`.
#[cfg(unix)]
fn serve(routes: &Routes) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = Arc::clone(routes);
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend(buffer.iter().take(n)),
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let length: usize = text
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            let head_end = text.find("\r\n\r\n").map_or(text.len(), |at| at + 4);
            let mut body = request.get(head_end..).unwrap_or_default().to_vec();
            while body.len() < length {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body.extend(buffer.iter().take(n)),
                }
            }
            let mut words = text.split_whitespace();
            let method = words.next().unwrap_or("GET").to_owned();
            let target = words.next().unwrap_or("/").to_owned();
            let path = target.split('?').next().unwrap_or(&target).to_owned();
            log.lock().unwrap().push(format!(
                "{method} {path} {}",
                String::from_utf8_lossy(&body)
            ));
            let (status, answer) = routes
                .lock()
                .unwrap()
                .get(&format!("{method} {path}"))
                .cloned()
                .unwrap_or((404, String::new()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (base, seen)
}

#[cfg(unix)]
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

#[test]
#[cfg(unix)]
fn opens_the_pull_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("project");
    std::fs::create_dir_all(&root).unwrap();
    project(&root);
    // No forge behind the remote.
    let (exit, _, err) = odx(
        &root,
        &["promote", "sale_x", "--to", "prod", "--open-pr", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert_eq!(err, "error: origin is not on a forge\n");
    // A forge on localhost, its repository a local bare one.
    let bare = tmp.path().join("bare.git");
    git(
        tmp.path(),
        &["init", "-q", "--bare", &bare.display().to_string()],
    );
    let routes: Routes = Arc::default();
    let (api, seen) = serve(&routes);
    let route = |key: &str, status: u16, body: &str| {
        routes
            .lock()
            .unwrap()
            .insert(key.to_owned(), (status, body.to_owned()));
    };
    route("GET /api/v3/meta", 200, "{}");
    route(
        "GET /api/v3/repos/acme/project",
        200,
        r#"{"permissions":{"push":true}}"#,
    );
    route(
        "POST /api/v3/repos/acme/project/pulls",
        201,
        &format!(r#"{{"number":3,"html_url":"{api}/acme/project/pull/3"}}"#),
    );
    let remote = format!("{api}/acme/project.git");
    git(&root, &["remote", "add", "origin", &remote]);
    let out = Command::new(env!("CARGO_BIN_EXE_odx"))
        .current_dir(&root)
        .args(["promote", "sale_x", "--to", "prod", "--open-pr", "--yes"])
        .env("ODX_UV", "true")
        .env("ODX_CACHE_DIR", tmp.path().join("cache"))
        .env("GIT_CONFIG_COUNT", "1")
        .env(
            "GIT_CONFIG_KEY_0",
            format!("url.{}.insteadOf", file_url(&bare)),
        )
        .env("GIT_CONFIG_VALUE_0", &remote)
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert!(out.status.success(), "{stderr}");
    assert!(
        stdout.ends_with(&format!(
            "pushed to origin\nopened {api}/acme/project/pull/3\n"
        )),
        "{stdout}"
    );
    let pushed = git(
        &bare,
        &["log", "-1", "--format=%s", "odx/promote/prod-sale_x"],
    );
    assert_eq!(pushed, "promote sale_x from test to prod");
    let requests = seen.lock().unwrap().join("\n");
    assert!(requests.contains("\"base\":\"19-prod\""), "{requests}");
    assert!(
        requests.contains("\"head\":\"odx/promote/prod-sale_x\""),
        "{requests}"
    );
    // An environment in a repository that is not on a forge.
    let prod = tmp.path().join("prod");
    git(
        tmp.path(),
        &[
            "clone",
            "-q",
            "-b",
            "19-prod",
            &root.display().to_string(),
            &prod.display().to_string(),
        ],
    );
    pyproject(
        &root,
        &format!(
            "[tool.odx.environments.env.prod]\nrepo = \"{}\"\n",
            prod.display()
        ),
    );
    commit_all(&root, "prod elsewhere");
    let (exit, _, err) = odx(
        &root,
        &["promote", "sale_x", "--to", "prod", "--open-pr", "--yes"],
    );
    assert_eq!(exit, Exit::Failure);
    assert_eq!(
        err,
        format!("error: {} is not on a forge\n", prod.display())
    );
}
