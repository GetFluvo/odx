//! Copying a plan's pins exactly: the requirement lines and
//! `[tool.uv.sources]` entries of `pyproject.toml`, the `[[package]]`
//! blocks of `uv.lock` and the entries of `odx.lock`.

use odx_config::deps::{
    normalize_name, set_requirement, set_uv_source_entry, uv_source_entry, written_requirements,
};
use odx_config::lock::Lockfile;
use odx_config::uvlock::UvLock;
use toml_edit::DocumentMut;

/// `to` with the requirements and uv sources of `dists` as `from` has
/// them: each requirement string in the same group (added when the target
/// lacks it), each source entry copied, or removed when `from` has none.
///
/// # Errors
///
/// When either text is no TOML, or the target lacks a group the source
/// declares one of them in.
pub fn pyproject(from: &str, to: &str, dists: &[String]) -> Result<String, String> {
    let ours: DocumentMut = from.parse().map_err(|e| format!("pyproject.toml: {e}"))?;
    let mut theirs: DocumentMut = to.parse().map_err(|e| format!("pyproject.toml: {e}"))?;
    for dist in dists {
        for (group, requirement) in written_requirements(&ours, dist) {
            if !set_requirement(&mut theirs, group.as_deref(), dist, &requirement) {
                return Err(format!(
                    "the target has no {} for {requirement}",
                    group.map_or_else(
                        || "[project] dependencies".to_owned(),
                        |group| format!("dependency group {group}")
                    )
                ));
            }
        }
        let source = uv_source_entry(&ours, dist);
        if let Some((key, _)) = uv_source_entry(&theirs, dist) {
            set_uv_source_entry(&mut theirs, &key, None);
        }
        if let Some((key, value)) = source {
            set_uv_source_entry(&mut theirs, &key, Some(&value));
        }
    }
    Ok(theirs.to_string())
}

/// `uv.lock` as the text before the first package and each `[[package]]`
/// block with its name.
fn blocks(text: &str) -> (&str, Vec<(String, &str)>) {
    let starts: Vec<usize> = text
        .match_indices("[[package]]\n")
        .map(|(at, _)| at)
        .filter(|at| *at == 0 || text[..*at].ends_with('\n'))
        .collect();
    let head = &text[..starts.first().copied().unwrap_or(text.len())];
    let ends = starts.iter().skip(1).copied().chain([text.len()]);
    let found = starts
        .iter()
        .zip(ends)
        .map(|(start, end)| {
            let block = &text[*start..end];
            let name = block
                .lines()
                .find_map(|line| line.strip_prefix("name = \""))
                .and_then(|rest| rest.split('"').next())
                .map(normalize_name)
                .unwrap_or_default();
            (name, block)
        })
        .collect();
    (head, found)
}

/// `to` with the `[[package]]` blocks of `dists` replaced by `from`'s,
/// verbatim, and those it lacks added in name order.
#[must_use]
pub fn uv_lock(from: &str, to: &str, dists: &[String]) -> String {
    let wanted: Vec<String> = dists.iter().map(|dist| normalize_name(dist)).collect();
    let (_, ours) = blocks(from);
    let (head, theirs) = blocks(to);
    let copied = |name: &str| -> Vec<&str> {
        ours.iter()
            .filter(|(other, _)| other == name)
            .map(|(_, block)| *block)
            .collect()
    };
    let mut result: Vec<(String, Vec<&str>)> = Vec::new();
    for (name, block) in &theirs {
        if !wanted.contains(name) {
            result.push((name.clone(), vec![*block]));
        } else if !result.iter().any(|(other, _)| other == name) {
            result.push((name.clone(), copied(name)));
        }
    }
    for name in &wanted {
        if !result.iter().any(|(other, _)| other == name) {
            let at = result
                .iter()
                .position(|(other, _)| other > name)
                .unwrap_or(result.len());
            result.insert(at, (name.clone(), copied(name)));
        }
    }
    let mut text = head.to_owned();
    for (_, list) in result {
        for block in list {
            if !text.is_empty() && !text.ends_with("\n\n") {
                text.push('\n');
            }
            text.push_str(block.trim_end_matches('\n'));
            text.push('\n');
        }
    }
    text
}

/// The packages whose blocks differ between `expected` and `actual` (the
/// project itself left out): what `uv lock` re-resolved.
#[must_use]
pub fn drift(expected: &str, actual: &str) -> Vec<String> {
    let root = UvLock::parse(actual)
        .ok()
        .and_then(|lock| lock.root().map(|root| root.name.clone()))
        .unwrap_or_default();
    let (_, before) = blocks(expected);
    let (_, after) = blocks(actual);
    let gather = |list: &[(String, &str)]| {
        let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        for (name, block) in list {
            map.entry(name.clone())
                .or_default()
                .push_str(block.trim_end());
        }
        map
    };
    let (before, after) = (gather(&before), gather(&after));
    let mut names: Vec<String> = before
        .keys()
        .chain(after.keys())
        .filter(|name| **name != root && before.get(*name) != after.get(*name))
        .cloned()
        .collect();
    names.sort();
    names.dedup();
    names
}

/// `to` with `from`'s `odx.lock` entries of `dists`, and its Odoo core
/// when `core`.
pub fn odx_lock(from: &Lockfile, to: &mut Lockfile, dists: &[String], core: bool) {
    for dist in dists {
        let wanted = normalize_name(dist);
        to.packages.retain(|name, _| normalize_name(name) != wanted);
        to.wheels.retain(|name, _| normalize_name(name) != wanted);
        for (name, entry) in &from.packages {
            if normalize_name(name) == wanted {
                to.packages.insert(name.clone(), entry.clone());
            }
        }
        for (name, entry) in &from.wheels {
            if normalize_name(name) == wanted {
                to.wheels.insert(name.clone(), entry.clone());
            }
        }
    }
    if core {
        to.odoo.clone_from(&from.odoo);
    }
}

#[cfg(test)]
mod tests {
    use odx_config::lock::{LockSource, LockedPackage, OdooPin};

    use super::*;

    #[test]
    fn requirements_and_sources() {
        let from = "[project]\nname = \"p\"\n\n[project.optional-dependencies]\n\
                    oca = [\"odoo-addon-a==2\", \"odoo-addon-g\"]\n\n\
                    [tool.uv.sources]\nodoo-addon-g = { git = \"https://x/g\", rev = \"c2\" }\n";
        let to = "[project]\nname = \"p\"\n\n[project.optional-dependencies]\n\
                  oca = [\"odoo-addon-a==1\", \"odoo-addon-h\"]\n\n\
                  [tool.uv.sources]\nodoo_addon_g = { git = \"https://x/g\", rev = \"c1\" }\n\
                  odoo-addon-h = { git = \"https://x/h\" }\n";
        let dists = ["odoo-addon-a", "odoo-addon-g", "odoo-addon-h"].map(str::to_owned);
        let copied = pyproject(from, to, &dists).unwrap();
        assert_eq!(
            copied,
            "[project]\nname = \"p\"\n\n[project.optional-dependencies]\n\
             oca = [\"odoo-addon-a==2\", \"odoo-addon-h\", \"odoo-addon-g\"]\n\n\
             [tool.uv.sources]\nodoo-addon-g = { git = \"https://x/g\", rev = \"c2\" }\n"
        );
        let other = "[project]\nname = \"p\"\n";
        assert_eq!(
            pyproject(from, other, &dists[..1]),
            Err("the target has no dependency group oca for odoo-addon-a==2".to_owned())
        );
        let top = "[project]\nname = \"p\"\ndependencies = [\"odoo-addon-a==2\"]\n";
        assert_eq!(
            pyproject(top, other, &dists[..1]),
            Err("the target has no [project] dependencies for odoo-addon-a==2".to_owned())
        );
        assert!(pyproject("x = ", to, &dists).is_err());
        assert!(pyproject(from, "x = ", &dists).is_err());
    }

    const HEAD: &str = "version = 1\nrequires-python = \">=3.10\"\n\n";

    fn package(name: &str, version: &str) -> String {
        format!(
            "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {{ registry = \"https://pypi.org/simple\" }}\n"
        )
    }

    #[test]
    fn lockfile_blocks() {
        let root = "[[package]]\nname = \"p\"\nversion = \"0.1\"\nsource = { virtual = \".\" }\n";
        let from = format!(
            "{HEAD}{}\n{}\n{}\n{}\n{root}",
            package("a", "2"),
            package("a", "3"),
            package("b", "2"),
            package("c", "1")
        );
        let to = format!(
            "{HEAD}{}\n{}\n{}\n{root}",
            package("a", "1"),
            package("a", "0"),
            package("d", "1")
        );
        let dists = ["A", "c", "nowhere"].map(str::to_owned);
        let copied = uv_lock(&from, &to, &dists);
        assert_eq!(
            copied,
            format!(
                "{HEAD}{}\n{}\n{}\n{}\n{root}",
                package("a", "2"),
                package("a", "3"),
                package("c", "1"),
                package("d", "1")
            )
        );
        assert_eq!(drift(&copied, &copied), Vec::<String>::new());
        let moved = copied.replace(
            "name = \"d\"\nversion = \"1\"",
            "name = \"d\"\nversion = \"2\"",
        );
        let rooted = moved.replace("version = \"0.1\"", "version = \"0.2\"");
        let gone = rooted.replace(&format!("{}\n", package("c", "1")), "");
        assert_eq!(drift(&copied, &gone), ["c", "d"]);
        assert_eq!(uv_lock("", "", &dists), "");
        assert_eq!(drift("x", "y"), Vec::<String>::new());
    }

    #[test]
    fn lock_entries() {
        let entry = |sha: &str| LockedPackage {
            group: None,
            url: "u".to_owned(),
            source: LockSource {
                pr: None,
                sha: None,
                branch: Some("19.0".to_owned()),
                remote: None,
                tag: None,
            },
            subdirectory: None,
            sha: sha.to_owned(),
            mirror_ref: None,
        };
        let mut from = Lockfile::new();
        from.odoo = Some(OdooPin {
            url: "u".to_owned(),
            rev: "new".to_owned(),
        });
        from.packages.insert("odoo-addon-a".to_owned(), entry("a2"));
        from.packages.insert("odoo-addon-b".to_owned(), entry("b2"));
        let mut wheels = Lockfile::new();
        wheels.wheels.insert(
            "odoo-addon-a".to_owned(),
            odx_config::lock::LockedWheel {
                version: "1+odx.x".to_owned(),
                sha256: "sha256:00".to_owned(),
                index: None,
                source: "package odoo-addon-a".to_owned(),
                tree: "t".to_owned(),
            },
        );
        from.wheels.clone_from(&wheels.wheels);
        let other = from.wheels["odoo-addon-a"].clone();
        from.wheels.insert("odoo-addon-z".to_owned(), other);
        let mut to = Lockfile::new();
        to.packages.insert("odoo_addon_a".to_owned(), entry("a1"));
        to.packages.insert("odoo-addon-c".to_owned(), entry("c1"));
        to.wheels.insert(
            "odoo_addon_a".to_owned(),
            wheels.wheels["odoo-addon-a"].clone(),
        );
        odx_lock(&from, &mut to, &["odoo-addon-a".to_owned()], false);
        let names: Vec<(&str, &str)> = to
            .packages
            .iter()
            .map(|(name, entry)| (name.as_str(), entry.sha.as_str()))
            .collect();
        assert_eq!(names, [("odoo-addon-a", "a2"), ("odoo-addon-c", "c1")]);
        assert_eq!(to.wheels.keys().collect::<Vec<_>>(), ["odoo-addon-a"]);
        assert_eq!(to.odoo, None);
        odx_lock(&from, &mut to, &[], true);
        assert_eq!(to.odoo, from.odoo);
    }
}
