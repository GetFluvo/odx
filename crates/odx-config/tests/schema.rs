//! The documented configuration parses, and every enum value is accepted.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers should fail loudly"
)]

use std::path::Path;

use odx_config::schema::{
    BlacklistMode, BotAction, DashboardSection, IndexSource, MergirafMode, MigrateTool, OnConflict,
    OnHeadChange, PlatformProfile, Preset, RefStyle, Role,
};
use odx_config::{MergeSource, parse_pyproject};

#[test]
fn full_documented_example_parses() {
    let text = include_str!("fixtures/full.toml");
    let project = parse_pyproject(text, Path::new("/p/pyproject.toml"))
        .unwrap()
        .unwrap();
    let config = project.config;
    assert_eq!(project.warnings, []);
    assert_eq!(config.odoo_version, "17.0");
    assert_eq!(config.mergiraf, Some(MergirafMode::Auto));
    assert_eq!(
        config.mirror.as_ref().unwrap().ref_style,
        Some(RefStyle::Tags)
    );
    let repo = &config.repositories[0];
    assert_eq!(repo.name.as_deref(), Some("pos"));
    let sources: Vec<_> = repo.merges.iter().map(|m| m.source().unwrap()).collect();
    assert_eq!(
        sources,
        [
            MergeSource::Pr(123),
            MergeSource::Pr(456),
            MergeSource::Sha("a1b2c3d4e5f67890123456789012345678901234"),
            MergeSource::Branch {
                branch: "17.0-fix-rounding",
                remote: Some("https://github.com/acme/pos.git"),
            },
        ]
    );
    assert_eq!(repo.merges[1].on_conflict, Some(OnConflict::Skip));
    assert_eq!(repo.merges[1].on_head_change, Some(OnHeadChange::Pin));
    let groups = config.groups.as_ref().unwrap();
    assert_eq!(groups.default.as_deref(), Some("oca"));
    assert_eq!(groups.dirs.len(), 4);
    assert_eq!(
        config
            .cache
            .unwrap()
            .ttl
            .unwrap()
            .wheels
            .unwrap()
            .to_string(),
        "7d"
    );
    assert_eq!(config.features["pos-loyalty"].modules.len(), 2);
    let envs = config.environments.unwrap();
    assert_eq!(envs.preset, Some(Preset::MajorBranches));
    assert_eq!(envs.env["prod"].role, Some(Role::Prod));
    assert_eq!(
        config.platform.unwrap().profile,
        Some(PlatformProfile::GenericWheels)
    );
    assert_eq!(config.port.unwrap().migrate, Some(MigrateTool::OdooLint));
    assert_eq!(
        config.index.unwrap().overrides["base_tier_validation"]["19.0"],
        "OCA/tier-validation"
    );
}

/// Parse `value` for `key` in `table` and return the deserialized config.
fn with(table: &str, key: &str, value: &str) -> odx_config::OdxConfig {
    let text =
        format!("[tool.odx]\nodoo_version = \"17.0\"\n[tool.odx.{table}]\n{key} = {value}\n");
    parse_pyproject(&text, Path::new("p.toml"))
        .unwrap_or_else(|e| panic!("{table}.{key} = {value}: {e}"))
        .unwrap()
        .config
}

#[test]
fn every_enum_value_is_accepted() {
    for (value, expected) in [
        ("auto", MergirafMode::Auto),
        ("always", MergirafMode::Always),
        ("never", MergirafMode::Never),
    ] {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"{value}\"\n");
        let config = parse_pyproject(&text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config;
        assert_eq!(config.mergiraf, Some(expected));
    }
    for (value, expected) in [
        ("pin", OnHeadChange::Pin),
        ("follow-if-green", OnHeadChange::FollowIfGreen),
        ("follow", OnHeadChange::Follow),
    ] {
        let text = format!(
            "[tool.odx]\nodoo_version = \"17.0\"\non_head_change = \"{value}\"\non_conflict = \"skip\"\n"
        );
        let config = parse_pyproject(&text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config;
        assert_eq!(config.on_head_change, Some(expected));
    }
}

#[test]
fn port_and_platform_enums() {
    for (value, expected) in [
        ("auto", BlacklistMode::Auto),
        ("oca-port", BlacklistMode::OcaPort),
        ("mutes", BlacklistMode::Mutes),
        ("both", BlacklistMode::Both),
    ] {
        assert_eq!(
            with("port", "blacklist", &format!("\"{value}\""))
                .port
                .unwrap()
                .blacklist,
            Some(expected)
        );
    }
    for (value, expected) in [
        ("odoo-lint", MigrateTool::OdooLint),
        ("odoo-module-migrator", MigrateTool::OdooModuleMigrator),
        ("none", MigrateTool::None),
    ] {
        assert_eq!(
            with("port", "migrate", &format!("\"{value}\""))
                .port
                .unwrap()
                .migrate,
            Some(expected)
        );
    }
    for (value, expected) in [
        ("generic-wheels", PlatformProfile::GenericWheels),
        ("vendored-source", PlatformProfile::VendoredSource),
        ("odoo-sh", PlatformProfile::OdooSh),
        ("custom", PlatformProfile::Custom),
    ] {
        assert_eq!(
            with("platform", "profile", &format!("\"{value}\""))
                .platform
                .unwrap()
                .profile,
            Some(expected)
        );
    }
}

#[test]
fn environment_enums() {
    for (value, expected) in [
        ("major-branches", Preset::MajorBranches),
        ("series-branches", Preset::SeriesBranches),
        ("main-production", Preset::MainProduction),
        ("odoo-sh", Preset::OdooSh),
        ("explicit", Preset::Explicit),
    ] {
        assert_eq!(
            // Every preset with one environment of its own (explicit needs one).
            with(
                "environments",
                "preset",
                &format!("\"{value}\"\n[tool.odx.environments.env.a]\nbranch = \"a\"")
            )
            .environments
            .unwrap()
            .preset,
            Some(expected)
        );
    }
    let roles = with("environments.env.a", "role", "\"dev\"\nbranch = \"a\"");
    assert_eq!(roles.environments.unwrap().env["a"].role, Some(Role::Dev));
    for (value, expected) in [("test", Role::Test), ("uat", Role::Uat)] {
        let config = with(
            "environments.env.a",
            "role",
            &format!("\"{value}\"\nbranch = \"a\""),
        );
        assert_eq!(config.environments.unwrap().env["a"].role, Some(expected));
    }
}

#[test]
fn list_enums() {
    let sections = with(
        "dashboard",
        "sections",
        r#"["pinned", "droppable", "new-upstream", "forward-port", "auto-update", "drift"]"#,
    );
    assert_eq!(
        sections.dashboard.unwrap().sections.unwrap(),
        [
            DashboardSection::Pinned,
            DashboardSection::Droppable,
            DashboardSection::NewUpstream,
            DashboardSection::ForwardPort,
            DashboardSection::AutoUpdate,
            DashboardSection::Drift,
        ]
    );
    let sources = with(
        "index",
        "sources",
        r#"["overrides", "cache", "package-index", "forge", "oca-repo-list", "openupgrade"]"#,
    );
    assert_eq!(sources.index.unwrap().sources.unwrap().len(), 6);
    assert_eq!(
        with("index", "sources", r#"["openupgrade"]"#)
            .index
            .unwrap()
            .sources
            .unwrap(),
        [IndexSource::Openupgrade]
    );
    assert_eq!(
        with(
            "bot",
            "auto",
            r#"["drop-override", "repin", "auto-update"]"#
        )
        .bot
        .unwrap()
        .auto
        .unwrap(),
        [
            BotAction::DropOverride,
            BotAction::Repin,
            BotAction::AutoUpdate
        ]
    );
    assert_eq!(
        with("lock.auto_update", "policy", "\"none\"")
            .lock
            .unwrap()
            .auto_update
            .unwrap()
            .policy,
        Some(odx_config::schema::AutoUpdatePolicy::None)
    );
    let forge = "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nurl = \"u\"\nforge = \"gitea\"\n[[tool.odx.repositories]]\nurl = \"v\"\nforge = \"gitlab\"\n[[tool.odx.repositories]]\nurl = \"w\"\nforge = \"github\"\n";
    let repos = parse_pyproject(forge, Path::new("p.toml"))
        .unwrap()
        .unwrap()
        .config
        .repositories;
    assert_eq!(repos.len(), 3);
}

#[test]
fn bad_enum_value_names_the_alternatives() {
    let text = "[tool.odx]\nodoo_version = \"17.0\"\nmergiraf = \"sometimes\"\n";
    let err = parse_pyproject(text, Path::new("p.toml"))
        .unwrap_err()
        .to_string();
    assert!(
        err.starts_with("p.toml:3:12: unknown variant `sometimes`"),
        "{err}"
    );
}
