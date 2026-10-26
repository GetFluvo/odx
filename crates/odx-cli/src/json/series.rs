//! Reviewing and porting: changelog, diff, sniff, glue, check-deps, suggest, upgrade, migrate and port.

use schemars::{Schema, schema_for};

use crate::cmd;

/// The schemas of this group of commands.
pub(super) fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "changelog",
            schema_for!(cmd::changelog::ChangelogJson<'static>),
        ),
        ("diff", schema_for!(cmd::diff::DiffJson<'static>)),
        ("sniff", schema_for!(cmd::sniff::SniffJson<'static>)),
        ("glue", schema_for!(cmd::glue::GlueJson<'static>)),
        (
            "check-deps",
            schema_for!(cmd::check_deps::CheckDepsJson<'static>),
        ),
        ("suggest", schema_for!(cmd::suggest::SuggestJson<'static>)),
        ("upgrade", schema_for!(cmd::upgrade::UpgradeJson<'static>)),
        ("migrate", schema_for!(cmd::migrate::MigrateJson<'static>)),
        (
            "migrate-plan",
            schema_for!(cmd::upgrade::MigratePlanJson<'static>),
        ),
        (
            "migrate-plan-repository",
            schema_for!(cmd::migrate::MigratePlanRepositoryJson<'static>),
        ),
        (
            "migrate-pin",
            schema_for!(cmd::migrate::MigratePinJson<'static>),
        ),
        (
            "migrate-own",
            schema_for!(cmd::migrate::MigrateOwnJson<'static>),
        ),
        (
            "migrate-pr",
            schema_for!(cmd::migrate::MigratePrJson<'static>),
        ),
        ("migrate-update", schema_for!(cmd::update::Updated<'static>)),
        ("migrate-unpin", schema_for!(cmd::unpin::Unpinned)),
        ("port", schema_for!(cmd::port::PortJson<'static>)),
        ("port-sweep", schema_for!(cmd::sweep::SweepJson<'static>)),
    ]
}
