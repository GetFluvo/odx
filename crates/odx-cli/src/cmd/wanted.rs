//! `odx wanted`: addons the project may need later, watched per series
//! until they are there or someone has started migrating them, so the
//! project need not migrate them itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use odx_config::Problem;
use odx_config::edit::{EditError, PyprojectEdit};
use odx_config::schema::Wanted;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_dashboard::{Action, Entry};
use odx_forge::{CachedHttp, ForgeClient, ForgeKind, Http, PullRequest, RepoUrl, ReqwestHttp};
use odx_index::sources::distribution;
use odx_index::{Origin, Renames, SeriesIndex, series_key};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::deps::{migrates, refs};
use super::glue::repo_spec;
use crate::Context;
use crate::cli::{GlobalArgs, WantedArgs, WantedCommand};

/// Why a wanted addon was not checked, offline.
const OFFLINE: &str = "offline";

/// How many series below a wanted one are searched for where the addon
/// lived last.
const LOOKBACK: u32 = 3;

/// An open migration pull request of a wanted addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Migration {
    /// The repository it targets.
    pub(crate) url: String,
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) author: String,
    /// When it was opened (`YYYY-MM-DD`).
    pub(crate) opened: String,
    /// Its page.
    pub(crate) page: String,
}

/// Where a wanted addon stands on one series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum State {
    /// On the series, in `url` at `path`; `released` on the package index.
    Available {
        url: String,
        path: String,
        released: bool,
    },
    /// Not on the series yet, but someone is migrating it.
    Migrating(Migration),
    /// Neither; `last` says on which series and in which repository it
    /// lived last.
    Missing { last: Option<(String, String)> },
    /// Not there as far as the index knows, and odx could not ask further:
    /// `reason` says why (offline, or the forge did not answer); `last` as
    /// for [`State::Missing`].
    Unchecked {
        reason: String,
        last: Option<(String, String)>,
    },
}

impl State {
    /// `available`, `released`, `migrating`, `not-started` or `unchecked`.
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Available {
                released: false, ..
            } => "available",
            Self::Available { released: true, .. } => "released",
            Self::Migrating(_) => "migrating",
            Self::Missing { .. } => "not-started",
            Self::Unchecked { .. } => "unchecked",
        }
    }
}

/// A wanted addon on one series.
#[derive(Debug, Clone)]
pub(crate) struct Watched {
    pub(crate) addon: String,
    pub(crate) series: String,
    pub(crate) note: Option<String>,
    pub(crate) standing: State,
}

impl Watched {
    /// How the entry is known on the dashboard: `want:<addon>@<series>`.
    pub(crate) fn id(&self) -> String {
        format!("want:{}@{}", self.addon, self.series)
    }

    /// What `odx add` takes to add the addon as a package, on the
    /// project's series: the spec, the distribution and the subdirectory.
    pub(crate) fn add_spec(&self, project_series: &str) -> Option<(String, String, String)> {
        if self.series != project_series {
            return None;
        }
        let dist = distribution(&self.addon, &self.series);
        match &self.standing {
            State::Available { url, path, .. } => Some((
                format!("{}@{}", repo_spec(url), self.series),
                dist,
                path.clone(),
            )),
            State::Migrating(migration) => Some((
                format!("{}#{}", repo_spec(&migration.url), migration.number),
                dist,
                self.addon.clone(),
            )),
            State::Missing { .. } | State::Unchecked { .. } => None,
        }
    }

    /// The `odx add` command for [`Watched::add_spec`].
    fn add_command(&self, project_series: &str) -> Option<String> {
        self.add_spec(project_series).map(|(spec, dist, path)| {
            format!("odx add {spec} --package {dist} --subdirectory {path}")
        })
    }

    /// How the standing reads.
    pub(crate) fn describe(&self) -> String {
        match &self.standing {
            State::Available { url, released, .. } => format!(
                "{} in {}",
                if *released { "released" } else { "available" },
                short(url)
            ),
            State::Migrating(migration) => format!(
                "migrating: {}#{} by {}, opened {}",
                short(&migration.url),
                migration.number,
                migration.author,
                migration.opened
            ),
            State::Missing {
                last: Some((series, url)),
            } => {
                format!("not started; last on {series} in {}", short(url))
            }
            State::Missing { last: None } => {
                "not started; not found on an earlier series (set `repository` to watch for a migration)"
                    .to_owned()
            }
            State::Unchecked {
                reason,
                last: Some((series, url)),
            } => format!("not checked ({reason}); last on {series} in {}", short(url)),
            State::Unchecked { reason, last: None } => format!("not checked ({reason})"),
        }
    }
}

/// `owner/repo` of a repository URL.
fn short(url: &str) -> String {
    super::suggest::repository_name(url)
}

/// The URL of a `repository` as configured: `owner/repo` is on GitHub.
fn repository_url(repository: &str) -> String {
    if repository.contains("://") || repository.contains('@') {
        repository.to_owned()
    } else {
        format!("https://github.com/{}", repository.trim_matches('/'))
    }
}

/// The series below `series` that are searched for the addon, newest first.
fn earlier(series: &str) -> Vec<String> {
    let major = series_key(series).0;
    (major.saturating_sub(LOOKBACK)..major)
        .rev()
        .filter(|major| *major >= 8)
        .map(|major| format!("{major}.0"))
        .collect()
}

/// Where `addon` stands on `series` in the repository at `url`: migrating
/// in an open pull request, else on the branch already (merged but not
/// released yet), else nowhere. The repository's open pull requests are
/// asked once per run. A repository that cannot be asked is a warning, and
/// the error says why.
fn upstream(
    client: &ForgeClient<'_>,
    open: &mut BTreeMap<String, Option<(ForgeKind, Vec<PullRequest>)>>,
    url: &str,
    addon: &str,
    series: &str,
    warnings: &mut Vec<Problem>,
) -> Result<Option<State>, String> {
    let Some(repo) = RepoUrl::parse(url) else {
        warnings.push(Problem {
            key: format!("wanted {}", short(url)),
            message: format!("cannot look for a migration in {url}: no forge repository"),
        });
        return Err(format!("{url} is no forge repository"));
    };
    let listed = open.entry(format!("{url} {series}")).or_insert_with(|| {
        client
            .kind(&repo)
            .and_then(|kind| {
                client
                    .open_pull_requests(&repo, kind, series)
                    .map(|prs| (kind, prs))
            })
            .map_err(|e| {
                warnings.push(Problem {
                    key: format!("wanted {}", short(url)),
                    message: e.to_string(),
                });
            })
            .ok()
    });
    let silent = || format!("{} did not answer", repo.host);
    let (kind, prs) = listed.as_ref().ok_or_else(silent)?;
    if let Some(pr) = prs.iter().find(|pr| migrates(pr, addon)) {
        let (page, _) = refs(
            *kind,
            &format!("{}/{}", repo.origin(), repo.path),
            pr.number,
        );
        return Ok(Some(State::Migrating(Migration {
            url: url.to_owned(),
            number: pr.number,
            title: pr.title.clone(),
            author: pr.author.clone(),
            opened: pr.created_at.chars().take(10).collect(),
            page,
        })));
    }
    let manifest = client.manifest(&repo, *kind, series, addon).map_err(|e| {
        warnings.push(Problem {
            key: format!("wanted {}", short(url)),
            message: e.to_string(),
        });
        silent()
    })?;
    // An addon kept on the branch but not installable is not there yet.
    let installable = manifest.is_some_and(|text| {
        odx_analyze::manifest::parse_manifest(&text).map_or(true, |manifest| manifest.installable)
    });
    Ok(installable.then(|| State::Available {
        url: url.to_owned(),
        path: addon.to_owned(),
        released: false,
    }))
}

/// Where every wanted addon stands on each of its series.
pub(crate) fn watch(
    ctx: &Context<'_>,
    prepared: &Prepared,
    global: &GlobalArgs,
    warnings: &mut Vec<Problem>,
) -> Vec<Watched> {
    let config = &prepared.project.config;
    if config.wanted.is_empty() {
        return Vec::new();
    }
    let http = ReqwestHttp::new()
        .ok()
        .filter(|_| !global.offline)
        .map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let online = http.as_ref().map(|http| http as &dyn Http);
    // Each wanted series and the ones below it, with every wanted addon.
    let mut lookups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut own: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for wanted in &config.wanted {
        for series in config.wanted_series(wanted) {
            own.entry(series.clone())
                .or_default()
                .insert(wanted.addon.clone());
            for indexed in std::iter::once(series.clone()).chain(earlier(&series)) {
                lookups
                    .entry(indexed)
                    .or_default()
                    .insert(wanted.addon.clone());
            }
        }
    }
    let lookups: Vec<(String, Vec<String>)> = lookups
        .into_iter()
        .map(|(series, addons)| (series, addons.into_iter().collect()))
        .collect();
    if online.is_some() {
        ask_again(prepared.cache.root(), &own);
    }
    let index = super::upgrade::addon_index(
        ctx,
        prepared,
        online,
        Renames::default(),
        &lookups,
        global.fresh,
        warnings,
    );
    let sleep = std::thread::sleep;
    let client = online.map(|http| ForgeClient::new(http, ctx.env, &OsRunner, &sleep));
    let mut open = BTreeMap::new();
    let mut watched = Vec::new();
    for wanted in &config.wanted {
        for series in config.wanted_series(wanted) {
            let standing = standing(&index, wanted, &series, client.is_some(), |url| {
                client.as_ref().map_or_else(
                    || Err(OFFLINE.to_owned()),
                    |client| upstream(client, &mut open, url, &wanted.addon, &series, warnings),
                )
            });
            watched.push(Watched {
                addon: wanted.addon.clone(),
                series,
                note: wanted.note.clone(),
                standing,
            });
        }
    }
    watched
}

/// Forget that the package index did not know the wanted addons on their
/// own series, so this run asks it again: a migration merged since is seen
/// once it is released, not only when the cached index expires.
fn ask_again(cache: &Path, wanted: &BTreeMap<String, BTreeSet<String>>) {
    for (series, addons) in wanted {
        if let Some(mut index) = SeriesIndex::load(cache, series) {
            index.unknown.retain(|addon| !addons.contains(addon));
            // A cache that cannot be written only costs lookups next time.
            let _ = index.save(cache);
        }
    }
}

/// Where `wanted` stands on `series`: there, else migrating in its
/// repository or merged there (`find` asks the forge, or says why it
/// cannot), else not started.
fn standing(
    index: &odx_index::AddonIndex,
    wanted: &Wanted,
    series: &str,
    online: bool,
    mut find: impl FnMut(&str) -> Result<Option<State>, String>,
) -> State {
    let addon = &wanted.addon;
    if let Some(location) = index.locate(addon, series) {
        return State::Available {
            url: location.url.clone(),
            path: location.path.clone(),
            released: location.origin == Origin::PackageIndex,
        };
    }
    let last = earlier(series).into_iter().find_map(|older| {
        index
            .locate(addon, &older)
            .map(|location| (older, location.url.clone()))
    });
    let repository = wanted
        .repository
        .as_deref()
        .map(repository_url)
        .or_else(|| last.as_ref().map(|(_, url)| url.clone()));
    let Some(url) = repository else {
        // Offline, the index only knows what it learnt before.
        return if online {
            State::Missing { last }
        } else {
            State::Unchecked {
                reason: OFFLINE.to_owned(),
                last,
            }
        };
    };
    match find(&url) {
        Err(reason) => State::Unchecked { reason, last },
        Ok(Some(found)) => found,
        Ok(None) => State::Missing { last },
    }
}

/// What a wanted entry of the dashboard reached, as its comment marks it:
/// `migrating OCA/contract#9` or `available`; nothing for the others.
pub(crate) fn milestone(entry: &Entry) -> Option<String> {
    if let Some(rest) = entry.state.strip_prefix("migrating: ") {
        let reference = rest
            .split_once(" by ")
            .map_or(rest, |(reference, _)| reference);
        return Some(format!("migrating {reference}"));
    }
    ["available ", "released "]
        .iter()
        .any(|prefix| entry.state.starts_with(prefix))
        .then(|| "available".to_owned())
}

/// The hidden line a comment about `milestone` of entry `id` carries, by
/// which a later run knows it was posted.
pub(crate) fn marker(id: &str, milestone: &str) -> String {
    format!("<!-- odx:wanted {id} {milestone} -->")
}

/// The comment announcing `milestone` of `entry`, mentioning `notify`.
pub(crate) fn notice(entry: &Entry, milestone: &str, notify: &[String]) -> String {
    let mut body = String::new();
    for user in notify {
        let _ = write!(body, "@{} ", user.trim_start_matches('@'));
    }
    let subject = &entry.subject;
    if milestone.starts_with("migrating") {
        let _ = write!(
            body,
            "Someone started migrating the wanted addon {subject}: {}.",
            entry.state.trim_start_matches("migrating: ")
        );
    } else {
        let _ = write!(
            body,
            "The wanted addon {subject} is there: {}.",
            entry.state
        );
    }
    if entry.actions.contains(&Action::Include) {
        let _ = write!(
            body,
            " `odx dashboard act {} --include` adds it to the project.",
            entry.id
        );
    }
    let _ = write!(body, "\n\n{}\n", marker(&entry.id, milestone));
    body
}

/// What `odx --json wanted` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx wanted")]
pub(crate) struct WantedJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The project's Odoo series.
    series: &'a str,
    /// Every wanted addon on each series it is watched on.
    addons: Vec<WatchedJson<'a>>,
}

/// A wanted addon on one series.
#[derive(Debug, Serialize, JsonSchema)]
struct WatchedJson<'a> {
    /// The addon.
    addon: &'a str,
    /// The series.
    series: &'a str,
    /// Why it is wanted.
    note: Option<&'a str>,
    /// `available`, `released`, `migrating`, `not-started` or `unchecked`.
    state: &'static str,
    /// The repository it is in, is migrated in or was last in.
    repository: Option<&'a str>,
    /// The series it was last on, when it is not started or unchecked.
    last_series: Option<&'a str>,
    /// The migration pull request, when it is migrating.
    pull_request: Option<MigrationJson<'a>>,
    /// Why it was not checked, when it is unchecked: `offline`, or which
    /// forge did not answer.
    reason: Option<&'a str>,
    /// The `odx add` command that adds it, on the project's series.
    add: Option<String>,
}

/// A migration pull request.
#[derive(Debug, Serialize, JsonSchema)]
struct MigrationJson<'a> {
    /// Its number.
    number: u64,
    /// Its title.
    title: &'a str,
    /// Who opened it.
    author: &'a str,
    /// When it was opened, `YYYY-MM-DD`.
    opened: &'a str,
    /// Its page.
    url: &'a str,
}

fn json(watched: &[Watched], series: &str) -> String {
    super::to_json(&WantedJson {
        schema: 1,
        series,
        addons: watched
            .iter()
            .map(|watched| {
                let (repository, last_series, pull_request) = match &watched.standing {
                    State::Available { url, .. } => (Some(url.as_str()), None, None),
                    State::Migrating(migration) => (
                        Some(migration.url.as_str()),
                        None,
                        Some(MigrationJson {
                            number: migration.number,
                            title: &migration.title,
                            author: &migration.author,
                            opened: &migration.opened,
                            url: &migration.page,
                        }),
                    ),
                    State::Missing { last } | State::Unchecked { last, .. } => (
                        last.as_ref().map(|(_, url)| url.as_str()),
                        last.as_ref().map(|(series, _)| series.as_str()),
                        None,
                    ),
                };
                let reason = match &watched.standing {
                    State::Unchecked { reason, .. } => Some(reason.as_str()),
                    _ => None,
                };
                WatchedJson {
                    addon: &watched.addon,
                    series: &watched.series,
                    note: watched.note.as_deref(),
                    state: watched.standing.name(),
                    repository,
                    last_series,
                    pull_request,
                    reason,
                    add: watched.add_command(series),
                }
            })
            .collect(),
    })
}

fn text(watched: &[Watched], series: &str) -> String {
    let mut out = String::new();
    if watched.is_empty() {
        out.push_str("no wanted addons; `odx wanted add <addon>` watches one\n");
    }
    let mut addon = "";
    for item in watched {
        if item.addon != addon {
            addon = &item.addon;
            let note = item
                .note
                .as_ref()
                .map(|note| format!(" ({note})"))
                .unwrap_or_default();
            let _ = writeln!(out, "{addon}{note}");
        }
        let _ = writeln!(out, "  {}  {}", item.series, item.describe());
        if let State::Migrating(migration) = &item.standing {
            let _ = writeln!(out, "        {}", migration.page);
        }
        if let Some(add) = item.add_command(series) {
            let _ = writeln!(out, "        {add}");
        }
    }
    out
}

/// Write `pyproject.toml` as `edited`; an addon that is not wanted is a
/// usage error.
fn write(prepared: &Prepared, edited: Result<String, EditError>) -> Result<(), Report> {
    let file = &prepared.project.file;
    let failed = |e: &dyn std::fmt::Display| {
        Report::error(Exit::Failure, format!("{}: {e}", file.display()))
    };
    let text = edited.map_err(|e| match e {
        EditError::NotFound(message) => Report::error(Exit::Usage, message),
        other => failed(&other),
    })?;
    std::fs::write(file, text).map_err(|e| failed(&e))
}

/// `prepared`'s `pyproject.toml` with `edit` applied.
fn edited(
    prepared: &Prepared,
    edit: impl FnOnce(&mut PyprojectEdit) -> Result<(), EditError>,
) -> Result<String, EditError> {
    PyprojectEdit::parse(&prepared.project.text)
        .and_then(|mut document| edit(&mut document).map(|()| document.to_string()))
}

/// `odx wanted`, `odx wanted add` and `odx wanted remove`.
pub(crate) fn run_wanted(args: &WantedArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    wanted(args, global, ctx).unwrap_or_else(|report| report)
}

fn wanted(args: &WantedArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let file = prepared.project.file.display();
    match &args.command {
        Some(WantedCommand::Add {
            addon,
            series,
            repository,
            note,
        }) => {
            write(
                &prepared,
                edited(&prepared, |edit| {
                    edit.want(addon, series, repository.as_deref(), note.as_deref())
                }),
            )?;
            let mut out =
                format!("{addon} is wanted in {file}; `odx wanted` shows where it stands\n");
            let located = repository.is_some()
                || prepared
                    .project
                    .config
                    .wanted
                    .iter()
                    .any(|known| known.addon == *addon && known.repository.is_some());
            if !located {
                let _ = writeln!(
                    out,
                    "without a repository, odx looks for one on the {LOOKBACK} series before; pass --repository for an addon that did not live there"
                );
            }
            Ok(Report {
                out,
                ..Report::default()
            })
        }
        Some(WantedCommand::Remove { addon }) => {
            write(&prepared, edited(&prepared, |edit| edit.unwant(addon)))?;
            Ok(Report {
                out: format!("{addon} is no longer wanted in {file}\n"),
                ..Report::default()
            })
        }
        None => {
            let mut warnings = Vec::new();
            let watched = watch(ctx, &prepared, global, &mut warnings);
            let series = &prepared.project.config.odoo_version;
            Ok(Report {
                out: if global.json {
                    json(&watched, series)
                } else {
                    text(&watched, series)
                },
                warnings,
                ..Report::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watched(series: &str, standing: State) -> Watched {
        Watched {
            addon: "contract".to_owned(),
            series: series.to_owned(),
            note: None,
            standing,
        }
    }

    fn entry(state: &str, actions: Vec<Action>) -> Entry {
        Entry {
            id: "want:contract@17.0".to_owned(),
            addon: Some("contract".to_owned()),
            subject: "contract on 17.0".to_owned(),
            state: state.to_owned(),
            actions,
            notes: Vec::new(),
        }
    }

    #[test]
    fn milestones_and_their_comments() {
        let migrating = entry(
            "migrating: OCA/contract#9 by joe, opened 2026-01-01",
            vec![Action::Include],
        );
        let reached = milestone(&migrating).unwrap();
        assert_eq!(reached, "migrating OCA/contract#9");
        assert_eq!(
            notice(&migrating, &reached, &["ann".to_owned(), "@bob".to_owned()]),
            "@ann @bob Someone started migrating the wanted addon contract on 17.0: OCA/contract#9 by joe, opened 2026-01-01. `odx dashboard act want:contract@17.0 --include` adds it to the project.\n\n<!-- odx:wanted want:contract@17.0 migrating OCA/contract#9 -->\n"
        );
        let released = entry("released in OCA/contract", Vec::new());
        assert_eq!(milestone(&released).as_deref(), Some("available"));
        assert_eq!(
            notice(&released, "available", &[]),
            "The wanted addon contract on 17.0 is there: released in OCA/contract.\n\n<!-- odx:wanted want:contract@17.0 available -->\n"
        );
        assert_eq!(
            milestone(&entry("available in OCA/contract", Vec::new())).as_deref(),
            Some("available")
        );
        assert_eq!(milestone(&entry("not started", Vec::new())), None);
    }

    #[test]
    fn earlier_series_stop_at_eight() {
        assert_eq!(earlier("18.0"), ["17.0", "16.0", "15.0"]);
        assert_eq!(earlier("9.0"), ["8.0"]);
    }

    #[test]
    fn repositories_as_configured() {
        assert_eq!(
            repository_url("OCA/contract"),
            "https://github.com/OCA/contract"
        );
        assert_eq!(repository_url("https://git.x/o/r"), "https://git.x/o/r");
        assert_eq!(repository_url("git@git.x:o/r.git"), "git@git.x:o/r.git");
    }

    #[test]
    fn standings_read_and_add() {
        let migrating = watched(
            "17.0",
            State::Migrating(Migration {
                url: "https://github.com/OCA/contract".to_owned(),
                number: 9,
                title: "[17.0][MIG] contract".to_owned(),
                author: "joe".to_owned(),
                opened: "2026-01-01".to_owned(),
                page: "https://github.com/OCA/contract/pull/9".to_owned(),
            }),
        );
        assert_eq!(migrating.id(), "want:contract@17.0");
        assert_eq!(
            migrating.describe(),
            "migrating: OCA/contract#9 by joe, opened 2026-01-01"
        );
        assert_eq!(
            migrating.add_command("17.0").unwrap(),
            "odx add OCA/contract#9 --package odoo-addon-contract --subdirectory contract"
        );
        assert_eq!(migrating.add_command("16.0"), None);
        let released = watched(
            "17.0",
            State::Available {
                url: "https://github.com/OCA/contract".to_owned(),
                path: "contract".to_owned(),
                released: true,
            },
        );
        assert_eq!(
            (released.standing.name(), released.describe().as_str()),
            ("released", "released in OCA/contract")
        );
        let available = watched(
            "18.0",
            State::Available {
                url: "https://github.com/OCA/contract".to_owned(),
                path: "contract".to_owned(),
                released: false,
            },
        );
        assert_eq!(available.standing.name(), "available");
        assert_eq!(available.describe(), "available in OCA/contract");
        assert_eq!(
            available.add_command("18.0").unwrap(),
            "odx add OCA/contract@18.0 --package odoo-addon-contract --subdirectory contract"
        );
        assert_eq!(migrating.standing.name(), "migrating");
        let last = watched(
            "18.0",
            State::Missing {
                last: Some((
                    "17.0".to_owned(),
                    "https://github.com/OCA/contract".to_owned(),
                )),
            },
        );
        assert_eq!(
            (last.standing.name(), last.describe().as_str()),
            ("not-started", "not started; last on 17.0 in OCA/contract")
        );
        let missing = watched("18.0", State::Missing { last: None });
        assert_eq!(
            missing.describe(),
            "not started; not found on an earlier series (set `repository` to watch for a migration)"
        );
        assert_eq!(missing.add_command("18.0"), None);
        let unchecked = watched(
            "18.0",
            State::Unchecked {
                reason: OFFLINE.to_owned(),
                last: None,
            },
        );
        assert_eq!(
            (unchecked.standing.name(), unchecked.describe().as_str()),
            ("unchecked", "not checked (offline)")
        );
        let known = watched(
            "18.0",
            State::Unchecked {
                reason: OFFLINE.to_owned(),
                last: Some((
                    "17.0".to_owned(),
                    "https://github.com/OCA/contract".to_owned(),
                )),
            },
        );
        assert_eq!(
            known.describe(),
            "not checked (offline); last on 17.0 in OCA/contract"
        );
        assert_eq!(known.add_command("18.0"), None);
        assert_eq!(unchecked.add_command("18.0"), None);
    }
}
