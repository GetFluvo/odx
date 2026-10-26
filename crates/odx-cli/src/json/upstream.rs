//! Following upstream: status, dashboard, bot and promote.

use schemars::{Schema, schema_for};

use crate::cmd;

/// The schemas of this group of commands.
pub(super) fn schemas() -> Vec<(&'static str, Schema)> {
    vec![("status", schema_for!(cmd::status::StatusJson<'static>))]
}
