//! The dashboard issue: one per series, found by its label and the marker
//! on its first line, its body regenerated on every run around the notes a
//! person keeps below `<!-- odx:notes -->`.

use std::fmt::Write as _;

use odx_dashboard::{Action, ActionLine, Dashboard, sections, ticked};
use odx_forge::{Issue, IssueState};

use crate::pr::{Earlier, Marker, earlier};

/// The label of the dashboard issue.
pub const LABEL: &str = "odx:dashboard";

/// Where the part of the body people may write starts.
pub const NOTES: &str = "<!-- odx:notes -->";

/// The line starting with this changes on every run and is left out when
/// comparing bodies.
const LAST_RUN: &str = "Last run: ";

/// The title of the issue for `series`.
#[must_use]
pub fn title(series: &str) -> String {
    format!("odx upstream dashboard ({series})")
}

/// The marker on the first line of the issue for `series`.
#[must_use]
pub fn header(series: &str) -> String {
    format!("<!-- odx:dashboard series={series} -->")
}

/// The issue for `series` among `issues` (those labelled [`LABEL`]),
/// found by its marker, never by its title; an open one before a closed
/// one.
#[must_use]
pub fn find<'a>(issues: &'a [Issue], series: &str) -> Option<&'a Issue> {
    let header = header(series);
    let mut found: Vec<&Issue> = issues
        .iter()
        .filter(|issue| issue.body.lines().next().map(str::trim) == Some(header.as_str()))
        .collect();
    found.sort_by_key(|issue| issue.state != IssueState::Open);
    found.first().copied()
}

/// What a run knows when it writes the body.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    /// The dashboard of the series.
    pub dashboard: &'a Dashboard,
    /// When the run started, for people (`2026-10-12 02:10 UTC`).
    pub now: &'a str,
    /// The page of the workflow run, when there is one.
    pub run: Option<&'a str>,
    /// The bot's pull requests, open and closed.
    pub pulls: &'a [Issue],
    /// Boxes ticked that this run could not act on: they stay ticked.
    pub pending: &'a [(String, Action)],
    /// Boxes this run acted on without a pull request, with what it did.
    pub recorded: &'a [(String, Action, String)],
    /// What the run left for the next one (rate limits, `--max-prs`).
    pub deferred: &'a [String],
    /// Whether the run stopped early.
    pub partial: bool,
}

/// The issue's body for `page`, keeping the notes of `old`.
#[must_use]
pub fn body(page: &Page<'_>, old: Option<&str>) -> String {
    let dashboard = page.dashboard;
    let mut out = format!(
        "{}\n# {}\n\n{LAST_RUN}{}{}.\n\n",
        header(&dashboard.series),
        title(&dashboard.series),
        page.now,
        page.run
            .map(|run| format!(" ([workflow run]({run}))"))
            .unwrap_or_default(),
    );
    out.push_str(
        "Tick a box to ask the bot for a pull request (or a mute) on the next run. \
         Changes upstream need a human review there; nothing here comments on \
         upstream repositories.\n",
    );
    if page.partial {
        out.push_str(
            "\n> [!NOTE]\n> This run stopped early (rate limit); the rest follows on the next run.\n",
        );
    }
    let line = |entry: &odx_dashboard::Entry, action: Action| {
        if action == Action::Port {
            return ActionLine::Note(format!(
                "port upstream with `odx dashboard act {} --port --open-pr`",
                entry.id
            ));
        }
        let open = Marker::of_entry(action, &entry.id).and_then(|marker| {
            match earlier(page.pulls, &marker) {
                Earlier::Open(pull) => Some(pull),
                Earlier::Closed(_) | Earlier::None => None,
            }
        });
        if let Some((_, _, text)) = page
            .recorded
            .iter()
            .find(|(id, done, _)| *id == entry.id && *done == action)
        {
            return ActionLine::Note(text.clone());
        }
        let pending = page
            .pending
            .iter()
            .any(|(id, ticked)| *id == entry.id && *ticked == action);
        open.map_or(
            if pending {
                ActionLine::Ticked
            } else {
                ActionLine::Checkbox
            },
            |pull| ActionLine::Note(format!("[#{}]({})", pull.number, pull.url)),
        )
    };
    out.push_str(&sections(dashboard, &line));
    let open: Vec<&Issue> = page
        .pulls
        .iter()
        .filter(|pull| pull.state == IssueState::Open && Marker::parse(&pull.body).is_some())
        .collect();
    if !open.is_empty() {
        let _ = write!(out, "\n## Open bot pull requests ({})\n\n", open.len());
        for pull in open {
            let _ = writeln!(out, "- [#{}]({}) {}", pull.number, pull.url, pull.title);
        }
    }
    if !page.deferred.is_empty() {
        let _ = write!(
            out,
            "\n## Deferred to the next run ({})\n\n",
            page.deferred.len()
        );
        for item in page.deferred {
            let _ = writeln!(out, "- {item}");
        }
    }
    let notes = old.and_then(|old| old.split_once(NOTES)).map_or(
        "\nNotes below this line are kept between runs.\n",
        |(_, notes)| notes,
    );
    let _ = write!(out, "\n{NOTES}{notes}");
    out
}

/// Whether two bodies differ in more than the time of the run.
#[must_use]
pub fn same(a: &str, b: &str) -> bool {
    let stable = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with(LAST_RUN))
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    };
    stable(a) == stable(b)
}

/// A box ticked on the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The entry.
    pub id: String,
    /// What was asked.
    pub action: Action,
    /// The marker of the pull request that does it.
    pub marker: Marker,
}

/// The boxes ticked in `body` that `dashboard` still offers, except
/// porting upstream, which the bot never does. Text below the notes
/// marker is a person's and is not read.
#[must_use]
pub fn requests(body: &str, dashboard: &Dashboard) -> Vec<Request> {
    let generated = body
        .split_once(NOTES)
        .map_or(body, |(generated, _)| generated);
    let mut found: Vec<Request> = ticked(generated)
        .into_iter()
        .filter(|(id, action)| {
            dashboard
                .entry(id)
                .is_some_and(|entry| entry.actions.contains(action))
        })
        .filter_map(|(id, action)| {
            Marker::of_entry(action, &id).map(|marker| Request { id, action, marker })
        })
        .collect();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_dashboard::{Entry, Section, SectionKind};

    fn dashboard() -> Dashboard {
        Dashboard {
            schema: 1,
            series: "17.0".to_owned(),
            generated_at: 1,
            sections: vec![
                Section {
                    kind: SectionKind::NewUpstream,
                    fetched_at: 1,
                    entries: vec![Entry {
                        id: "new:OCA/pos#789".to_owned(),
                        addon: Some("pos_discount".to_owned()),
                        subject: "Fix rounding".to_owned(),
                        state: "open, CI green".to_owned(),
                        actions: vec![Action::Include, Action::Mute],
                        notes: Vec::new(),
                    }],
                },
                Section {
                    kind: SectionKind::ForwardPort,
                    fetched_at: 1,
                    entries: vec![Entry {
                        id: "fwd:pos.pos_x->17.0".to_owned(),
                        addon: Some("pos_x".to_owned()),
                        subject: "totals".to_owned(),
                        state: "from 16.0".to_owned(),
                        actions: vec![Action::Port, Action::Include],
                        notes: Vec::new(),
                    }],
                },
            ],
        }
    }

    fn issue(number: u64, state: IssueState, body: &str) -> Issue {
        Issue {
            number,
            title: format!("odx: #{number}"),
            body: body.to_owned(),
            state,
            labels: vec![LABEL.to_owned()],
            url: format!("https://x/{number}"),
            pull: false,
        }
    }

    fn page<'a>(dashboard: &'a Dashboard, pulls: &'a [Issue]) -> Page<'a> {
        Page {
            dashboard,
            now: "2026-10-12 02:10 UTC",
            run: Some("https://x/run/1"),
            pulls,
            pending: &[],
            recorded: &[],
            deferred: &[],
            partial: false,
        }
    }

    #[test]
    fn finds_the_issue_by_marker() {
        let issues = [
            issue(1, IssueState::Open, "odx upstream dashboard (17.0)"),
            issue(2, IssueState::Closed, &header("17.0")),
            issue(3, IssueState::Open, &format!("{}\nbody", header("17.0"))),
            issue(4, IssueState::Open, &header("18.0")),
        ];
        assert_eq!(find(&issues, "17.0").map(|i| i.number), Some(3));
        assert_eq!(find(&issues[..2], "17.0").map(|i| i.number), Some(2));
        assert_eq!(find(&issues, "16.0"), None);
        assert_eq!(title("17.0"), "odx upstream dashboard (17.0)");
    }

    #[test]
    fn writes_the_body() {
        let model = dashboard();
        let text = body(&page(&model, &[]), None);
        assert_eq!(
            text,
            "<!-- odx:dashboard series=17.0 -->\n\
             # odx upstream dashboard (17.0)\n\n\
             Last run: 2026-10-12 02:10 UTC ([workflow run](https://x/run/1)).\n\n\
             Tick a box to ask the bot for a pull request (or a mute) on the next run. \
             Changes upstream need a human review there; nothing here comments on \
             upstream repositories.\n\
             \n## New upstream pull requests (1)\n\n\
             - `new:OCA/pos#789` open, CI green: Fix rounding\n  \
             - [ ] include <!-- odx:include new:OCA/pos#789 -->\n  \
             - [ ] mute <!-- odx:mute new:OCA/pos#789 -->\n\
             \n## Forward-port suggestions (1)\n\n\
             - `fwd:pos.pos_x->17.0` from 16.0: totals\n  \
             - port: port upstream with `odx dashboard act fwd:pos.pos_x->17.0 --port --open-pr`\n  \
             - [ ] include <!-- odx:include fwd:pos.pos_x->17.0 -->\n\
             \n<!-- odx:notes -->\nNotes below this line are kept between runs.\n"
        );
        assert_eq!(requests(&text, &model), []);
        // Porting upstream is never asked of the bot.
        assert_eq!(
            requests(
                &text.replace("- port:", "- [x] <!-- odx:port fwd:pos.pos_x->17.0 -->"),
                &model
            ),
            []
        );
    }

    fn asked(body: &str, dashboard: &Dashboard) -> Vec<(String, Action)> {
        requests(body, dashboard)
            .into_iter()
            .map(|request| {
                assert_eq!(
                    Some(request.marker),
                    Marker::of_entry(request.action, &request.id)
                );
                (request.id, request.action)
            })
            .collect()
    }

    #[test]
    fn links_pull_requests_and_keeps_requests_and_notes() {
        let model = dashboard();
        let marker = Marker::of_entry(Action::Include, "new:OCA/pos#789").unwrap();
        let mut open = issue(9, IssueState::Open, &marker.render());
        open.pull = true;
        let closed = issue(8, IssueState::Closed, "<!-- odx:pr kind=repin key=k -->");
        let pulls = [open, closed];
        let pending = [("fwd:pos.pos_x->17.0".to_owned(), Action::Include)];
        let recorded = [(
            "new:OCA/pos#789".to_owned(),
            Action::Mute,
            "recorded m:1".to_owned(),
        )];
        let deferred = ["auto-update: --max-prs reached".to_owned()];
        let old =
            "old\n<!-- odx:notes -->\nmine\n- [x] include <!-- odx:include new:OCA/pos#789 -->\n";
        let text = body(
            &Page {
                run: None,
                pending: &pending,
                recorded: &recorded,
                deferred: &deferred,
                partial: true,
                ..page(&model, &pulls)
            },
            Some(old),
        );
        assert!(text.contains("Last run: 2026-10-12 02:10 UTC.\n"), "{text}");
        assert!(text.contains("> This run stopped early"), "{text}");
        assert!(text.contains("  - include: [#9](https://x/9)\n"), "{text}");
        assert!(text.contains("  - mute: recorded m:1\n"), "{text}");
        assert!(text.contains("  - [x] include <!-- odx:include fwd:pos.pos_x->17.0 -->\n"));
        assert!(text.contains("\n## Open bot pull requests (1)\n\n- [#9](https://x/9) odx: #9\n"));
        assert!(
            text.contains(
                "\n## Deferred to the next run (1)\n\n- auto-update: --max-prs reached\n"
            )
        );
        assert!(text.ends_with(
            "<!-- odx:notes -->\nmine\n- [x] include <!-- odx:include new:OCA/pos#789 -->\n"
        ));
        // The pending box is asked again; the one in the notes is not read.
        assert_eq!(asked(&text, &model), pending);
    }

    #[test]
    fn reads_requests() {
        let model = dashboard();
        let text = body(&page(&model, &[]), None)
            .replace("- [ ]", "- [x]")
            .replace(
                NOTES,
                "- [x] mute <!-- odx:mute gone -->\n- [x] drop <!-- odx:drop new:OCA/pos#789 -->\n<!-- odx:notes -->",
            );
        assert_eq!(
            asked(&text, &model),
            [
                ("new:OCA/pos#789".to_owned(), Action::Include),
                ("new:OCA/pos#789".to_owned(), Action::Mute),
                ("fwd:pos.pos_x->17.0".to_owned(), Action::Include),
            ]
        );
        let doubled =
            "- [x] <!-- odx:mute new:OCA/pos#789 -->\n- [x] <!-- odx:mute new:OCA/pos#789 -->\n";
        assert_eq!(asked(doubled, &model).len(), 1);
    }

    #[test]
    fn compares_without_the_run_time() {
        let model = dashboard();
        let first = body(&page(&model, &[]), None);
        let later = body(
            &Page {
                now: "2026-10-13 02:10 UTC",
                ..page(&model, &[])
            },
            Some(&first),
        );
        assert_ne!(first, later);
        assert!(same(&first, &later));
        assert!(!same(&first, &first.replace("[ ] mute", "[x] mute")));
    }
}
