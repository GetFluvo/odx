//! The platform requirements file: the external Python packages the
//! deployed addons need, pinned to `uv.lock`, without what the platform
//! provides.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::normalize_name;
use crate::provided::ProvidedSet;

/// A Python requirement and who declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// Distribution name as written.
    pub name: String,
    /// Version specifier (`>=2.31`), empty when none.
    pub specifier: String,
    /// The addon, package or group that declares it.
    pub by: String,
}

/// A requirement whose range excludes every version the platform provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    /// Normalized distribution name.
    pub package: String,
    /// The requested range.
    pub specifier: String,
    /// Who asks for it.
    pub by: String,
    /// The versions the platform provides.
    pub provided: Vec<String>,
}

/// One line of the requirements file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    /// Normalized distribution name.
    pub package: String,
    /// `==<version>` from `uv.lock`, or the declared ranges when `uv.lock`
    /// has no single version.
    pub specifier: String,
    /// Who needs it, sorted.
    pub via: Vec<String>,
}

/// What `odx vendor --requirements` writes and reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlatformRequirements {
    /// Requirement lines, sorted by package.
    pub lines: Vec<Line>,
    /// Provided packages that were left out.
    pub omitted: Vec<String>,
    /// Ranges that exclude the provided version.
    pub conflicts: Vec<Conflict>,
    /// Packages without a single version in `uv.lock`.
    pub unresolved: Vec<String>,
}

/// Whether `name` is Odoo or an Odoo addon package: those are delivered as
/// source or wheels, never through the requirements file.
pub fn is_odoo(name: &str) -> bool {
    let name = normalize_name(name);
    name == "odoo"
        || name.starts_with("odoo-addon-")
        || name
            .strip_prefix("odoo")
            .and_then(|rest| rest.split_once("-addon-"))
            .is_some_and(|(series, _)| series.chars().all(|c| c.is_ascii_digit()))
}

/// Build the requirements from `declared`, pinning to `locked` (normalized
/// name → versions in `uv.lock`) and leaving out what `provided` has.
pub fn build(
    declared: &[Declared],
    provided: &ProvidedSet,
    locked: &BTreeMap<String, Vec<String>>,
) -> PlatformRequirements {
    let mut by_package: BTreeMap<String, Vec<&Declared>> = BTreeMap::new();
    for item in declared.iter().filter(|item| !is_odoo(&item.name)) {
        by_package
            .entry(normalize_name(&item.name))
            .or_default()
            .push(item);
    }
    let mut result = PlatformRequirements::default();
    for (package, items) in by_package {
        if provided.contains(&package) {
            result.conflicts.extend(
                items
                    .iter()
                    .filter(|item| provided.excludes(&package, &item.specifier))
                    .map(|item| Conflict {
                        package: package.clone(),
                        specifier: item.specifier.clone(),
                        by: item.by.clone(),
                        provided: provided.versions(&package).to_vec(),
                    }),
            );
            result.omitted.push(package);
            continue;
        }
        let mut via: Vec<String> = items.iter().map(|item| item.by.clone()).collect();
        via.sort();
        via.dedup();
        let specifier = if let Some([version]) = locked.get(&package).map(Vec::as_slice) {
            format!("=={version}")
        } else {
            result.unresolved.push(package.clone());
            let mut ranges: Vec<&str> = items
                .iter()
                .map(|item| item.specifier.as_str())
                .filter(|specifier| !specifier.is_empty())
                .collect();
            ranges.sort_unstable();
            ranges.dedup();
            ranges.join(",")
        };
        result.lines.push(Line {
            package,
            specifier,
            via,
        });
    }
    result
}

const DIGEST: &str = "# odx-digest: ";

/// The file text: a header naming the profile and provided set, one line
/// per package with who needs it, and a digest that tells hand edits apart.
pub fn render(requirements: &PlatformRequirements, header: &str) -> String {
    let mut body = String::new();
    for line in &requirements.lines {
        let _ = writeln!(
            body,
            "{}{}  # via {}",
            line.package,
            line.specifier,
            line.via.join(", ")
        );
    }
    with_header(&body, header)
}

/// `body` under odx's header and digest.
pub fn with_header(body: &str, header: &str) -> String {
    format!(
        "# Generated by `odx vendor --requirements`; edits are overwritten.\n# {header}\n{DIGEST}{}\n{body}",
        digest(body)
    )
}

/// Whether `text`, a requirements file odx wrote, was edited by hand since:
/// its body no longer matches its digest. A file without a digest was not
/// written by odx and counts as edited.
pub fn edited(text: &str) -> bool {
    let mut lines = text.split_inclusive('\n');
    let recorded = lines
        .by_ref()
        .find_map(|line| line.trim_end().strip_prefix(DIGEST).map(str::to_owned));
    recorded.is_none_or(|recorded| recorded != digest(&lines.collect::<String>()))
}

fn digest(body: &str) -> String {
    Sha256::digest(body.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(name: &str, specifier: &str, by: &str) -> Declared {
        Declared {
            name: name.to_owned(),
            specifier: specifier.to_owned(),
            by: by.to_owned(),
        }
    }

    #[test]
    fn odoo_packages() {
        for name in [
            "odoo",
            "Odoo",
            "odoo-addon-web_responsive",
            "odoo14-addon-x",
        ] {
            assert!(is_odoo(name), "{name}");
        }
        for name in ["odoorpc", "odoo-test-helper", "odoox-addon-y", "requests"] {
            assert!(!is_odoo(name), "{name}");
        }
    }

    #[test]
    fn requirements_from_declarations() {
        let provided = ProvidedSet::parse("odoo-standard-17.0", "Babel==2.10.3\nlxml\n");
        let locked = BTreeMap::from([
            ("python-stdnum".to_owned(), vec!["1.19".to_owned()]),
            (
                "requests".to_owned(),
                vec!["2.31.0".to_owned(), "2.32.0".to_owned()],
            ),
        ]);
        let result = build(
            &[
                declared("python_stdnum", "", "l10n_nl_x"),
                declared("python-stdnum", ">=1.18", "group oca"),
                declared("requests", ">=2.30", "pos_x"),
                declared("requests", "<3", "pos_y"),
                declared("unknown", "", "pos_x"),
                declared("babel", ">=2.11", "pos_x"),
                declared("babel", ">=2.10", "pos_y"),
                declared("lxml", "", "pos_y"),
                declared("odoo-addon-base_x", "", "pos_x"),
            ],
            &provided,
            &locked,
        );
        let lines: Vec<String> = result
            .lines
            .iter()
            .map(|line| format!("{}{} {:?}", line.package, line.specifier, line.via))
            .collect();
        assert_eq!(
            lines,
            [
                "python-stdnum==1.19 [\"group oca\", \"l10n_nl_x\"]",
                "requests<3,>=2.30 [\"pos_x\", \"pos_y\"]",
                "unknown [\"pos_x\"]"
            ]
        );
        assert_eq!(result.omitted, ["babel", "lxml"]);
        assert_eq!(result.unresolved, ["requests", "unknown"]);
        assert_eq!(
            result.conflicts,
            [Conflict {
                package: "babel".to_owned(),
                specifier: ">=2.11".to_owned(),
                by: "pos_x".to_owned(),
                provided: vec!["2.10.3".to_owned()],
            }]
        );

        let text = render(&result, "vendored-source, provided: odoo-standard-17.0");
        assert!(
            text.starts_with("# Generated by `odx vendor --requirements`"),
            "{text}"
        );
        assert!(
            text.ends_with("python-stdnum==1.19  # via group oca, l10n_nl_x\nrequests<3,>=2.30  # via pos_x, pos_y\nunknown  # via pos_x\n"),
            "{text}"
        );
        assert!(!edited(&text));
        assert!(edited(&text.replace("1.19", "1.20")));
        assert!(edited("requests==1\n"));
        assert!(!edited(&render(&PlatformRequirements::default(), "x")));
    }
}
