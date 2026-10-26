//! Building the workspace: aggregate, lock, vendor, verify, sync, cache, daemon, doctor, stats and env.

use schemars::Schema;

/// The schemas of this group of commands.
pub(super) fn schemas() -> Vec<(&'static str, Schema)> {
    Vec::new()
}
