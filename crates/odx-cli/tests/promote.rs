//! `odx promote`: plans of copying addons with their closure between
//! environments.

#![expect(clippy::unwrap_used, reason = "test helpers should fail loudly")]

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

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
    addon(
        "account_due",
        "19.0.1.1.0",
        &["account"],
        "Due dates on invoices",
    );
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
    commit_all(root, "test");
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
             2 to copy into prod; plan only, nothing written\n",
            &sale[..14],
            &helper[..14],
            &helper[..14],
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
