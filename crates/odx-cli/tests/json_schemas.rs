//! The JSON Schemas under `docs/json/` are those of the `--json` outputs.
//! `ODX_UPDATE_SCHEMAS=1 cargo test --test json_schemas` rewrites them.

use std::path::Path;

#[test]
fn published_schemas_are_current() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/json");
    let update = std::env::var_os("ODX_UPDATE_SCHEMAS").is_some();
    let mut stale = Vec::new();
    for (name, schema) in odx_cli::json::schemas() {
        let file = dir.join(format!("{name}.schema.json"));
        let text = format!("{}\n", serde_json::to_string_pretty(&schema).unwrap());
        if update {
            std::fs::write(&file, &text).unwrap();
        } else if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
            stale.push(file.display().to_string());
        }
    }
    assert!(
        stale.is_empty(),
        "stale, run ODX_UPDATE_SCHEMAS=1 cargo test --test json_schemas: {stale:?}"
    );
}
