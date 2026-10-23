//! The bot's pull requests: kinds, the marker every body starts with,
//! branch names, and finding the pull request a run opened before.

use odx_dashboard::Action;
use odx_forge::{Issue, IssueState};

/// The label every bot pull request carries, besides its kind's.
pub const LABEL: &str = "odx:bot";

/// What a bot pull request does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Drop overrides merged and released upstream (one per run).
    DropOverride,
    /// Re-pin pull requests whose new head is green (one per run).
    Repin,
    /// Index releases under the auto-update policy (one per run).
    AutoUpdate,
    /// Add a merge or package asked for on the dashboard.
    Add,
    /// Drop a merge or package asked for on the dashboard.
    Drop,
    /// Promote an addon closure to the next environment.
    Promote,
    /// Record a mute asked for on the dashboard, when mutes live in the
    /// project (no verdict repository).
    Mute,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Self; 7] = [
        Self::DropOverride,
        Self::Repin,
        Self::AutoUpdate,
        Self::Add,
        Self::Drop,
        Self::Promote,
        Self::Mute,
    ];

    /// The kind's name, as in markers, labels and branches.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DropOverride => "drop-override",
            Self::Repin => "repin",
            Self::AutoUpdate => "auto-update",
            Self::Add => "add",
            Self::Drop => "drop",
            Self::Promote => "promote",
            Self::Mute => "mute",
        }
    }

    /// The kind named `name`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The label its pull requests carry, `odx:<name>`.
    #[must_use]
    pub fn label(self) -> String {
        format!("odx:{}", self.name())
    }

    /// Whether a run opens one pull request of this kind for everything
    /// it found, rather than one per dashboard entry.
    #[must_use]
    pub const fn grouped(self) -> bool {
        matches!(self, Self::DropOverride | Self::Repin | Self::AutoUpdate)
    }

    /// The kind of pull request a ticked dashboard box asks for; `None`
    /// for porting upstream, which the bot never does.
    #[must_use]
    pub const fn of_action(action: Action) -> Option<Self> {
        match action {
            Action::Include => Some(Self::Add),
            Action::Drop => Some(Self::Drop),
            Action::Follow => Some(Self::Repin),
            Action::Promote => Some(Self::Promote),
            Action::Mute => Some(Self::Mute),
            Action::Port => None,
        }
    }
}

/// The marker on the first line of a bot pull request's body:
/// `<!-- odx:pr kind=<kind> key=<key> [entry=<action>:<id>] -->`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// What it does.
    pub kind: Kind,
    /// Stable for the same proposed change.
    pub key: String,
    /// The dashboard box that asked for it.
    pub entry: Option<(Action, String)>,
}

impl Marker {
    /// The marker for the pull request a ticked box asks for, keyed by
    /// the entry; `None` for porting upstream.
    #[must_use]
    pub fn of_entry(action: Action, id: &str) -> Option<Self> {
        Kind::of_action(action).map(|kind| Self {
            kind,
            key: slug(id),
            entry: Some((action, id.to_owned())),
        })
    }

    /// The marker for a grouped pull request, keyed by the change.
    #[must_use]
    pub fn of_change(kind: Kind, change: &str) -> Self {
        Self {
            kind,
            key: digest(change),
            entry: None,
        }
    }

    /// The marker for the promotion into `to` of what pull request
    /// `number` merged (it carries the promotion label).
    #[must_use]
    pub fn of_merged(number: u64, to: &str) -> Self {
        Self {
            kind: Kind::Promote,
            key: format!("pr-{number}-{}", slug(to)),
            entry: None,
        }
    }

    /// The marker as an HTML comment.
    #[must_use]
    pub fn render(&self) -> String {
        let entry = self
            .entry
            .as_ref()
            .map(|(action, id)| format!(" entry={}:{id}", action.name()))
            .unwrap_or_default();
        format!(
            "<!-- odx:pr kind={} key={}{entry} -->",
            self.kind.name(),
            self.key
        )
    }

    /// The marker on the first line of `body`, if it carries one.
    #[must_use]
    pub fn parse(body: &str) -> Option<Self> {
        let inner = body
            .lines()
            .next()?
            .trim()
            .strip_prefix("<!-- odx:pr ")?
            .strip_suffix("-->")?;
        let mut kind = None;
        let mut key = None;
        let mut entry = None;
        for field in inner.split_whitespace() {
            match field.split_once('=') {
                Some(("kind", value)) => kind = Kind::parse(value),
                Some(("key", value)) => key = Some(value.to_owned()),
                Some(("entry", value)) => {
                    entry = value
                        .split_once(':')
                        .and_then(|(action, id)| Some((Action::parse(action)?, id.to_owned())));
                }
                _ => {}
            }
        }
        Some(Self {
            kind: kind?,
            key: key?,
            entry,
        })
    }

    /// The branch it is pushed to, `odx/<kind>/<key>`.
    #[must_use]
    pub fn branch(&self) -> String {
        format!("odx/{}/{}", self.kind.name(), self.key)
    }
}

/// The line a grouped pull request's body carries for each dashboard
/// entry it takes care of.
#[must_use]
pub fn covering(id: &str) -> String {
    format!("<!-- odx:covers {id} -->")
}

/// Whether `pull` takes care of the dashboard entry `id`.
#[must_use]
pub fn covers(pull: &Issue, id: &str) -> bool {
    pull.body.lines().any(|line| line.ends_with(&covering(id)))
}

/// `text` as a branch-safe key: lowercase ASCII letters, digits and dots,
/// other runs of characters as one `-`.
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// A short, stable digest of `text` (FNV-1a, 12 hex digits).
#[must_use]
pub fn digest(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:012x}", hash & 0xffff_ffff_ffff)
}

/// What an earlier run left for `marker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Earlier<'a> {
    /// An open pull request: update it in place.
    Open(&'a Issue),
    /// The same change, closed by a person: leave it closed.
    Closed(&'a Issue),
    /// Nothing: open one.
    None,
}

/// What `pulls` (the bot's pull requests, open and closed) hold for
/// `marker`.
///
/// An open pull request of a grouped kind is taken whatever its
/// key, since a run has one of each; one for a dashboard box is matched by
/// its entry, any other by its key. A closed one only counts with the same
/// key, and never for a box (ticking it again asks anew): a different
/// change deserves a new pull request.
#[must_use]
pub fn earlier<'a>(pulls: &'a [Issue], marker: &Marker) -> Earlier<'a> {
    let candidates: Vec<(&Issue, Marker)> = pulls
        .iter()
        .filter_map(|pull| Marker::parse(&pull.body).map(|found| (pull, found)))
        .filter(|(_, found)| found.kind == marker.kind)
        .collect();
    let open = candidates.iter().find(|(pull, found)| {
        pull.state == IssueState::Open
            && (marker.kind.grouped()
                || (found.entry == marker.entry
                    && (marker.entry.is_some() || found.key == marker.key)))
    });
    if let Some((pull, _)) = open {
        return Earlier::Open(pull);
    }
    candidates
        .iter()
        .find(|(pull, found)| {
            pull.state == IssueState::Closed && found.key == marker.key && marker.entry.is_none()
        })
        .map_or(Earlier::None, |(pull, _)| Earlier::Closed(pull))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(number: u64, state: IssueState, body: &str) -> Issue {
        Issue {
            number,
            title: format!("#{number}"),
            body: body.to_owned(),
            state,
            labels: Vec::new(),
            url: format!("https://x/pull/{number}"),
            pull: true,
        }
    }

    #[test]
    fn kinds() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.name()), Some(kind));
            assert_eq!(kind.label(), format!("odx:{}", kind.name()));
        }
        assert_eq!(Kind::parse("port"), None);
        assert_eq!(
            Kind::ALL.map(Kind::grouped),
            [true, true, true, false, false, false, false]
        );
        assert_eq!(
            Action::ALL.map(Kind::of_action),
            [
                Some(Kind::Add),
                None,
                Some(Kind::Mute),
                Some(Kind::Drop),
                Some(Kind::Repin),
                Some(Kind::Promote)
            ]
        );
    }

    #[test]
    fn markers_round_trip() {
        let entry = Marker::of_entry(Action::Include, "pr:OCA/pos#789").unwrap();
        assert_eq!(
            entry.render(),
            "<!-- odx:pr kind=add key=pr-oca-pos-789 entry=include:pr:OCA/pos#789 -->"
        );
        assert_eq!(entry.branch(), "odx/add/pr-oca-pos-789");
        assert_eq!(
            Marker::parse(&format!("{}\nbody", entry.render())),
            Some(entry)
        );
        assert_eq!(Marker::of_entry(Action::Port, "x"), None);
        let grouped = Marker::of_change(Kind::DropOverride, "pos_x pos_y");
        assert_eq!(grouped.key.len(), 12);
        assert_eq!(
            grouped,
            Marker::of_change(Kind::DropOverride, "pos_x pos_y")
        );
        assert_ne!(
            grouped.key,
            Marker::of_change(Kind::DropOverride, "pos_x").key
        );
        assert_eq!(Marker::parse(&grouped.render()), Some(grouped));
        for body in [
            "",
            "plain",
            "<!-- odx:pr kind=add -->",
            "<!-- odx:pr key=k -->",
            "<!-- odx:pr kind=merge key=k -->",
            "<!-- odx:dashboard series=17.0 -->",
            "<!-- odx:pr kind=add key=k",
        ] {
            assert_eq!(Marker::parse(body), None, "{body}");
        }
        // Unknown fields and a broken entry are ignored.
        for broken in ["nope", "merge:x"] {
            assert_eq!(
                Marker::parse(&format!(
                    "<!-- odx:pr kind=drop key=k entry={broken} other -->"
                )),
                Some(Marker {
                    kind: Kind::Drop,
                    key: "k".to_owned(),
                    entry: None
                })
            );
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("pr:OCA/pos#789"), "pr-oca-pos-789");
        assert_eq!(
            slug("fwd:OCA/sale-workflow#1234->17.0"),
            "fwd-oca-sale-workflow-1234-17.0"
        );
        assert_eq!(slug("::a__b::"), "a-b");
        assert_eq!(slug(""), "");
    }

    #[test]
    fn finds_earlier_pull_requests() {
        let add = Marker::of_entry(Action::Include, "pr:OCA/pos#789").unwrap();
        let other = Marker::of_entry(Action::Include, "pr:OCA/pos#1").unwrap();
        let drops = Marker::of_change(Kind::DropOverride, "a");
        let pulls = [
            pull(1, IssueState::Open, "no marker"),
            pull(2, IssueState::Open, &other.render()),
            pull(3, IssueState::Closed, &add.render()),
            pull(4, IssueState::Open, &add.render()),
            pull(5, IssueState::Closed, &drops.render()),
        ];
        assert_eq!(earlier(&pulls, &add), Earlier::Open(&pulls[3]));
        // A closed pull request for a box does not block a new tick.
        assert_eq!(earlier(&pulls[..3], &add), Earlier::None);
        // Grouped: closed with the same change stays closed; a new change
        // gets a new one; an open one is updated whatever its key.
        assert_eq!(earlier(&pulls, &drops), Earlier::Closed(&pulls[4]));
        let changed = Marker::of_change(Kind::DropOverride, "b");
        assert_eq!(earlier(&pulls, &changed), Earlier::None);
        let open = [pull(6, IssueState::Open, &drops.render())];
        assert_eq!(earlier(&open, &changed), Earlier::Open(&open[0]));
        // Promotions of a merged pull request: by key, open or closed.
        let promoted = Marker::of_merged(7, "prod");
        assert_eq!(promoted.branch(), "odx/promote/pr-7-prod");
        let pulls = [
            pull(8, IssueState::Open, &Marker::of_merged(6, "prod").render()),
            pull(9, IssueState::Closed, &promoted.render()),
        ];
        assert_eq!(earlier(&pulls, &promoted), Earlier::Closed(&pulls[1]));
        assert_eq!(earlier(&pulls[..1], &promoted), Earlier::None);
        let reopened = [pull(10, IssueState::Open, &promoted.render())];
        assert_eq!(earlier(&reopened, &promoted), Earlier::Open(&reopened[0]));
    }

    #[test]
    fn covered_entries() {
        let body = format!("{}\n- `pr:OCA/pos#1` {}\n", "x", covering("pr:OCA/pos#1"));
        let grouped = pull(1, IssueState::Open, &body);
        assert!(covers(&grouped, "pr:OCA/pos#1"));
        assert!(!covers(&grouped, "pr:OCA/pos#12"));
    }
}
