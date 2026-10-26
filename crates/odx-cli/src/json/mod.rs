//! The `--json` output of every command, as JSON Schema.
//!
//! Each output type derives [`JsonSchema`](schemars::JsonSchema); the
//! schemas are published under `docs/json/`, and a test keeps them current.
//! An output's `schema` field changes when a field is removed or changes
//! meaning; adding a field does not change it.

use schemars::Schema;

mod series;
mod upstream;
mod workspace;

/// Every `--json` output, by command.
#[must_use]
pub fn schemas() -> Vec<(&'static str, Schema)> {
    let mut all = workspace::schemas();
    all.extend(upstream::schemas());
    all.extend(series::schemas());
    all
}
