//! The interactive view (`tui` feature).
//!
//! Sections are tabs, entries rows, one key per action. [`App`] is the
//! state and decides what a key means;
//! the caller runs the [`Command`]s it returns, so every action goes
//! through the same code the command line uses. Porting upstream asks for
//! a confirmation that names the entry; a mute asks for its reason.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, TableState, Tabs};

use crate::{Action, Dashboard, Entry, Section};

/// A key the view understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Previous tab.
    Left,
    /// Next tab.
    Right,
    /// Previous row.
    Up,
    /// Next row.
    Down,
    /// Confirm.
    Enter,
    /// Cancel, or leave.
    Esc,
    /// Delete the last character typed.
    Backspace,
    /// A character.
    Char(char),
}

/// The key of a terminal key event; `Ctrl-C` leaves like `Esc`.
#[must_use]
pub fn key_of(event: KeyEvent) -> Option<Key> {
    match event.code {
        KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => Some(Key::Esc),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right | KeyCode::Tab => Some(Key::Right),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Char(c) => Some(Key::Char(c)),
        _ => None,
    }
}

/// What the caller is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Take `action` on entry `id` (`reason` for a mute).
    Act {
        /// The entry.
        id: String,
        /// What to do.
        action: Action,
        /// Why, for a mute.
        reason: Option<String>,
    },
    /// Build the dashboard again.
    Refresh,
    /// Leave the view.
    Quit,
}

/// A question the view is asking.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    /// The reason of a mute, as typed so far.
    Reason(String),
    /// Whether to port the entry upstream.
    Port,
}

/// The state of the view.
#[derive(Debug, Clone)]
pub struct App {
    /// `(series, section)` of every tab: the sections with entries.
    tabs: Vec<(String, Section)>,
    tab: usize,
    row: usize,
    prompt: Option<Prompt>,
    /// The last message for the person.
    pub status: String,
}

impl App {
    /// The view of `dashboards`, on its first tab.
    #[must_use]
    pub fn new(dashboards: &[Dashboard]) -> Self {
        let mut app = Self {
            tabs: Vec::new(),
            tab: 0,
            row: 0,
            prompt: None,
            status: "i include  p port  m mute  d drop  f follow  r refresh  q quit".to_owned(),
        };
        app.load(dashboards);
        app
    }

    /// Show `dashboards` instead, keeping the tab where possible.
    pub fn load(&mut self, dashboards: &[Dashboard]) {
        self.tabs = dashboards
            .iter()
            .flat_map(|dashboard| {
                dashboard
                    .sections
                    .iter()
                    .filter(|section| !section.entries.is_empty())
                    .map(|section| (dashboard.series.clone(), section.clone()))
            })
            .collect();
        self.tab = self.tab.min(self.tabs.len().saturating_sub(1));
        self.row = 0;
    }

    /// The entry under the cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        self.tabs
            .get(self.tab)
            .and_then(|(_, section)| section.entries.get(self.row))
    }

    fn rows(&self) -> usize {
        self.tabs
            .get(self.tab)
            .map_or(0, |(_, section)| section.entries.len())
    }

    /// Take `action` on the selected entry, if it offers it.
    fn act(&mut self, action: Action) -> Option<Command> {
        let entry = self.selected()?;
        if !entry.actions.contains(&action) {
            self.status = format!("{} offers no {} action", entry.id, action.name());
            return None;
        }
        let id = entry.id.clone();
        match action {
            Action::Mute => {
                self.prompt = Some(Prompt::Reason(String::new()));
                self.status = format!("why mute {id}? (Enter to record, Esc to cancel)");
                None
            }
            Action::Port => {
                self.prompt = Some(Prompt::Port);
                self.status = format!("port {id} upstream? (y to confirm, anything else cancels)");
                None
            }
            Action::Include | Action::Drop | Action::Follow => Some(Command::Act {
                id,
                action,
                reason: None,
            }),
        }
    }

    /// Handle `key`; what the caller should do, if anything.
    pub fn key(&mut self, key: Key) -> Option<Command> {
        match (self.prompt.take(), key) {
            (Some(Prompt::Reason(_)), Key::Esc) | (Some(Prompt::Port), _)
                if key != Key::Char('y') =>
            {
                "cancelled".clone_into(&mut self.status);
                None
            }
            (Some(Prompt::Port), _) => self.selected().map(|entry| Command::Act {
                id: entry.id.clone(),
                action: Action::Port,
                reason: None,
            }),
            (Some(Prompt::Reason(reason)), Key::Enter) if !reason.trim().is_empty() => {
                self.selected().map(|entry| Command::Act {
                    id: entry.id.clone(),
                    action: Action::Mute,
                    reason: Some(reason),
                })
            }
            (Some(Prompt::Reason(mut reason)), key) => {
                match key {
                    Key::Char(c) => reason.push(c),
                    Key::Backspace => {
                        reason.pop();
                    }
                    _ => {}
                }
                self.status = format!("why? {reason}");
                self.prompt = Some(Prompt::Reason(reason));
                None
            }
            (None, Key::Esc | Key::Char('q')) => Some(Command::Quit),
            (None, Key::Right) => {
                self.tab = (self.tab + 1) % self.tabs.len().max(1);
                self.row = 0;
                None
            }
            (None, Key::Left) => {
                let count = self.tabs.len().max(1);
                self.tab = (self.tab + count - 1) % count;
                self.row = 0;
                None
            }
            (None, Key::Down) => {
                self.row = (self.row + 1).min(self.rows().saturating_sub(1));
                None
            }
            (None, Key::Up) => {
                self.row = self.row.saturating_sub(1);
                None
            }
            (None, Key::Char('r')) => Some(Command::Refresh),
            (None, Key::Char(c)) => match c {
                'i' => self.act(Action::Include),
                'p' => self.act(Action::Port),
                'm' => self.act(Action::Mute),
                'd' => self.act(Action::Drop),
                'f' => self.act(Action::Follow),
                _ => None,
            },
            (None, Key::Enter | Key::Backspace) => None,
        }
    }

    /// Draw the view.
    pub fn render(&self, frame: &mut Frame<'_>) {
        let [tabs, body, status] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .areas(frame.area());
        let titles: Vec<Line<'_>> = self
            .tabs
            .iter()
            .map(|(series, section)| {
                Line::from(format!(
                    "{series} {} ({})",
                    section.kind.title(),
                    section.entries.len()
                ))
            })
            .collect();
        frame.render_widget(
            Tabs::new(titles)
                .select(self.tab)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("odx dashboard"),
                )
                .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
            tabs,
        );
        let rows: Vec<Row<'_>> = self
            .tabs
            .get(self.tab)
            .map(|(_, section)| section.entries.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                let actions: Vec<&str> = entry.actions.iter().map(|a| a.name()).collect();
                Row::new(vec![
                    entry.id.clone(),
                    entry.state.clone(),
                    entry.subject.clone(),
                    actions.join(","),
                ])
            })
            .collect();
        let empty = rows.is_empty();
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(30),
                Constraint::Percentage(15),
                Constraint::Percentage(40),
                Constraint::Percentage(15),
            ],
        )
        .block(Block::default().borders(Borders::ALL).title(if empty {
            "Nothing to report"
        } else {
            "Entries"
        }))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        let mut state = TableState::default().with_selected(Some(self.row));
        frame.render_stateful_widget(table, body, &mut state);
        frame.render_widget(
            Paragraph::new(self.status.as_str()).block(Block::default().borders(Borders::ALL)),
            status,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SectionKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyEventKind;

    fn entry(id: &str, actions: &[Action]) -> Entry {
        Entry {
            id: id.to_owned(),
            addon: None,
            subject: format!("subject of {id}"),
            state: "open".to_owned(),
            actions: actions.to_vec(),
            notes: Vec::new(),
        }
    }

    fn dashboards() -> Vec<Dashboard> {
        vec![Dashboard {
            schema: 1,
            series: "17.0".to_owned(),
            generated_at: 0,
            sections: vec![
                Section {
                    kind: SectionKind::Pinned,
                    fetched_at: 0,
                    entries: vec![
                        entry("pr:a#1", &[Action::Drop]),
                        entry("pr:a#2", &[Action::Drop]),
                    ],
                },
                Section {
                    kind: SectionKind::Backport,
                    fetched_at: 0,
                    entries: Vec::new(),
                },
                Section {
                    kind: SectionKind::ForwardPort,
                    fetched_at: 0,
                    entries: vec![entry("fwd:a#3->17.0", &[Action::Port, Action::Mute])],
                },
            ],
        }]
    }

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|line| {
                line.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn keys_move_and_act() {
        let mut app = App::new(&dashboards());
        assert_eq!(app.selected().unwrap().id, "pr:a#1");
        assert_eq!(app.key(Key::Down), None);
        assert_eq!(app.key(Key::Down), None);
        assert_eq!(app.selected().unwrap().id, "pr:a#2");
        assert_eq!(
            app.key(Key::Char('d')),
            Some(Command::Act {
                id: "pr:a#2".to_owned(),
                action: Action::Drop,
                reason: None
            })
        );
        assert_eq!(app.key(Key::Char('i')), None);
        assert_eq!(app.status, "pr:a#2 offers no include action");
        assert_eq!(app.key(Key::Char('f')), None);
        assert_eq!(app.status, "pr:a#2 offers no follow action");
        assert_eq!(app.key(Key::Up), None);
        assert_eq!(app.selected().unwrap().id, "pr:a#1");
        assert_eq!(app.key(Key::Char('z')), None);
        assert_eq!(app.key(Key::Enter), None);
        assert_eq!(app.key(Key::Backspace), None);
        assert_eq!(app.key(Key::Char('r')), Some(Command::Refresh));
        // The empty section is no tab: right goes to the forward ports.
        assert_eq!(app.key(Key::Right), None);
        assert_eq!(app.selected().unwrap().id, "fwd:a#3->17.0");
        assert_eq!(app.key(Key::Right), None);
        assert_eq!(app.selected().unwrap().id, "pr:a#1");
        assert_eq!(app.key(Key::Left), None);
        assert_eq!(app.selected().unwrap().id, "fwd:a#3->17.0");
        assert_eq!(app.key(Key::Char('q')), Some(Command::Quit));
        assert_eq!(app.key(Key::Esc), Some(Command::Quit));
    }

    #[test]
    fn porting_and_muting_ask_first() {
        let mut app = App::new(&dashboards());
        app.key(Key::Right);
        assert_eq!(app.key(Key::Char('p')), None);
        assert!(
            app.status.starts_with("port fwd:a#3->17.0 upstream?"),
            "{}",
            app.status
        );
        assert_eq!(app.key(Key::Char('n')), None);
        assert_eq!(app.status, "cancelled");
        app.key(Key::Char('p'));
        assert_eq!(
            app.key(Key::Char('y')),
            Some(Command::Act {
                id: "fwd:a#3->17.0".to_owned(),
                action: Action::Port,
                reason: None
            })
        );
        assert_eq!(app.key(Key::Char('m')), None);
        assert_eq!(app.key(Key::Enter), None);
        for c in "notx".chars() {
            app.key(Key::Char(c));
        }
        app.key(Key::Backspace);
        app.key(Key::Left);
        assert_eq!(app.status, "why? not");
        assert_eq!(
            app.key(Key::Enter),
            Some(Command::Act {
                id: "fwd:a#3->17.0".to_owned(),
                action: Action::Mute,
                reason: Some("not".to_owned())
            })
        );
        app.key(Key::Char('m'));
        assert_eq!(app.key(Key::Esc), None);
        assert_eq!(app.status, "cancelled");
    }

    #[test]
    fn rendering_and_reloading() {
        let mut app = App::new(&dashboards());
        let shown = screen(&app);
        assert!(shown.contains("17.0 Pinned pull requests (2)"), "{shown}");
        assert!(shown.contains("pr:a#1"), "{shown}");
        app.key(Key::Right);
        app.load(&[]);
        assert_eq!(app.selected(), None);
        assert_eq!(app.key(Key::Char('d')), None);
        assert_eq!(app.key(Key::Down), None);
        assert_eq!(app.key(Key::Right), None);
        assert_eq!(app.key(Key::Left), None);
        assert!(screen(&app).contains("Nothing to report"));
    }

    #[test]
    fn terminal_keys() {
        let press = |code, modifiers| {
            key_of(KeyEvent::new_with_kind(
                code,
                modifiers,
                KeyEventKind::Press,
            ))
        };
        assert_eq!(
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Key::Esc)
        );
        assert_eq!(
            press(KeyCode::Char('c'), KeyModifiers::NONE),
            Some(Key::Char('c'))
        );
        for (code, key) in [
            (KeyCode::Left, Key::Left),
            (KeyCode::Right, Key::Right),
            (KeyCode::Tab, Key::Right),
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Enter, Key::Enter),
            (KeyCode::Esc, Key::Esc),
            (KeyCode::Backspace, Key::Backspace),
        ] {
            assert_eq!(press(code, KeyModifiers::NONE), Some(key));
        }
        assert_eq!(press(KeyCode::F(1), KeyModifiers::NONE), None);
    }
}
