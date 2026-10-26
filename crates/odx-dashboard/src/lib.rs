//! Dashboard model and renderers for odx: one model for the terminal table and the bot's Markdown issue.
//!
//! Part of [odx](https://github.com/getfluvo/odx). A [`Dashboard`] is a list
//! of [`Section`]s of [`Entry`]s, each with a stable, printable id
//! (`pr:OCA/pos#789`, `fwd:OCA/pos#1234->17.0`, `pkg:odoo-addon-x@17.0.1.2.4`)
//! and the [`Action`]s a person may take on it. [`table`] and [`markdown`]
//! render the same model; [`ticked`] reads the boxes ticked in the Markdown
//! back as `(id, action)` pairs. The model never calls a forge.

#[cfg(feature = "tui")]
pub mod tui;

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a section shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SectionKind {
    /// Pull requests the project aggregates or pins, and their state.
    Pinned,
    /// Overrides whose pull request is merged upstream.
    Droppable,
    /// Open pull requests upstream that touch addons the project uses.
    NewUpstream,
    /// Changes from lower series the project's series lacks.
    ForwardPort,
    /// Changes from higher series.
    Backport,
    /// Index patch releases the auto-update policy would take.
    AutoUpdate,
    /// Glue addons Odoo would install with what the project has, that it
    /// does not have.
    Glue,
    /// Addons the project may need later, once they are there or being
    /// migrated.
    Wanted,
    /// What differs between environments.
    Drift,
    /// What verdicts, mutes and blacklists hide.
    Suppressed,
}

impl SectionKind {
    /// Every section, in display order.
    pub const ALL: [Self; 10] = [
        Self::Pinned,
        Self::Droppable,
        Self::NewUpstream,
        Self::ForwardPort,
        Self::Backport,
        Self::AutoUpdate,
        Self::Glue,
        Self::Wanted,
        Self::Drift,
        Self::Suppressed,
    ];

    /// How the section is named on the command line and in the config.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Droppable => "droppable",
            Self::NewUpstream => "new-upstream",
            Self::ForwardPort => "forward-port",
            Self::Backport => "backport",
            Self::AutoUpdate => "auto-update",
            Self::Glue => "glue",
            Self::Wanted => "wanted",
            Self::Drift => "drift",
            Self::Suppressed => "suppressed",
        }
    }

    /// The section's heading.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Pinned => "Pinned pull requests",
            Self::Droppable => "Overrides that can be dropped",
            Self::NewUpstream => "New upstream pull requests",
            Self::ForwardPort => "Forward-port suggestions",
            Self::Backport => "Backport candidates",
            Self::AutoUpdate => "Index updates",
            Self::Glue => "Glue addons the project misses",
            Self::Wanted => "Wanted addons",
            Self::Drift => "Environment drift",
            Self::Suppressed => "Suppressed",
        }
    }

    /// The section named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

/// Something a person can do about an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Add the pull request to the project.
    Include,
    /// Port the change upstream (always a person's decision).
    Port,
    /// Record a mute or verdict.
    Mute,
    /// Remove the override from the project.
    Drop,
    /// Take the update.
    Follow,
    /// Promote the addon into the next environment.
    Promote,
}

impl Action {
    /// Every action.
    pub const ALL: [Self; 6] = [
        Self::Include,
        Self::Port,
        Self::Mute,
        Self::Drop,
        Self::Follow,
        Self::Promote,
    ];

    /// How the action is written.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Port => "port",
            Self::Mute => "mute",
            Self::Drop => "drop",
            Self::Follow => "follow",
            Self::Promote => "promote",
        }
    }

    /// The action named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// One line of a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    /// Stable id, the same in every renderer.
    pub id: String,
    /// The addon it concerns, if one.
    pub addon: Option<String>,
    /// What it is about, for people.
    pub subject: String,
    /// Where it stands.
    pub state: String,
    /// What can be done.
    pub actions: Vec<Action>,
    /// More to know.
    pub notes: Vec<String>,
}

/// One section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Section {
    /// What it shows.
    pub kind: SectionKind,
    /// Seconds since the epoch its data was read.
    pub fetched_at: u64,
    /// Its entries.
    pub entries: Vec<Entry>,
}

/// The dashboard of one series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dashboard {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    pub schema: u32,
    /// The series.
    pub series: String,
    /// Seconds since the epoch it was built.
    pub generated_at: u64,
    /// Its sections, in display order.
    pub sections: Vec<Section>,
}

impl Dashboard {
    /// The entry with `id`, in whichever section.
    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.sections
            .iter()
            .flat_map(|section| &section.entries)
            .find(|entry| entry.id == id)
    }
}

/// `text` padded to `width` characters.
fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(len)))
}

/// The dashboard as terminal tables, one per section with entries.
#[must_use]
pub fn table(dashboard: &Dashboard) -> String {
    let mut out = format!("Dashboard for {}\n", dashboard.series);
    for section in dashboard.sections.iter().filter(|s| !s.entries.is_empty()) {
        let _ = write!(
            out,
            "\n{} ({})\n",
            section.kind.title(),
            section.entries.len()
        );
        let rows: Vec<[String; 4]> = section
            .entries
            .iter()
            .map(|entry| {
                let actions: Vec<&str> = entry.actions.iter().map(|a| a.name()).collect();
                [
                    entry.id.clone(),
                    entry.state.clone(),
                    entry.subject.clone(),
                    actions.join(","),
                ]
            })
            .collect();
        let widths: Vec<usize> = (0..3)
            .map(|column| {
                rows.iter()
                    .map(|row| row.get(column).map_or(0, |cell| cell.chars().count()))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for (row, entry) in rows.iter().zip(&section.entries) {
            let cells: Vec<String> = row
                .iter()
                .zip(widths.iter().chain(std::iter::repeat(&0)))
                .map(|(cell, width)| pad(cell, *width))
                .collect();
            let _ = writeln!(out, "  {}", cells.join("  ").trim_end());
            for note in &entry.notes {
                let _ = writeln!(out, "    {note}");
            }
        }
    }
    if dashboard.sections.iter().all(|s| s.entries.is_empty()) {
        out.push_str("\nNothing to report.\n");
    }
    out
}

/// The dashboard as Markdown; with `checkboxes`, every action of every
/// entry is a box `- [ ] <!-- odx:<action> <id> -->` that [`ticked`] reads.
#[must_use]
pub fn markdown(dashboard: &Dashboard, checkboxes: bool) -> String {
    format!(
        "# odx dashboard for {}\n{}",
        dashboard.series,
        sections(dashboard, &|_, _| if checkboxes {
            ActionLine::Checkbox
        } else {
            ActionLine::Hidden
        })
    )
}

/// How [`sections`] writes one action of an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionLine {
    /// A box `- [ ] <action> <!-- odx:<action> <id> -->` that [`ticked`] reads.
    Checkbox,
    /// The same box, ticked.
    Ticked,
    /// A plain line `- <action>: <text>`, nothing to tick.
    Note(String),
    /// Nothing.
    Hidden,
}

/// The sections of the dashboard as Markdown, each action of an entry
/// written as `line` says; the heading is left to the caller.
#[must_use]
pub fn sections(dashboard: &Dashboard, line: &dyn Fn(&Entry, Action) -> ActionLine) -> String {
    let mut out = String::new();
    for section in dashboard.sections.iter().filter(|s| !s.entries.is_empty()) {
        let _ = write!(
            out,
            "\n## {} ({})\n\n",
            section.kind.title(),
            section.entries.len()
        );
        for entry in &section.entries {
            let _ = writeln!(out, "- `{}` {}: {}", entry.id, entry.state, entry.subject);
            for note in &entry.notes {
                let _ = writeln!(out, "  - {note}");
            }
            for action in &entry.actions {
                let name = action.name();
                match line(entry, *action) {
                    ActionLine::Checkbox => {
                        let _ = writeln!(out, "  - [ ] {name} <!-- odx:{name} {} -->", entry.id);
                    }
                    ActionLine::Ticked => {
                        let _ = writeln!(out, "  - [x] {name} <!-- odx:{name} {} -->", entry.id);
                    }
                    ActionLine::Note(text) => {
                        let _ = writeln!(out, "  - {name}: {text}");
                    }
                    ActionLine::Hidden => {}
                }
            }
        }
    }
    out
}

/// The `(id, action)` pairs of the ticked boxes in `text` (Markdown that
/// [`markdown`] wrote, edited by a person).
#[must_use]
pub fn ticked(text: &str) -> Vec<(String, Action)> {
    text.lines()
        .map(str::trim_start)
        .filter_map(|line| {
            let rest = line
                .strip_prefix("- [x]")
                .or_else(|| line.strip_prefix("- [X]"))?;
            let marker = rest.split_once("<!-- odx:")?.1.split_once("-->")?.0;
            let (action, id) = marker.trim().split_once(' ')?;
            Some((id.trim().to_owned(), Action::parse(action)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dashboard() -> Dashboard {
        Dashboard {
            schema: 1,
            series: "17.0".to_owned(),
            generated_at: 1,
            sections: vec![
                Section {
                    kind: SectionKind::Pinned,
                    fetched_at: 1,
                    entries: vec![Entry {
                        id: "pr:OCA/pos#789".to_owned(),
                        addon: None,
                        subject: "[17.0][FIX] pos_x: rounding".to_owned(),
                        state: "open, head same".to_owned(),
                        actions: vec![Action::Drop],
                        notes: vec!["approved".to_owned()],
                    }],
                },
                Section {
                    kind: SectionKind::ForwardPort,
                    fetched_at: 1,
                    entries: vec![Entry {
                        id: "fwd:OCA/pos#1234->17.0".to_owned(),
                        addon: Some("pos_x".to_owned()),
                        subject: "[FIX] pos_x: totals".to_owned(),
                        state: "from 16.0".to_owned(),
                        actions: vec![Action::Port, Action::Mute],
                        notes: Vec::new(),
                    }],
                },
                Section {
                    kind: SectionKind::Backport,
                    fetched_at: 1,
                    entries: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn names_round_trip() {
        for kind in SectionKind::ALL {
            assert_eq!(SectionKind::parse(kind.name()), Some(kind));
            assert!(kind.title().chars().next().is_some_and(char::is_uppercase));
        }
        for action in Action::ALL {
            assert_eq!(Action::parse(action.name()), Some(action));
        }
        assert_eq!(SectionKind::parse("drift"), Some(SectionKind::Drift));
        assert_eq!(SectionKind::parse("nope"), None);
        assert_eq!(Action::parse("merge"), None);
    }

    #[test]
    fn table_rendering() {
        assert_eq!(
            table(&dashboard()),
            "Dashboard for 17.0\n\
             \nPinned pull requests (1)\n  pr:OCA/pos#789  open, head same  [17.0][FIX] pos_x: rounding  drop\n    approved\n\
             \nForward-port suggestions (1)\n  fwd:OCA/pos#1234->17.0  from 16.0  [FIX] pos_x: totals  port,mute\n"
        );
        let empty = Dashboard {
            sections: Vec::new(),
            ..dashboard()
        };
        assert_eq!(table(&empty), "Dashboard for 17.0\n\nNothing to report.\n");
        assert_eq!(
            dashboard().entry("pr:OCA/pos#789").unwrap().state,
            "open, head same"
        );
        assert_eq!(dashboard().entry("nope"), None);
    }

    #[test]
    fn the_model_round_trips_through_json() {
        let model = vec![dashboard()];
        let text = serde_json::to_string(&model).unwrap();
        let back: Vec<Dashboard> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, model);
    }

    #[test]
    fn markdown_round_trip() {
        let model = dashboard();
        let plain = markdown(&model, false);
        assert!(!plain.contains("<!--"), "{plain}");
        let text = markdown(&model, true);
        assert_eq!(
            text,
            "# odx dashboard for 17.0\n\
             \n## Pinned pull requests (1)\n\n- `pr:OCA/pos#789` open, head same: [17.0][FIX] pos_x: rounding\n  - approved\n  - [ ] drop <!-- odx:drop pr:OCA/pos#789 -->\n\
             \n## Forward-port suggestions (1)\n\n- `fwd:OCA/pos#1234->17.0` from 16.0: [FIX] pos_x: totals\n  - [ ] port <!-- odx:port fwd:OCA/pos#1234->17.0 -->\n  - [ ] mute <!-- odx:mute fwd:OCA/pos#1234->17.0 -->\n"
        );
        assert_eq!(ticked(&text), Vec::new());
        let edited = text
            .replace("- [ ] mute", "- [x] mute")
            .replace("- [ ] drop", "- [X] drop")
            + "- [x] nothing here\n- [x] <!-- odx:merge x -->\n- [x] <!-- odx:mute -->\n- [x] <!-- odx:mute x\n";
        let picked = ticked(&edited);
        assert_eq!(
            picked,
            [
                ("pr:OCA/pos#789".to_owned(), Action::Drop),
                ("fwd:OCA/pos#1234->17.0".to_owned(), Action::Mute),
            ]
        );
        for (id, action) in picked {
            assert!(model.entry(&id).unwrap().actions.contains(&action));
        }
    }

    #[test]
    fn sections_with_notes_instead_of_boxes() {
        let text = sections(&dashboard(), &|entry, action| match action {
            Action::Port => ActionLine::Note(format!("`odx dashboard act {} --port`", entry.id)),
            Action::Mute => ActionLine::Hidden,
            _ => ActionLine::Ticked,
        });
        assert!(
            text.starts_with("\n## Pinned pull requests (1)\n"),
            "{text}"
        );
        assert!(text.contains("  - [x] drop <!-- odx:drop pr:OCA/pos#789 -->\n"));
        assert!(text.contains("  - port: `odx dashboard act fwd:OCA/pos#1234->17.0 --port`\n"));
        assert!(!text.contains("mute"), "{text}");
        assert_eq!(ticked(&text), [("pr:OCA/pos#789".to_owned(), Action::Drop)]);
    }
}
