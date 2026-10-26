//! Building the workspace: aggregate, lock, vendor, verify, sync, cache, daemon, doctor, stats and env.

use schemars::{Schema, schema_for};

use crate::cmd;

/// The schemas of this group of commands.
pub(super) fn schemas() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "aggregate",
            schema_for!(cmd::aggregate::AggregateJson<'static>),
        ),
        ("vendor", schema_for!(cmd::vendor::VendorJson)),
        ("verify", schema_for!(cmd::verify::VerifyJson<'static>)),
        ("sync", schema_for!(cmd::sync::SyncJson<'static>)),
        ("cache-info", schema_for!(cmd::sync::CacheInfoJson)),
        ("daemon-status", schema_for!(cmd::daemon::DaemonStatusJson)),
        ("doctor", schema_for!(cmd::doctor::DoctorJson<'static>)),
        ("stats", schema_for!(cmd::stats::StatsJson<'static>)),
        (
            "stats-per-addon",
            schema_for!(cmd::stats::StatsPerAddonJson<'static>),
        ),
        ("env-list", schema_for!(cmd::env::EnvListJson<'static>)),
        ("env-diff", schema_for!(cmd::env_diff::EnvDiffJson<'static>)),
    ]
}
