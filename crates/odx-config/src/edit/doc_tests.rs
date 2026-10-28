use super::*;
use crate::edit::parse_spec;

const PROJECT: &str = r#"[project]
name = "addons"

[project.optional-dependencies]
oca = ["odoo-addon-web_responsive==17.0.1.2.0"]
oca_unmerged = [
  "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
]

[tool.uv.sources]
odoo-addon-reports = { git = "https://forge.example/reports.git", rev = "abc" }

# Aggregation for this project.
[tool.odx]
odoo_version = "17.0"

[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"
merges = [
    { pr = 123 },  # loyalty fix
    { sha = "A1B2C3D4" },
    { branch = "17.0-fix", remote = "https://github.com/acme/pos.git" },
]

[[tool.odx.repositories]]
url = "https://github.com/OCA/web.git"
"#;

fn edit(text: &str) -> PyprojectEdit {
    PyprojectEdit::parse(text).unwrap()
}

fn spec(text: &str) -> Spec {
    parse_spec(text).unwrap()
}

#[test]
fn add_merge_keeps_layout_and_comments() {
    let mut doc = edit(PROJECT);
    assert_eq!(doc.add_merge(&spec("OCA/pos#200"), None), Ok(Added::Merge));
    let expected = PROJECT.replace(
        "remote = \"https://github.com/acme/pos.git\" },\n]",
        "remote = \"https://github.com/acme/pos.git\" },\n    { pr = 200 },\n]",
    );
    assert_eq!(doc.to_string(), expected);
}

#[test]
fn add_merge_variants() {
    let mut doc = edit(PROJECT);
    assert_eq!(
        doc.add_merge(&spec("git@github.com:oca/pos#123"), None),
        Ok(Added::AlreadyPresent)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/pos@a1b2c3d4"), None),
        Ok(Added::AlreadyPresent)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/pos@17.0-fix"), None),
        Ok(Added::Merge)
    );
    assert_eq!(doc.add_merge(&spec("OCA/web#9"), None), Ok(Added::Merge));
    assert!(
        doc.to_string()
            .contains("url = \"https://github.com/OCA/web.git\"\nmerges = [{ pr = 9 }]\n")
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/server-ux#1"), None),
        Ok(Added::Repository)
    );
    let text = doc.to_string();
    assert!(text.ends_with("[[tool.odx.repositories]]\nname = \"server-ux\"\nurl = \"https://github.com/OCA/server-ux.git\"\nmerges = [\n    { pr = 1 },\n]\n"), "{text}");
    assert!(
        crate::parse_pyproject(&text, std::path::Path::new("p.toml"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn add_creates_tool_odx_only_with_a_series() {
    let mut doc = edit("[project]\nname = \"x\"\n");
    assert_eq!(
        doc.add_merge(&spec("OCA/pos#1"), None),
        Err(EditError::NoOdxTable)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/pos#1"), Some("18.0")),
        Ok(Added::Repository)
    );
    let text = doc.to_string();
    assert!(text.starts_with("[project]\nname = \"x\"\n\n[tool.odx]\nodoo_version = \"18.0\"\n\n[[tool.odx.repositories]]\n"), "{text}");
    let mut with_tool = edit("[tool.ruff]\nline-length = 100\n");
    assert_eq!(
        with_tool.add_merge(&spec("OCA/pos#1"), Some("18.0")),
        Ok(Added::Repository)
    );
}

#[test]
fn unexpected_types_are_reported() {
    for (text, what) in [
        ("tool = 1\n", "[tool]"),
        ("[tool]\nodx = 1\n", "[tool.odx]"),
        (
            "[tool.odx]\nodoo_version = \"17.0\"\nrepositories = 1\n",
            "[tool.odx].repositories",
        ),
        (
            "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nurl = \"https://github.com/OCA/pos.git\"\nmerges = 1\n",
            "merges",
        ),
    ] {
        let mut doc = edit(text);
        assert_eq!(
            doc.add_merge(&spec("OCA/pos#1"), Some("17.0")),
            Err(EditError::Malformed(what)),
            "{text}"
        );
    }
    let mut doc = edit("[tool.odx]\nodoo_version = \"17.0\"\npackages = 1\n");
    assert_eq!(
        doc.add_package("x", &spec("OCA/pos#1"), None, None, None),
        Err(EditError::Malformed("[tool.odx.packages]"))
    );
    assert_ne!(PyprojectEdit::parse("[x").unwrap_err().to_string(), "");
}

#[test]
fn drop_merge() {
    let mut doc = edit(PROJECT);
    doc.drop_merge(&spec("OCA/pos#123")).unwrap();
    assert!(!doc.to_string().contains("{ pr = 123 }"));
    assert!(doc.to_string().contains("    { sha = \"A1B2C3D4\" },\n"));
    let missing = [
        (
            "OCA/pos#123",
            "no merge #123 on https://github.com/OCA/pos.git",
        ),
        (
            "OCA/pos@17.0-fix",
            "no merge @17.0-fix on https://github.com/OCA/pos.git",
        ),
        ("OCA/web#1", "no merge #1 on https://github.com/OCA/web.git"),
        (
            "OCA/nope#1",
            "no merge #1 on https://github.com/OCA/nope.git",
        ),
    ];
    for (text, message) in missing {
        assert_eq!(
            doc.drop_merge(&spec(text)),
            Err(EditError::NotFound(message.to_owned()))
        );
    }
    let mut bare = edit("[project]\n");
    assert!(matches!(
        bare.drop_merge(&spec("OCA/pos#1")),
        Err(EditError::NotFound(_))
    ));
    let mut odd = edit(
        "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nurl = \"https://github.com/OCA/pos.git\"\nmerges = [\"x\"]\n",
    );
    assert!(odd.drop_merge(&spec("OCA/pos#1")).is_err());
}

#[test]
fn add_repositories_skips_known_urls() {
    let mut doc = edit(PROJECT);
    let repos = crate::compat::import(
        &crate::compat::parse_repos(
            "./pos:\n  remotes:\n    o: https://github.com/OCA/pos\n  merges: [o 17.0]\n./crm:\n  remotes:\n    o: https://github.com/OCA/crm.git\n  merges: [o 17.0]\n",
        )
        .unwrap(),
        None,
    );
    let skipped = doc.add_repositories(&repos.repositories, None).unwrap();
    assert_eq!(skipped, ["https://github.com/OCA/pos"]);
    assert!(doc.to_string().ends_with("[[tool.odx.repositories]]\nname = \"crm\"\nurl = \"https://github.com/OCA/crm.git\"\ntarget_dir = \"crm\"\nbase = \"17.0\"\n"));
}

#[test]
fn add_package_as_direct_reference() {
    let mut doc = edit(PROJECT);
    for (dist, text, sub, expected) in [
        (
            "odoo-addon-a",
            "OCA/pos#7",
            Some("a"),
            "odoo-addon-a @ git+https://github.com/OCA/pos.git@refs/pull/7/head#subdirectory=a",
        ),
        (
            "odoo-addon-b",
            "https://gitlab.com/g/p/-/merge_requests/3",
            None,
            "odoo-addon-b @ git+https://gitlab.com/g/p.git@refs/merge-requests/3/head",
        ),
        (
            "odoo-addon-c",
            "OCA/pos@abcdef1",
            None,
            "odoo-addon-c @ git+https://github.com/OCA/pos.git@abcdef1",
        ),
    ] {
        assert_eq!(
            doc.add_package(dist, &spec(text), sub, Some("oca_unmerged"), None),
            Ok(Added::DirectReference)
        );
        assert!(
            doc.to_string().contains(&format!("  \"{expected}\",\n")),
            "{expected}"
        );
    }
    assert_eq!(
        doc.add_package(
            "odoo_addon.pos_vouchers",
            &spec("OCA/pos#45"),
            None,
            Some("oca_unmerged"),
            None
        ),
        Ok(Added::AlreadyPresent)
    );
}

#[test]
fn add_package_as_entry() {
    let mut doc = edit(PROJECT);
    assert_eq!(
        doc.add_package(
            "odoo-addon-x",
            &spec("OCA/pos#7"),
            Some("x"),
            Some("oca"),
            None
        ),
        Ok(Added::PackageEntry)
    );
    assert_eq!(
        doc.add_package("odoo-addon-y", &spec("OCA/pos@abcdef1"), None, None, None),
        Ok(Added::PackageEntry)
    );
    assert_eq!(
        doc.add_package(
            "odoo-addon-z",
            &spec("OCA/pos@17.0"),
            None,
            Some("missing"),
            None
        ),
        Ok(Added::PackageEntry)
    );
    assert_eq!(
        doc.add_package("odoo-addon-x", &spec("OCA/pos#7"), None, None, None),
        Ok(Added::AlreadyPresent)
    );
    let text = doc.to_string();
    assert!(text.contains("[tool.odx.packages]\nodoo-addon-x = { url = \"https://github.com/OCA/pos.git\", pr = 7, subdirectory = \"x\", group = \"oca\" }\nodoo-addon-y = { url = \"https://github.com/OCA/pos.git\", rev = \"abcdef1\" }\nodoo-addon-z = { url = \"https://github.com/OCA/pos.git\", branch = \"17.0\", group = \"missing\" }\n"), "{text}");
}

#[test]
fn drop_package_everywhere() {
    let mut doc = edit(PROJECT);
    doc.add_package("odoo-addon-x", &spec("OCA/pos#7"), None, None, None)
        .unwrap();
    doc.drop_package("odoo-addon-x", &[]).unwrap();
    doc.drop_package("odoo-addon-pos-vouchers", &["oca_unmerged", "missing"])
        .unwrap();
    doc.drop_package("odoo-addon-reports", &[]).unwrap();
    let text = doc.to_string();
    assert!(
        !text.contains("odoo-addon-x")
            && !text.contains("pos_vouchers @")
            && !text.contains("odoo-addon-reports")
    );
    assert_eq!(
        doc.drop_package("odoo-addon-ghost", &["oca"]),
        Err(EditError::NotFound(
            "no package odoo-addon-ghost".to_owned()
        ))
    );
}

#[test]
fn dropping_a_package_never_creates_tool_odx() {
    let text = "[project]\nname = \"x\"\n\n[tool.uv.sources]\nodoo-addon-x = { git = \"https://x/x.git\" }\n";
    let mut doc = edit(text);
    doc.drop_package("odoo-addon-x", &[]).unwrap();
    assert_eq!(
        doc.to_string(),
        "[project]\nname = \"x\"\n\n[tool.uv.sources]\n"
    );
}

#[test]
fn remaining_target_kinds() {
    let mut doc = edit(PROJECT);
    assert_eq!(
        doc.add_merge(&spec("OCA/pos@abcdef12"), None),
        Ok(Added::Merge)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/crm@17.0-x"), None),
        Ok(Added::Repository)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/pos@17.0-a"), None),
        Ok(Added::Merge)
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/pos@17.0-b"), None),
        Ok(Added::Merge)
    );
    doc.drop_merge(&spec("OCA/pos@17.0-b")).unwrap();
    assert!(doc.to_string().contains("{ branch = \"17.0-a\" }"));
    assert_eq!(
        doc.add_package(
            "odoo-addon-q",
            &spec("OCA/pos@17.0-q"),
            None,
            Some("oca_unmerged"),
            None
        ),
        Ok(Added::DirectReference)
    );
    assert!(
        doc.to_string()
            .contains("\"odoo-addon-q @ git+https://github.com/OCA/pos.git@17.0-q\"")
    );
    assert_eq!(
        doc.add_merge(&spec("OCA/sale@abcdef9"), None),
        Ok(Added::Repository)
    );
    assert!(doc.to_string().contains("    { sha = \"abcdef9\" },\n"));
    let mut bare = edit("[project]\n");
    assert_eq!(
        bare.add_package("x", &spec("OCA/pos#1"), None, None, None),
        Err(EditError::NoOdxTable)
    );
    assert_eq!(bare.add_repositories(&[], None), Err(EditError::NoOdxTable));
}

#[test]
fn repo_names() {
    assert_eq!(repo_name("git@github.com:OCA/pos.git"), "pos");
    assert_eq!(repo_name("https://forge.example/x/"), "x");
}

#[test]
fn glue_is_ignored_with_a_reason() {
    let mut edit = PyprojectEdit::parse("[tool.odx]\nodoo_version = \"17.0\"\n").unwrap();
    edit.ignore_glue("pos_x_sale", "no credit sales").unwrap();
    edit.ignore_glue("pos_y_sale", "later").unwrap();
    assert_eq!(
        edit.to_string(),
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.glue]\nignore = { pos_x_sale = \"no credit sales\", pos_y_sale = \"later\" }\n"
    );
    let mut none = PyprojectEdit::parse("[project]\n").unwrap();
    assert_eq!(none.ignore_glue("x", "y"), Err(EditError::NoOdxTable));
    for (text, key) in [
        ("[tool.odx]\nglue = 1\n", "[tool.odx.glue]"),
        ("[tool.odx.glue]\nignore = 1\n", "[tool.odx.glue] ignore"),
    ] {
        let mut bad = PyprojectEdit::parse(text).unwrap();
        assert_eq!(bad.ignore_glue("x", "y"), Err(EditError::Malformed(key)));
    }
}

#[test]
fn wanted_addons_are_added_changed_and_removed() {
    let mut edit = PyprojectEdit::parse("[tool.odx]\nodoo_version = \"17.0\"\n").unwrap();
    edit.want("contract", &[], None, None).unwrap();
    edit.want(
        "agreement",
        &["17.0".to_owned(), "18.0".to_owned()],
        Some("OCA/contract"),
        Some("customer asks in Q3"),
    )
    .unwrap();
    edit.want("contract", &[], None, Some("later")).unwrap();
    // Only what is given changes.
    edit.want("agreement", &["18.0".to_owned()], None, None)
        .unwrap();
    assert_eq!(
        edit.to_string(),
        "[tool.odx]\nodoo_version = \"17.0\"\n\n[[tool.odx.wanted]]\naddon = \"contract\"\nnote = \"later\"\n\n[[tool.odx.wanted]]\naddon = \"agreement\"\nseries = [\"18.0\"]\nrepository = \"OCA/contract\"\nnote = \"customer asks in Q3\"\n"
    );
    edit.unwant("contract").unwrap();
    assert!(!edit.to_string().contains("\"contract\""));
    assert_eq!(
        edit.unwant("contract"),
        Err(EditError::NotFound("contract is not wanted".to_owned()))
    );
    let mut bad = PyprojectEdit::parse("[tool.odx]\nwanted = 1\n").unwrap();
    assert_eq!(
        bad.want("x", &[], None, None),
        Err(EditError::Malformed("[tool.odx].wanted"))
    );
    let mut none = PyprojectEdit::parse("[project]\n").unwrap();
    assert_eq!(none.unwant("x"), Err(EditError::NoOdxTable));
}
