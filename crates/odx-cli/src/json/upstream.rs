//! Following upstream: status, dashboard, bot and promote.

use odx_dashboard::Dashboard;
use schemars::{Schema, schema_for};

use crate::cmd;

/// The schemas of this group of commands.
pub(super) fn schemas() -> Vec<(&'static str, Schema)> {
    let mut dashboard = schema_for!(Vec<Dashboard>);
    dashboard.insert("title".to_owned(), "odx dashboard".into());
    dashboard.insert(
        "description".to_owned(),
        "What `odx --json dashboard` prints: the dashboard of each series.".into(),
    );
    vec![
        ("status", schema_for!(cmd::status::StatusJson<'static>)),
        ("dashboard", dashboard),
        ("bot", schema_for!(cmd::bot::BotJson)),
        ("promote", schema_for!(cmd::promote::PromoteJson<'static>)),
        (
            "promote-search",
            schema_for!(cmd::promote::SearchJson<'static>),
        ),
        (
            "promote-rollback",
            schema_for!(cmd::promote_rollback::RollbackJson<'static>),
        ),
        (
            "lock-auto-update",
            schema_for!(cmd::autoupdate::AutoUpdateJson<'static>),
        ),
    ]
}
