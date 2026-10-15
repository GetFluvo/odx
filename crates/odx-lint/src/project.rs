//! odx's project-level rules: what only the project knows (its series,
//! where its addons come from, what was released), reported as findings
//! like odoo-lint's.

use std::path::{Path, PathBuf};

use crate::Violation;

/// A rule of odx's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// `ODX001` and on.
    pub code: &'static str,
    /// Symbolic name.
    pub name: &'static str,
}

/// The manifest version is not of the project's series.
pub const VERSION_SERIES: Rule = Rule {
    code: "ODX001",
    name: "manifest-version-series",
};
/// A dependency nothing in the project provides.
pub const DEPENDS_UNAVAILABLE: Rule = Rule {
    code: "ODX002",
    name: "depends-unavailable",
};
/// A migration folder that does not fit the manifest version.
pub const MIGRATION_VERSION: Rule = Rule {
    code: "ODX003",
    name: "migration-folder-version",
};
/// The same addon from two sources.
pub const TWO_SOURCES: Rule = Rule {
    code: "ODX004",
    name: "addon-from-two-sources",
};
/// Changed since the last release without a version bump.
pub const VERSION_NOT_BUMPED: Rule = Rule {
    code: "ODX005",
    name: "version-not-bumped",
};

/// Every odx rule.
pub const RULES: &[Rule] = &[
    VERSION_SERIES,
    DEPENDS_UNAVAILABLE,
    MIGRATION_VERSION,
    TWO_SOURCES,
    VERSION_NOT_BUMPED,
];

/// Whether a `select`/`ignore` entry names `rule`: `ALL`, its code or name,
/// or a code prefix such as `ODX`.
#[must_use]
pub fn matches(selector: &str, rule: Rule) -> bool {
    selector.eq_ignore_ascii_case("all")
        || selector.eq_ignore_ascii_case(rule.code)
        || selector.eq_ignore_ascii_case(rule.name)
        || (!selector.is_empty()
            && rule
                .code
                .get(..selector.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(selector)))
}

/// Whether a selector names any odx rule, so it is odx's to apply rather
/// than odoo-lint's.
#[must_use]
pub fn is_odx(selector: &str) -> bool {
    !selector.eq_ignore_ascii_case("all") && RULES.iter().any(|rule| matches(selector, *rule))
}

/// Whether `rule` runs under `select` (none: every rule) and `ignore`.
#[must_use]
pub fn enabled(rule: Rule, select: Option<&[String]>, ignore: &[String]) -> bool {
    select.is_none_or(|select| select.iter().any(|selector| matches(selector, rule)))
        && !ignore.iter().any(|selector| matches(selector, rule))
}

/// An addon of the project, as the rules see it.
#[derive(Debug, Clone, Default)]
pub struct Addon {
    /// Its technical name.
    pub name: String,
    /// Its folder.
    pub dir: PathBuf,
    /// Its manifest file.
    pub manifest: PathBuf,
    /// The manifest's text.
    pub text: String,
    /// The manifest's `version`.
    pub version: Option<String>,
    /// The manifest's `depends`.
    pub depends: Vec<String>,
}

/// The 1-based line of `text` with `needle` on it, else 1.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .map_or(1, |at| at + 1)
}

fn finding(rule: Rule, file: &Path, line: usize, message: String) -> Violation {
    Violation {
        file_path: file.to_string_lossy().into_owned(),
        line,
        column: 1,
        code: rule.code.to_owned(),
        name: rule.name.to_owned(),
        message,
        fix: None,
    }
}

/// The version's place in the manifest.
fn version_line(addon: &Addon) -> usize {
    line_of(&addon.text, "version")
}

/// ODX001: the manifest version must start with the project's series.
#[must_use]
pub fn version_series(addon: &Addon, series: &str) -> Option<Violation> {
    let version = addon.version.as_deref()?;
    let ok = version
        .strip_prefix(series)
        .is_some_and(|rest| rest.starts_with('.'));
    (!ok).then(|| {
        finding(
            VERSION_SERIES,
            &addon.manifest,
            version_line(addon),
            format!("version {version} is not of the project's series {series}"),
        )
    })
}

/// ODX002: every dependency must be one of Odoo's own addons or provided
/// by the project.
pub fn depends_unavailable(addon: &Addon, available: &dyn Fn(&str) -> bool) -> Vec<Violation> {
    addon
        .depends
        .iter()
        .filter(|dep| !available(dep))
        .map(|dep| {
            finding(
                DEPENDS_UNAVAILABLE,
                &addon.manifest,
                line_of(&addon.text, &format!("{dep}'"))
                    .max(line_of(&addon.text, &format!("{dep}\""))),
                format!(
                    "depends on {dep}, which is neither Odoo's nor in the project (`odx check-deps --fix` adds released ones)"
                ),
            )
        })
        .collect()
}

/// Numeric parts of a version: `18.0.1.2.0` → `[18, 0, 1, 2, 0]`.
fn parts(version: &str) -> Option<Vec<u32>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// ODX003: a folder in `migrations/` must name a version of the series no
/// newer than the manifest's, or its scripts never run. `folders` are the
/// names in it.
#[must_use]
pub fn migration_folders(addon: &Addon, series: &str, folders: &[String]) -> Vec<Violation> {
    let Some(current) = addon.version.as_deref().and_then(parts) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for folder in folders {
        if folder == "tests" {
            continue;
        }
        let full = if folder.matches('.').count() == 2 {
            format!("{series}.{folder}")
        } else {
            folder.clone()
        };
        let why = match parts(&full) {
            None => Some("is no version".to_owned()),
            Some(_) if !full.starts_with(&format!("{series}.")) => {
                Some(format!("is not of the series {series}"))
            }
            Some(version) if version > current => Some(format!(
                "is newer than the manifest version {}",
                addon.version.as_deref().unwrap_or_default()
            )),
            Some(_) => None,
        };
        if let Some(why) = why {
            found.push(finding(
                MIGRATION_VERSION,
                &addon.dir.join("migrations").join(folder),
                1,
                format!("migrations/{folder} {why}: its scripts never run"),
            ));
        }
    }
    found
}

/// ODX004: an addon must come from one place; `sources` says where it
/// comes from.
#[must_use]
pub fn two_sources(addon: &Addon, sources: &[String]) -> Option<Violation> {
    (sources.len() > 1).then(|| {
        finding(
            TWO_SOURCES,
            &addon.manifest,
            1,
            format!("{} comes from {}", addon.name, sources.join(" and ")),
        )
    })
}

/// ODX005: an addon changed since the release `tag` must bump its version,
/// or the platform does not update it; `released` is the version at the
/// tag.
#[must_use]
pub fn version_not_bumped(addon: &Addon, tag: &str, released: &str) -> Option<Violation> {
    (addon.version.as_deref() == Some(released)).then(|| {
        finding(
            VERSION_NOT_BUMPED,
            &addon.manifest,
            version_line(addon),
            format!(
                "{} changed since {tag} but its version is still {released}: bump it, or the platform does not update it",
                addon.name
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(version: Option<&str>) -> Addon {
        Addon {
            name: "a".to_owned(),
            dir: PathBuf::from("addons/a"),
            manifest: PathBuf::from("addons/a/__manifest__.py"),
            text: "{\n    'name': 'A',\n    'version': '18.0.1.0.0',\n    'depends': ['base', \"gone\"],\n}\n".to_owned(),
            version: version.map(str::to_owned),
            depends: vec!["base".to_owned(), "gone".to_owned()],
        }
    }

    #[test]
    fn selection() {
        let all = ["ALL".to_owned()];
        let odx = ["odx".to_owned()];
        let named = ["version-not-bumped".to_owned()];
        assert!(enabled(VERSION_SERIES, None, &[]));
        assert!(enabled(VERSION_SERIES, Some(&all), &[]));
        assert!(enabled(VERSION_SERIES, Some(&odx), &[]));
        assert!(!enabled(VERSION_SERIES, Some(&named), &[]));
        assert!(enabled(VERSION_NOT_BUMPED, Some(&named), &[]));
        assert!(!enabled(VERSION_SERIES, None, &["ODX001".to_owned()]));
        assert!(!enabled(VERSION_SERIES, Some(&["C81".to_owned()]), &[]));
        assert!(!matches("", VERSION_SERIES));
        assert!(!matches("ODX0011", VERSION_SERIES));
        assert!(is_odx("ODX"));
        assert!(is_odx("depends-unavailable"));
        assert!(!is_odx("ALL"));
        assert!(!is_odx("C8101"));
    }

    #[test]
    fn series_of_versions() {
        assert!(version_series(&addon(Some("18.0.1.0.0")), "18.0").is_none());
        assert!(version_series(&addon(None), "18.0").is_none());
        let wrong = version_series(&addon(Some("17.0.1.0.0")), "18.0").unwrap();
        assert_eq!((wrong.line, wrong.code.as_str()), (3, "ODX001"));
        assert!(version_series(&addon(Some("18.01.0.0")), "18.0").is_some());
    }

    #[test]
    fn unavailable_dependencies() {
        let found = depends_unavailable(&addon(None), &|dep| dep == "base");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 4);
        assert!(found[0].message.starts_with("depends on gone,"));
        assert_eq!(line_of("x", "y"), 1);
    }

    #[test]
    fn migration_folder_versions() {
        let folders: Vec<String> = [
            "tests",
            "18.0.1.0.0",
            "1.0.0",
            "18.0.2.0.0",
            "17.0.1.0.0",
            "next",
            "2.0.0",
        ]
        .map(str::to_owned)
        .to_vec();
        let found = migration_folders(&addon(Some("18.0.1.0.0")), "18.0", &folders);
        let messages: Vec<&str> = found.iter().map(|v| v.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "migrations/18.0.2.0.0 is newer than the manifest version 18.0.1.0.0: its scripts never run",
                "migrations/17.0.1.0.0 is not of the series 18.0: its scripts never run",
                "migrations/next is no version: its scripts never run",
                "migrations/2.0.0 is newer than the manifest version 18.0.1.0.0: its scripts never run",
            ]
        );
        assert!(found[0].file_path.ends_with("18.0.2.0.0"));
        assert_eq!(migration_folders(&addon(None), "18.0", &folders), []);
        assert_eq!(migration_folders(&addon(Some("x")), "18.0", &folders), []);
    }

    #[test]
    fn sources_and_bumps() {
        let one = ["the project".to_owned()];
        let two = ["the project".to_owned(), "repository pos".to_owned()];
        assert!(two_sources(&addon(None), &one).is_none());
        let both = two_sources(&addon(None), &two).unwrap();
        assert_eq!(both.message, "a comes from the project and repository pos");
        let current = addon(Some("18.0.1.0.0"));
        assert!(version_not_bumped(&current, "v1", "18.0.1.0.1").is_none());
        let same = version_not_bumped(&current, "v1", "18.0.1.0.0").unwrap();
        assert_eq!((same.line, same.code.as_str()), (3, "ODX005"));
        assert!(same.message.starts_with("a changed since v1"));
    }
}
