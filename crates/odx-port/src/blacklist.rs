//! oca-port blacklists: `.oca/oca-port/blacklist/<addon>.json` on a target
//! branch, read and written byte-compatibly with oca-port.

use std::fmt::{self, Write as _};

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

/// The key that blacklists every orphaned commit of an addon.
pub const ORPHANED: &str = "orphaned_commits";

/// Where an addon's blacklist lives in a repository.
///
/// ```
/// assert_eq!(odx_port::blacklist::path("pos_x"), ".oca/oca-port/blacklist/pos_x.json");
/// ```
#[must_use]
pub fn path(addon: &str) -> String {
    format!(".oca/oca-port/blacklist/{addon}.json")
}

/// The entries of one addon's blacklist, in file order: `<owner>/<repo>#<n>`
/// (or [`ORPHANED`]) and the reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blacklist {
    /// Entries in order.
    pub entries: Vec<(String, String)>,
}

/// A blacklist file: its `pull_requests` in file order (empty when it is
/// no object); other keys are ignored.
struct File(Vec<(String, serde_json::Value)>);

impl<'de> Deserialize<'de> for File {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Top;
        impl<'de> Visitor<'de> for Top {
            type Value = File;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<File, A::Error> {
                let mut entries = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "pull_requests" {
                        entries = map.next_value::<Maybe>()?.0;
                    } else {
                        map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                Ok(File(entries))
            }
        }
        deserializer.deserialize_map(Top)
    }
}

/// An object in file order, or nothing when the value is something else.
struct Maybe(Vec<(String, serde_json::Value)>);

impl<'de> Deserialize<'de> for Maybe {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Any;
        impl<'de> Visitor<'de> for Any {
            type Value = Maybe;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Maybe, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Maybe(entries))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Maybe, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(Maybe(Vec::new()))
            }
        }
        deserializer.deserialize_any(Any)
    }
}

/// A string as Python's `json.dumps(..., ensure_ascii=False)` writes it.
fn quoted(text: &str) -> String {
    let mut out = String::from('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Blacklist {
    /// Read a blacklist file. Values that are not strings are skipped;
    /// other top-level keys are ignored.
    ///
    /// # Errors
    ///
    /// When the text is not a JSON object.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = serde_json::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self {
            entries: file
                .0
                .into_iter()
                .filter_map(|(key, value)| value.as_str().map(|reason| (key, reason.to_owned())))
                .collect(),
        })
    }

    /// The reason `key` is blacklisted, if it is.
    #[must_use]
    pub fn reason(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(entry, _)| entry == key)
            .map(|(_, reason)| reason.as_str())
    }

    /// Add `key` with `reason` at the end, or replace the reason of an
    /// existing entry in place.
    ///
    /// # Errors
    ///
    /// When `reason` is empty: every entry needs one.
    pub fn add(&mut self, key: &str, reason: &str) -> Result<(), String> {
        if reason.trim().is_empty() {
            return Err(format!("{key}: a blacklist entry needs a reason"));
        }
        match self.entries.iter_mut().find(|(entry, _)| entry == key) {
            Some(entry) => reason.clone_into(&mut entry.1),
            None => self.entries.push((key.to_owned(), reason.to_owned())),
        }
        Ok(())
    }

    /// The file as oca-port writes it: two-space indent, entries in order,
    /// non-ASCII kept, one trailing newline.
    #[must_use]
    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return "{\n  \"pull_requests\": {}\n}\n".to_owned();
        }
        let lines: Vec<String> = self
            .entries
            .iter()
            .map(|(key, reason)| format!("    {}: {}", quoted(key), quoted(reason)))
            .collect();
        format!(
            "{{\n  \"pull_requests\": {{\n{}\n  }}\n}}\n",
            lines.join(",\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "{\n  \"pull_requests\": {\n    \"OCA/contract#1301\": \"superseded: 19.0 sends through account.move.send\",\n    \"OCA/contract#1201\": \"repo-wide \\\"test\\\" change; Ünïcode\",\n    \"orphaned_commits\": \"[MIG] subscription: Migration to 18.0\"\n  }\n}\n";

    #[test]
    fn files_round_trip_byte_for_byte() {
        let blacklist = Blacklist::parse(FILE).unwrap();
        assert_eq!(blacklist.entries.len(), 3);
        assert_eq!(blacklist.entries[0].0, "OCA/contract#1301");
        assert_eq!(blacklist.render(), FILE);
        assert_eq!(
            blacklist.reason(ORPHANED),
            Some("[MIG] subscription: Migration to 18.0")
        );
        assert_eq!(blacklist.reason("OCA/x#1"), None);
    }

    #[test]
    fn entries_are_added_in_order_and_need_a_reason() {
        let mut blacklist = Blacklist::default();
        assert_eq!(blacklist.render(), "{\n  \"pull_requests\": {}\n}\n");
        blacklist.add("OCA/x#2", "b").unwrap();
        blacklist
            .add("OCA/x#1", "a\tb\u{1}\\\n\r\u{8}\u{c}")
            .unwrap();
        blacklist.add("OCA/x#2", "c").unwrap();
        assert_eq!(
            blacklist.render(),
            "{\n  \"pull_requests\": {\n    \"OCA/x#2\": \"c\",\n    \"OCA/x#1\": \"a\\tb\\u0001\\\\\\n\\r\\b\\f\"\n  }\n}\n"
        );
        assert_eq!(
            blacklist.add("OCA/x#3", " ").unwrap_err(),
            "OCA/x#3: a blacklist entry needs a reason"
        );
        // Other keys and values are tolerated on reading.
        let odd = Blacklist::parse(r#"{"other": 1, "pull_requests": {"a": 1, "b": "x"}}"#).unwrap();
        assert_eq!(odd.entries, [("b".to_owned(), "x".to_owned())]);
        assert_eq!(
            Blacklist::parse(r#"{"pull_requests": []}"#)
                .unwrap()
                .entries,
            Vec::new()
        );
        assert!(Blacklist::parse("[]").is_err());
        for broken in [
            "{,}",
            r#"{"pull_requests": {"a": }}"#,
            r#"{"x": [}"#,
            r#"{"pull_requests": [1,}"#,
            r#"{"pull_requests": "x"}"#,
        ] {
            assert!(Blacklist::parse(broken).is_err(), "{broken}");
        }
    }
}
