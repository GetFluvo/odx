//! Local development workspaces for odx: addons path, editable installs and package develop mode.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `odx dev` writes the
//! addons path and `odoo.conf`; `odx dev <package>` switches a package to a
//! local editable clone and `--restore` puts the locked source back,
//! byte for byte.

use std::collections::BTreeMap;

use odx_config::deps::{
    direct_references, normalize_name, parse_requirement, replace_requirement, set_uv_source_entry,
    uv_source_entry,
};
use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

/// `text` (an `odoo.conf`) with `addons_path` set to `path` in the
/// `[options]` section: the existing line replaced, else a line added at
/// the top of the section, else the section added.
pub fn set_addons_path(text: &str, path: &str) -> String {
    let line = format!("addons_path = {path}");
    let mut out = Vec::new();
    let mut in_options = false;
    let mut done = false;
    for current in text.lines() {
        let trimmed = current.trim();
        if trimmed.starts_with('[') {
            in_options = trimmed == "[options]";
            out.push(current.to_owned());
            if in_options && !done {
                out.push(line.clone());
                done = true;
            }
            continue;
        }
        let key = trimmed.split(['=', ':']).next().unwrap_or_default().trim();
        if in_options && key == "addons_path" {
            continue;
        }
        out.push(current.to_owned());
    }
    if !done {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push("[options]".to_owned());
        out.push(line);
    }
    out.join("\n") + "\n"
}

/// What `odx dev <package>` changed in `pyproject.toml`, so `--restore`
/// can undo it exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    /// The `[tool.uv.sources]` key used.
    pub key: String,
    /// The source entry before, as written; `None` when there was none.
    pub source: Option<String>,
    /// The direct reference in a group before, as written.
    pub requirement: Option<String>,
    /// What it was replaced with.
    pub replacement: Option<String>,
    /// The local path the source points at now.
    pub path: String,
}

/// `.odx/develop.toml`: the packages in develop mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevelopState {
    /// By distribution name.
    #[serde(default)]
    pub packages: BTreeMap<String, Saved>,
}

impl DevelopState {
    /// Parse the file.
    ///
    /// # Errors
    ///
    /// When it is not a develop state file.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.message().to_owned())
    }

    /// The file text.
    pub fn render(&self) -> String {
        format!(
            "# Packages in develop mode; `odx dev --restore` reads this.\n{}",
            toml::to_string(self).unwrap_or_default()
        )
    }
}

/// Point `dist` at the editable local `path`: its `[tool.uv.sources]`
/// entry becomes `{ path, editable = true }` and a direct reference in a
/// group becomes the bare requirement (extras and markers kept).
pub fn enter(doc: &mut DocumentMut, dist: &str, path: &str) -> Saved {
    let (key, source) = uv_source_entry(doc, dist).map_or_else(
        || (normalize_name(dist), None),
        |(key, value)| (key, Some(value)),
    );
    let requirement = direct_references(doc, dist).into_iter().next();
    let replacement = requirement
        .as_deref()
        .and_then(|text| parse_requirement(text).ok())
        .map(|req| {
            let extras = req
                .extras
                .map(|extras| format!("[{extras}]"))
                .unwrap_or_default();
            let markers = req
                .markers
                .map(|markers| format!(" ; {markers}"))
                .unwrap_or_default();
            format!("{}{extras}{markers}", req.name)
        });
    if let (Some(from), Some(to)) = (&requirement, &replacement) {
        replace_requirement(doc, dist, from, to);
    }
    let value = format!(
        " {{ path = {}, editable = true }}",
        toml_edit::Value::from(path)
    );
    set_uv_source_entry(doc, &key, Some(&value));
    Saved {
        key,
        source,
        requirement,
        replacement,
        path: path.to_owned(),
    }
}

/// Undo [`enter`].
pub fn leave(doc: &mut DocumentMut, dist: &str, saved: &Saved) {
    set_uv_source_entry(doc, &saved.key, saved.source.as_deref());
    if let (Some(from), Some(to)) = (&saved.replacement, &saved.requirement) {
        replace_requirement(doc, dist, from, to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odoo_conf() {
        assert_eq!(
            set_addons_path(
                "[options]\nadmin_passwd = x\naddons_path = /old\ndb_name = d\n",
                "/a,/b"
            ),
            "[options]\naddons_path = /a,/b\nadmin_passwd = x\ndb_name = d\n"
        );
        assert_eq!(
            set_addons_path("[other]\naddons_path = keep\n", "/a"),
            "[other]\naddons_path = keep\n\n[options]\naddons_path = /a\n"
        );
        assert_eq!(set_addons_path("", "/a"), "[options]\naddons_path = /a\n");
        assert_eq!(
            set_addons_path(
                "[options]\naddons_path: /old\n[x]\n[options]\nk = v\n",
                "/a"
            ),
            "[options]\naddons_path = /a\n[x]\n[options]\nk = v\n"
        );
    }

    const PYPROJECT: &str = "[project]\nname = \"p\"\n\n[project.optional-dependencies]\n\
        oca = [\"odoo-addon-x[e] @ git+https://x/r.git@v1#subdirectory=x ; python_version > '3'\", \"other\"]\n\n\
        [tool.uv.sources]\nodoo-addon-y = { git = \"https://y\", tag = \"t\" }  # vaulted\n";

    #[test]
    fn develop_mode_round_trips() {
        let mut doc: DocumentMut = PYPROJECT.parse().unwrap();
        let x = enter(&mut doc, "odoo_addon_x", ".src/x/x");
        let y = enter(&mut doc, "odoo-addon-y", ".src/y");
        let text = doc.to_string();
        assert!(
            text.contains("oca = [\"odoo-addon-x[e] ; python_version > '3'\", \"other\"]"),
            "{text}"
        );
        assert!(
            text.contains("odoo-addon-y = { path = \".src/y\", editable = true }\n"),
            "{text}"
        );
        assert!(
            text.contains("odoo-addon-x = { path = \".src/x/x\", editable = true }"),
            "{text}"
        );
        assert_eq!(x.source, None);
        assert_eq!(y.requirement, None);
        let state = DevelopState {
            packages: BTreeMap::from([
                ("odoo-addon-x".to_owned(), x),
                ("odoo-addon-y".to_owned(), y),
            ]),
        };
        let parsed = DevelopState::parse(&state.render()).unwrap();
        assert_eq!(parsed, state);
        for (dist, saved) in &parsed.packages {
            leave(&mut doc, dist, saved);
        }
        assert_eq!(doc.to_string(), PYPROJECT);
        assert!(DevelopState::parse("other = 1").is_err());
    }
}
