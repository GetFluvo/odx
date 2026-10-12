//! `.odx/mutes.toml`: project decisions about upstream suggestions.
//!
//! ```toml
//! [[mute]]
//! match = "OCA/pos#789"
//! patch_ids = ["<patch-id>"]
//! kind = "not-wanted"
//! reason = "We do not use the loyalty program"
//! expires = "2027-01-01"
//! recheck = "never"
//! environments = ["test"]
//! muted_by = "developer@example.com"
//! muted_at = "2026-10-12"
//! ```
//!
//! Every mute needs a kind and a free-text reason. A `deferred` mute needs
//! an expiry date or a re-check trigger.

use serde::Deserialize;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::Problem;

/// Why a suggestion is declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MuteKind {
    /// The change does not apply to the target series.
    NotApplicable,
    /// The change is not needed on the target series.
    NotNeeded,
    /// Superseded by another change.
    Superseded,
    /// Already present on the target, possibly by another route.
    AlreadyPresent,
    /// Declined for this project.
    Declined,
    /// Not wanted in this project.
    NotWanted,
    /// Review later.
    Deferred,
}

impl MuteKind {
    /// The kind as written in the file.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not-applicable",
            Self::NotNeeded => "not-needed",
            Self::Superseded => "superseded",
            Self::AlreadyPresent => "already-present",
            Self::Declined => "declined",
            Self::NotWanted => "not-wanted",
            Self::Deferred => "deferred",
        }
    }
}

/// When a mute is reconsidered besides its expiry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recheck {
    /// When the touched code on the target series changes.
    TargetCodeChange,
    /// Never.
    Never,
}

impl Recheck {
    /// The value as written in the file.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TargetCodeChange => "target-code-change",
            Self::Never => "never",
        }
    }
}

/// One `[[mute]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mute {
    /// `OCA/pos#789` for a pull request, `OCA/pos@<sha>` for a commit.
    #[serde(rename = "match")]
    pub matches: String,
    /// Patch ids recorded so the mute survives renumbering.
    #[serde(default)]
    pub patch_ids: Vec<String>,
    /// Kind of decision.
    pub kind: MuteKind,
    /// Why; mandatory.
    pub reason: String,
    /// Expiry date (`YYYY-MM-DD`).
    pub expires: Option<String>,
    /// Re-check trigger.
    pub recheck: Option<Recheck>,
    /// Environments the mute is restricted to; empty means all.
    #[serde(default)]
    pub environments: Vec<String>,
    /// Who muted.
    pub muted_by: Option<String>,
    /// When (`YYYY-MM-DD`).
    pub muted_at: Option<String>,
}

/// The contents of a mutes file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutesFile {
    /// Mutes in file order.
    #[serde(default, rename = "mute")]
    pub mutes: Vec<Mute>,
}

/// Errors from reading a mutes file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MutesError {
    /// The file is not valid TOML or does not match the format.
    #[error("{0}")]
    Format(String),
    /// Entries are incomplete or inconsistent.
    #[error("invalid mutes:\n{}", .0.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Invalid(Vec<Problem>),
}

/// Whether `text` is a `YYYY-MM-DD` date with a plausible month and day.
fn is_date(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    let number = |part: &str, len: usize| {
        (part.len() == len && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u32>().ok())
            .flatten()
    };
    match parts.as_slice() {
        [year, month, day] => {
            number(year, 4).is_some()
                && number(month, 2).is_some_and(|m| (1..=12).contains(&m))
                && number(day, 2).is_some_and(|d| (1..=31).contains(&d))
        }
        _ => false,
    }
}

impl MutesFile {
    /// Parse and validate a mutes file.
    ///
    /// # Errors
    ///
    /// [`MutesError::Format`] for malformed TOML and unknown keys,
    /// [`MutesError::Invalid`] listing every mute without a reason, with a
    /// malformed date, or a `deferred` mute with neither expiry nor recheck.
    pub fn parse(text: &str) -> Result<Self, MutesError> {
        let file: Self = toml::from_str(text).map_err(|e| MutesError::Format(e.to_string()))?;
        let mut problems = Vec::new();
        for (i, mute) in file.mutes.iter().enumerate() {
            let mut problem = |message: &str| {
                problems.push(Problem {
                    key: format!("mute[{i}] ({})", mute.matches),
                    message: message.to_owned(),
                });
            };
            if mute.reason.trim().is_empty() {
                problem("`reason` is mandatory");
            }
            for date in [&mute.expires, &mute.muted_at].into_iter().flatten() {
                if !is_date(date) {
                    problem("dates must be written as YYYY-MM-DD");
                }
            }
            if mute.kind == MuteKind::Deferred && mute.expires.is_none() && mute.recheck.is_none() {
                problem("a deferred mute needs `expires` or `recheck`");
            }
        }
        if problems.is_empty() {
            Ok(file)
        } else {
            Err(MutesError::Invalid(problems))
        }
    }

    /// Render the file in its canonical layout.
    pub fn render(&self) -> String {
        let mut doc = DocumentMut::new();
        if !self.mutes.is_empty() {
            let mut tables = ArrayOfTables::new();
            for mute in &self.mutes {
                tables.push(mute_table(mute));
            }
            doc.insert("mute", Item::ArrayOfTables(tables));
        }
        doc.to_string()
    }
}

fn mute_table(mute: &Mute) -> Table {
    let mut table = Table::new();
    let mut set = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            table.insert(key, Item::Value(value));
        }
    };
    let list = |values: &[String]| {
        (!values.is_empty())
            .then(|| Value::Array(values.iter().map(String::as_str).collect::<Array>()))
    };
    set("match", Some(Value::from(mute.matches.as_str())));
    set("patch_ids", list(&mute.patch_ids));
    set("kind", Some(Value::from(mute.kind.as_str())));
    set("reason", Some(Value::from(mute.reason.as_str())));
    set("expires", mute.expires.as_deref().map(Value::from));
    set("recheck", mute.recheck.map(|r| Value::from(r.as_str())));
    set("environments", list(&mute.environments));
    set("muted_by", mute.muted_by.as_deref().map(Value::from));
    set("muted_at", mute.muted_at.as_deref().map(Value::from));
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const SAMPLE: &str = r#"[[mute]]
match = "OCA/pos#789"
patch_ids = ["a1b2"]
kind = "not-wanted"
reason = "We do not use the loyalty program"
recheck = "never"
environments = ["test"]
muted_by = "developer@example.com"
muted_at = "2026-10-12"

[[mute]]
match = "OCA/contract@abc1234"
kind = "deferred"
reason = "Review later"
expires = "2027-01-01"
"#;

    #[test]
    fn parses_and_renders_canonically() {
        let file = MutesFile::parse(SAMPLE).unwrap();
        assert_eq!(file.mutes.len(), 2);
        assert_eq!(file.mutes[0].kind, MuteKind::NotWanted);
        assert_eq!(file.render(), SAMPLE);
        assert_eq!(MutesFile::parse("").unwrap(), MutesFile::default());
        assert_eq!(MutesFile::default().render(), "");
    }

    #[test]
    fn every_kind_and_recheck_round_trips() {
        for kind in [
            MuteKind::NotApplicable,
            MuteKind::NotNeeded,
            MuteKind::Superseded,
            MuteKind::AlreadyPresent,
            MuteKind::Declined,
            MuteKind::NotWanted,
            MuteKind::Deferred,
        ] {
            let text = format!(
                "[[mute]]\nmatch = \"x\"\nkind = \"{}\"\nreason = \"r\"\nrecheck = \"target-code-change\"\n",
                kind.as_str()
            );
            let file = MutesFile::parse(&text).unwrap();
            assert_eq!(file.mutes[0].kind, kind);
            assert_eq!(file.mutes[0].recheck, Some(Recheck::TargetCodeChange));
            assert_eq!(file.render(), text);
        }
    }

    #[test]
    fn validation_reports_every_problem() {
        let text = "[[mute]]\nmatch = \"a\"\nkind = \"deferred\"\nreason = \" \"\nmuted_at = \"12-10-2026\"\n\n[[mute]]\nmatch = \"b\"\nkind = \"declined\"\nreason = \"r\"\nexpires = \"2027-13-01\"\n";
        let err = MutesFile::parse(text).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid mutes:\n  - mute[0] (a): `reason` is mandatory\n  - mute[0] (a): dates must be written as YYYY-MM-DD\n  - mute[0] (a): a deferred mute needs `expires` or `recheck`\n  - mute[1] (b): dates must be written as YYYY-MM-DD"
        );
        let format = MutesFile::parse("[[mute]]\nmatch = 1\n").unwrap_err();
        assert_eq!(format, MutesError::Format(format.to_string()));
    }

    #[test]
    fn dates() {
        for good in ["2026-10-12", "2027-01-31"] {
            assert!(is_date(good), "{good}");
        }
        for bad in [
            "2026-00-10",
            "2026-10-32",
            "2026-1-10",
            "26-10-12",
            "2026-10",
            "abcd-10-12",
            "2026-10-00",
        ] {
            assert!(!is_date(bad), "{bad}");
        }
    }

    fn text() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9 ._/:@\"'\\\\#=-]{0,24}"
    }

    fn date() -> impl Strategy<Value = String> {
        (2000u32..2100, 1u32..=12, 1u32..=28).prop_map(|(y, m, d)| format!("{y:04}-{m:02}-{d:02}"))
    }

    fn mute() -> impl Strategy<Value = Mute> {
        (
            text(),
            proptest::collection::vec(text(), 0..3),
            prop_oneof![
                Just(MuteKind::NotApplicable),
                Just(MuteKind::Superseded),
                Just(MuteKind::NotWanted)
            ],
            "[a-z]{1,12}",
            proptest::option::of(date()),
            proptest::option::of(prop_oneof![
                Just(Recheck::Never),
                Just(Recheck::TargetCodeChange)
            ]),
            proptest::collection::vec(text(), 0..3),
            proptest::option::of(text()),
            proptest::option::of(date()),
        )
            .prop_map(
                |(
                    matches,
                    patch_ids,
                    kind,
                    reason,
                    expires,
                    recheck,
                    environments,
                    muted_by,
                    muted_at,
                )| Mute {
                    matches,
                    patch_ids,
                    kind,
                    reason,
                    expires,
                    recheck,
                    environments,
                    muted_by,
                    muted_at,
                },
            )
    }

    proptest! {
        #[test]
        fn render_is_a_fixed_point(mutes in proptest::collection::vec(mute(), 0..4)) {
            let file = MutesFile { mutes };
            let rendered = file.render();
            let parsed = MutesFile::parse(&rendered).unwrap();
            prop_assert_eq!(&parsed, &file);
            prop_assert_eq!(parsed.render(), rendered);
        }
    }
}
