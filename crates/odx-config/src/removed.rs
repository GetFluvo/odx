//! Keys a release removed. An unknown-key error for one of them says which
//! release removed it and what to use instead, so an upgrade of odx does not
//! leave a bare "unknown field" behind.

/// A key that was accepted once and is gone.
struct Removed {
    /// The table below `[tool.odx]`, `""` for `[tool.odx]` itself.
    table: &'static str,
    key: &'static str,
    release: &'static str,
    /// What to do instead, if anything replaces it.
    instead: Option<&'static str>,
}

/// Every removed key; 0.0.9 removed the keys that were accepted but never read.
const REMOVED: &[Removed] = &[
    gone(
        "http",
        "cache_ttl",
        Some("set `refs` and `pr_state` in [tool.odx.cache.ttl]"),
    ),
    gone("http", "offline", Some("pass --offline")),
    gone("cache", "warm_worktrees", None),
    gone("cache.ttl", "index", None),
    gone("cache.ttl", "wheels", None),
    gone("cache.ttl", "suggest", None),
    gone("cache.ttl", "drift", None),
    gone(
        "dashboard",
        "show_backports",
        Some("list `backport` in `sections`"),
    ),
    gone("upgrade", "check_pypi", None),
    gone("upgrade", "github_orgs", None),
    gone("port", "blacklist", None),
    gone("port", "migrate", None),
    gone("port", "rewrites", None),
    gone("suggest", "mutes_file", None),
    gone("platform", "sbom", None),
    gone("bot", "group_prs", None),
];

const fn gone(table: &'static str, key: &'static str, instead: Option<&'static str>) -> Removed {
    Removed {
        table,
        key,
        release: "0.0.9",
        instead,
    }
}

/// The table below `[tool.odx]` whose header is in force at `offset` of
/// `text`: `cache.ttl` for `[tool.odx.cache.ttl]`, `""` for `[tool.odx]`,
/// `None` outside `[tool.odx]`.
fn table_at(text: &str, offset: usize) -> Option<&str> {
    let before = text.get(..offset).unwrap_or(text);
    let header = before
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with('['))?;
    let name = header.trim_matches(|c: char| c == '[' || c == ']' || c.is_whitespace());
    if name == "tool.odx" {
        Some("")
    } else {
        name.strip_prefix("tool.odx.")
    }
}

/// `message` with what replaced the key, when the parser's `message` at
/// `offset` of `text` is an unknown field that a release removed. The key
/// may sit under its own header, or in an inline or dotted table below the
/// header in force (`dashboard = { show_backports = true }` under
/// `[tool.odx]`).
pub(crate) fn explain(text: &str, offset: usize, message: &str) -> Option<String> {
    let key = message.strip_prefix("unknown field `")?.split_once('`')?.0;
    let table = table_at(text, offset)?;
    let below = |removed: &Removed| {
        table.is_empty()
            || removed
                .table
                .strip_prefix(table)
                .is_some_and(|rest| rest.starts_with('.'))
    };
    let removed = REMOVED
        .iter()
        .find(|removed| removed.key == key && (removed.table == table || below(removed)))?;
    let mut note = format!(
        "`{}.{key}` was removed in odx {}, it had no effect",
        removed.table, removed.release
    );
    if let Some(instead) = removed.instead {
        note.push_str("; ");
        note.push_str(instead);
    }
    Some(format!("{message}\n{note}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_by_header() {
        let text = "[project]\nx = 1\n[tool.odx]\na = 1\n[ tool.odx.cache.ttl ]\nb = 1\n[[tool.odx.repositories]]\nc = 1\n";
        assert_eq!(table_at(text, 0), None);
        assert_eq!(table_at(text, text.find("x =").unwrap()), None);
        assert_eq!(table_at(text, text.find("a =").unwrap()), Some(""));
        assert_eq!(table_at(text, text.find("b =").unwrap()), Some("cache.ttl"));
        assert_eq!(
            table_at(text, text.find("c =").unwrap()),
            Some("repositories")
        );
        assert_eq!(table_at(text, text.len() + 1), Some("repositories"));
    }

    #[test]
    fn removals_name_their_release() {
        // `gone` runs at compile time for REMOVED; this runs it once more.
        assert_eq!(gone("http", "offline", None).release, "0.0.9");
        assert!(REMOVED.iter().all(|removed| !removed.release.is_empty()));
    }

    #[test]
    fn every_removed_key_is_rejected_with_its_note() {
        for removed in REMOVED {
            let (table, key) = (removed.table, removed.key);
            for text in [
                format!("[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.{table}]\n{key} = 1\n"),
                format!("[tool.odx]\nodoo_version = \"17.0\"\n{table}.{key} = 1\n"),
            ] {
                let err = crate::parse_pyproject(&text, std::path::Path::new("p.toml"))
                    .unwrap_err()
                    .to_string();
                assert!(
                    err.contains(&format!("`{table}.{key}` was removed")),
                    "{err}"
                );
            }
        }
    }

    #[test]
    fn only_removed_keys_are_explained() {
        let text = "[tool.odx.http]\noffline = true\n";
        let at = text.find("offline").unwrap();
        let message = "unknown field `offline`, expected `timeout`";
        assert_eq!(
            explain(text, at, message).unwrap(),
            "unknown field `offline`, expected `timeout`\n`http.offline` was removed in odx 0.0.9, it had no effect; pass --offline"
        );
        assert_eq!(
            explain(
                "[tool.odx.cache]\nwarm_worktrees = true\n",
                17,
                "unknown field `warm_worktrees`"
            )
            .unwrap(),
            "unknown field `warm_worktrees`\n`cache.warm_worktrees` was removed in odx 0.0.9, it had no effect"
        );
        // Another table, another key, another error.
        assert_eq!(
            explain("[tool.odx.mirror]\noffline = 1\n", 18, message),
            None
        );
        assert_eq!(explain(text, at, "unknown field `retries`"), None);
        // Inline and dotted tables below the header in force.
        for text in [
            "[tool.odx]\ndashboard = { show_backports = true }\n",
            "[tool.odx.cache]\nttl.index = \"1d\"\n",
        ] {
            let key = if text.contains("ttl") {
                "index"
            } else {
                "show_backports"
            };
            let explained = explain(text, text.len() - 2, &format!("unknown field `{key}`"));
            assert!(
                explained.unwrap().contains("was removed in odx 0.0.9"),
                "{text}"
            );
        }
        assert_eq!(
            explain("[tool.odx.cache]\nhttp = { offline = 1 }\n", 30, message),
            None
        );
        assert_eq!(explain(text, at, "invalid type: integer"), None);
        assert_eq!(explain(text, at, "unknown field `offline"), None);
        assert_eq!(explain("offline = 1\n", 0, message), None);
    }
}
