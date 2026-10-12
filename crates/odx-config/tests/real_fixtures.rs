//! Real-world `pyproject.toml` files are read correctly, and editing them
//! leaves every line that is not part of the edit exactly as it was.

use std::path::Path;

use odx_config::deps::{read_group, read_uv_git_sources, set_direct_ref};
use odx_config::edit::{Added, PyprojectEdit, parse_spec};
use odx_config::parse_pyproject;

const OCA_ADDON: &str = include_str!("fixtures/real/oca-addon-whool.toml");
const OCA_ROOT: &str = include_str!("fixtures/real/oca-repo-root.toml");
const HATCH_ODOO: &str = include_str!("fixtures/real/hatch-odoo.toml");
const UVAULT: &str = include_str!("fixtures/real/uvault.toml");
const CLIENT: &str = include_str!("fixtures/real/client-four-groups.toml");

/// Every line of `before` appears in `after`, in order: the edit only
/// added or replaced lines, it never reformatted untouched ones.
fn untouched_lines_survive(before: &str, after: &str, replaced: &[&str]) {
    let mut after_lines = after.lines();
    for line in before.lines().filter(|line| !replaced.contains(line)) {
        assert!(
            after_lines.any(|candidate| candidate == line),
            "line {line:?} was changed or reordered in:\n{after}"
        );
    }
}

#[test]
fn files_without_tool_odx_are_ignored() {
    for text in [OCA_ADDON, OCA_ROOT, HATCH_ODOO, UVAULT] {
        assert!(
            parse_pyproject(text, Path::new("pyproject.toml"))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn adding_odx_to_existing_projects_keeps_them_intact() {
    for text in [OCA_ADDON, OCA_ROOT, HATCH_ODOO, UVAULT] {
        let mut doc = PyprojectEdit::parse(text).unwrap();
        let added = doc
            .add_merge(&parse_spec("OCA/pos#123").unwrap(), Some("17.0"))
            .unwrap();
        assert_eq!(added, Added::Repository);
        let after = doc.to_string();
        assert!(
            after.starts_with(text),
            "the original text is a prefix:\n{after}"
        );
        assert!(
            parse_pyproject(&after, Path::new("pyproject.toml"))
                .unwrap()
                .is_some()
        );
    }
}

#[test]
fn uvault_sources_are_read() {
    let doc = UVAULT.parse().unwrap();
    let sources = read_uv_git_sources(&doc);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].tag.as_deref(), Some("apycod-my-package-abcdef1"));
    assert_eq!(sources[0].subdirectory.as_deref(), Some("my_package"));
}

#[test]
fn client_project_reads_and_edits_in_place() {
    let project = parse_pyproject(CLIENT, Path::new("pyproject.toml"))
        .unwrap()
        .unwrap();
    let groups = project.config.groups.unwrap();
    assert_eq!(groups.dirs.len(), 4);
    assert_eq!(groups.default.as_deref(), Some("oca"));

    let doc = CLIENT.parse().unwrap();
    let unmerged = read_group(&doc, "oca_unmerged").unwrap();
    assert!(unmerged.uses_direct_references());
    assert_eq!(read_group(&doc, "third_party").unwrap().requirements, []);
    assert_eq!(read_uv_git_sources(&doc).len(), 1);

    // Re-pointing a direct reference touches exactly that line.
    let mut doc = doc;
    let old = "    \"odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers\",";
    assert!(set_direct_ref(
        &mut doc,
        &unmerged,
        "odoo-addon-pos-vouchers",
        "git+https://forge.example/vault.git@refs/tags/odx/0123456789abcdef0123456789abcdef01234567#subdirectory=pos_vouchers",
    ));
    let after = doc.to_string();
    untouched_lines_survive(CLIENT, &after, &[old]);
    assert_eq!(after.lines().count(), CLIENT.lines().count());

    // Adding a package and a merge only adds lines.
    let mut edit = PyprojectEdit::parse(CLIENT).unwrap();
    let pr = parse_spec("OCA/pos#77").unwrap();
    assert_eq!(
        edit.add_package(
            "odoo-addon-pos_x",
            &pr,
            Some("pos_x"),
            Some("oca_unmerged"),
            None
        ),
        Ok(Added::DirectReference)
    );
    assert_eq!(edit.add_merge(&pr, None), Ok(Added::Repository));
    let after = edit.to_string();
    untouched_lines_survive(CLIENT, &after, &[]);
    assert!(
        parse_pyproject(&after, Path::new("pyproject.toml"))
            .unwrap()
            .is_some()
    );

    // Dropping it again restores the original file exactly, apart from the
    // repository table, which stays (it may carry other merges).
    edit.drop_package("odoo-addon-pos_x", &["oca_unmerged"])
        .unwrap();
    edit.drop_merge(&pr).unwrap();
    let after = edit.to_string();
    assert!(after.starts_with(CLIENT), "{after}");
}
