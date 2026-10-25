//! `odx bot`: the dashboard as an issue of the project repository, the
//! boxes ticked there acted on as pull requests (or mutes), and the routine
//! changes (overrides merged upstream, new heads, index updates, promotions
//! asked for by label) proposed on their own, each run picking up what the
//! last one left.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use odx_aggregate::date;
use odx_bot::issue::{self, Page, Request};
use odx_bot::pr::{self, Earlier, Kind, Marker, covering, earlier};
use odx_config::Span;
use odx_config::environments::Model;
use odx_config::lock::Lockfile;
use odx_config::schema::BotAction;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_dashboard::{Action, Dashboard, Entry, SectionKind};
use odx_forge::{
    CachedHttp, ForgeClient, ForgeError, ForgeKind, Http, Issue, IssueEdit, IssueState, Proposal,
    RepoUrl, ReqwestHttp, Visibility,
};
use serde_json::{Value, json};

use super::Report;
use super::aggregate::{Prepared, prepare, run_aggregate, run_lock, vault_heads};
use super::checkout::git;
use super::dashboard::{Specs, build, run_act};
use super::promote_apply::{commit, failed, worktree};
use super::propose::forge_repo;
use super::verify::run_verify;
use crate::Context;
use crate::cli::{
    AggregateArgs, BotArgs, BotCommand, DashboardActArgs, DashboardArgs, DashboardFormat,
    GlobalArgs, LockArgs, PromoteArgs, VerdictKind, VerifyArgs,
};

/// Pull requests a run opens at most, without `--max-prs` or
/// `[tool.odx.bot].max_prs_per_run`.
const MAX_PRS: usize = 10;

/// Mutes from the dashboard expire after this many days, without
/// `[tool.odx.bot].mute_expiry`.
const MUTE_DAYS: u64 = 180;

/// The label on a merged pull request that asks for the promotion of what
/// it changed, without `[tool.odx.bot].promote_label`.
const PROMOTE_LABEL: &str = "promote";

/// What a run of `odx bot` does.
#[derive(Debug, Clone, Copy)]
struct Mode {
    /// Act on the ticked boxes.
    act: bool,
    /// Propose the routine changes and the promotions asked for by label.
    auto: bool,
    /// Check out what odx.lock pins, and verify it, first.
    full: bool,
}

/// `odx bot`.
pub(crate) fn run_bot(command: &BotCommand, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    let mode = |act, auto, full| Mode { act, auto, full };
    let (bot, mode, max_prs, plan) = match command {
        BotCommand::Run { bot, max_prs } => (bot.clone(), mode(true, true, true), *max_prs, false),
        BotCommand::Act { bot, max_prs, .. } => {
            (bot.clone(), mode(true, false, false), *max_prs, false)
        }
        BotCommand::Dashboard { bot } => (bot.clone(), mode(false, false, false), None, false),
        BotCommand::Plan { series, remote } => (
            BotArgs {
                apply: false,
                series: series.clone(),
                remote: remote.clone(),
                reopen: false,
            },
            mode(true, true, false),
            None,
            true,
        ),
    };
    let global = GlobalArgs {
        json: global.json || plan,
        fresh: true,
        ..global.clone()
    };
    cycle(&bot, mode, max_prs, &global, ctx).unwrap_or_else(|report| report)
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands the error over by value"
)]
fn forge_failure(e: ForgeError) -> Report {
    Report::error(Exit::Failure, e.to_string())
}

/// The project repository on its forge.
struct Forge<'a> {
    client: ForgeClient<'a>,
    repo: RepoUrl,
    kind: ForgeKind,
}

/// What became of one ticked box, or of one routine change.
#[derive(Debug)]
enum Outcome {
    /// A dry run: what would be done.
    Planned,
    /// Left for the next run (`--max-prs`).
    Deferred,
    /// Its pull request, opened or updated.
    Proposed(Issue, bool),
    /// Its pull request, open with the same change already.
    Unchanged(Issue),
    /// Its pull request, closed by a person: the same change is not
    /// proposed again.
    Closed(Issue),
    /// Done without a pull request (a mute in the verdict repository), or
    /// nothing to propose.
    Recorded(String),
    /// It failed; the box stays ticked.
    Failed(String),
}

/// A pull request, as JSON.
fn pull_json(pull: &Issue) -> Value {
    json!({"number": pull.number, "url": pull.url})
}

impl Outcome {
    fn json(&self) -> Value {
        match self {
            Self::Planned => json!({"outcome": "planned"}),
            Self::Deferred => json!({"outcome": "deferred"}),
            Self::Proposed(pull, opened) => json!({
                "outcome": if *opened { "opened" } else { "updated" },
                "pull_request": pull_json(pull),
            }),
            Self::Unchanged(pull) => {
                json!({"outcome": "unchanged", "pull_request": pull_json(pull)})
            }
            Self::Closed(pull) => json!({"outcome": "closed", "pull_request": pull_json(pull)}),
            Self::Recorded(message) => json!({"outcome": "recorded", "message": message}),
            Self::Failed(message) => json!({"outcome": "failed", "message": message}),
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Planned => "planned".to_owned(),
            Self::Deferred => "deferred to the next run (--max-prs)".to_owned(),
            Self::Proposed(pull, opened) => format!(
                "{} #{} {}",
                if *opened { "opened" } else { "updated" },
                pull.number,
                pull.url
            ),
            Self::Unchanged(pull) => format!("unchanged #{} {}", pull.number, pull.url),
            Self::Closed(pull) => format!("#{} was closed: not proposed again", pull.number),
            Self::Recorded(message) => message.clone(),
            Self::Failed(message) => format!("failed: {message}"),
        }
    }
}

/// When the run started, for people, and the page of the workflow run
/// (GitHub and Forgejo Actions set the same variables).
fn run_info(ctx: &Context<'_>) -> (String, Option<String>) {
    let now = SystemTime::now();
    let seconds = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let time = format!(
        "{} {:02}:{:02} UTC",
        date(now),
        seconds / 3600 % 24,
        seconds / 60 % 60
    );
    let var = |name: &str| ctx.env.var(name).filter(|value| !value.is_empty());
    let run = var("GITHUB_SERVER_URL")
        .zip(var("GITHUB_REPOSITORY"))
        .zip(var("GITHUB_RUN_ID"))
        .map(|((server, repository), id)| format!("{server}/{repository}/actions/runs/{id}"));
    (time, run)
}

/// The project repository, refused when it is public and the project did
/// not allow it.
fn project_forge<'a>(
    ctx: &Context<'a>,
    prepared: &Prepared,
    remote: &str,
    http: &'a dyn Http,
    sleep: &'a dyn Fn(Duration),
) -> Result<Forge<'a>, Report> {
    let repo = forge_repo(ctx, remote)
        .ok_or_else(|| Report::error(Exit::Usage, format!("{remote} is not on a forge")))?;
    let client = ForgeClient::new(http, ctx.env, &OsRunner, sleep);
    let kind = client.kind(&repo).map_err(forge_failure)?;
    let allowed = prepared
        .project
        .config
        .bot
        .as_ref()
        .and_then(|bot| bot.allow_public)
        .unwrap_or(false);
    if client.visibility(&repo, kind).map_err(forge_failure)? == Visibility::Public && !allowed {
        return Err(Report::error(
            Exit::Failure,
            format!(
                "{}/{} is public: the dashboard names the addons and pull requests the project \
                 uses; set [tool.odx.bot] allow_public = true to run the bot there",
                repo.host, repo.path
            ),
        ));
    }
    Ok(Forge { client, repo, kind })
}

/// The branch bot pull requests target: `[tool.odx.bot].environment`'s,
/// else the branch checked out.
fn base_branch(prepared: &Prepared) -> Result<String, Report> {
    let config = &prepared.project.config;
    if let Some(name) = config.bot.as_ref().and_then(|bot| bot.environment.as_ref()) {
        // The configuration was validated: the environment exists.
        return Ok(Model::resolve(config)
            .ok()
            .flatten()
            .and_then(|model| model.get(name).map(|env| env.branch.clone()))
            .unwrap_or_default());
    }
    git(&prepared.project.root, &["branch", "--show-current"])
        .ok()
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                "no branch checked out: set [tool.odx.bot] environment".to_owned(),
            )
        })
}

/// What `odx dashboard` shows, all of it.
const fn all_sections() -> DashboardArgs {
    DashboardArgs {
        format: DashboardFormat::Table,
        env: None,
        section: Vec::new(),
        checkboxes: false,
        act: None,
    }
}

/// `odx aggregate --locked`: the checkout odx.lock pins.
const fn locked_checkout() -> AggregateArgs {
    AggregateArgs {
        fetch_only: false,
        repo: None,
        prefer_mirror: false,
        keep_conflicts: false,
        force: false,
        bundle_dir: None,
        locked: true,
        update: false,
    }
}

/// `odx verify`, nothing more.
const fn plain_verify() -> VerifyArgs {
    VerifyArgs {
        vendored: false,
        installed: None,
        requirements: false,
        deployed: None,
        allow_shell: false,
    }
}

fn cycle(
    bot: &BotArgs,
    mode: Mode,
    max_prs: Option<usize>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    // The nightly run starts from what odx.lock pins, checked, before
    // anything is proposed.
    let ready = if mode.full {
        checked(run_aggregate(&locked_checkout(), global, ctx))
            .and_then(|_| checked(run_verify(&plain_verify(), global, ctx)))
            .map(drop)
    } else {
        Ok(())
    };
    let sleep = std::thread::sleep;
    ready
        .and_then(|()| {
            ReqwestHttp::new()
                .map(|http| CachedHttp::new(http, prepared.cache.root().join("http")))
                .map_err(forge_failure)
        })
        .and_then(|http| {
            project_forge(ctx, &prepared, &bot.remote, &http, &sleep)
                .and_then(|forge| cycle_on(&forge, bot, mode, max_prs, global, ctx, &prepared))
        })
}

/// [`cycle`] on the project repository's forge.
#[expect(
    clippy::too_many_lines,
    reason = "the run, dashboard by dashboard, as the issue shows it"
)]
fn cycle_on(
    forge: &Forge<'_>,
    bot: &BotArgs,
    mode: Mode,
    max_prs: Option<usize>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
) -> Result<Report, Report> {
    let settings = prepared.project.config.bot.as_ref();
    let base = base_branch(prepared)?;
    let mut warnings = Vec::new();
    let (dashboards, specs) = build(&all_sections(), global, ctx, prepared, &mut warnings)?;
    let issues = forge
        .client
        .labelled(&forge.repo, forge.kind, issue::LABEL, false)
        .map_err(forge_failure)?;
    let mut pulls = forge
        .client
        .labelled(&forge.repo, forge.kind, pr::LABEL, true)
        .map_err(forge_failure)?;
    let (now, run) = run_info(ctx);
    let mut budget = max_prs
        .or_else(|| settings?.max_prs_per_run.map(usize::from))
        .unwrap_or(MAX_PRS);
    let mut out = String::new();
    let job = Job {
        ctx,
        global,
        prepared,
        forge,
        dashboards: &dashboards,
        specs: &specs,
        base: &base,
        remote: &bot.remote,
    };
    let promotions = if mode.auto {
        job.promotions(bot.apply, &mut pulls, &mut budget, &mut out)?
    } else {
        Vec::new()
    };
    let mut listed = Vec::new();
    let wanted = dashboards
        .iter()
        .filter(|dashboard| bot.series.as_ref().is_none_or(|s| *s == dashboard.series));
    for dashboard in wanted {
        let found = issue::find(&issues, &dashboard.series);
        let paused = found.is_some_and(|i| i.state == IssueState::Closed) && !bot.reopen;
        if paused || settings.and_then(|bot| bot.dashboard) == Some(false) {
            let _ = writeln!(
                out,
                "{}: the dashboard is paused (its issue is closed; --reopen reopens it)",
                dashboard.series
            );
            listed.push(json!({"series": dashboard.series, "issue": {"write": "paused"}}));
            continue;
        }
        let apply = mode.act && bot.apply;
        let mut acted = Acted::default();
        if let Some(found) = found {
            let requests = issue::requests(&found.body, dashboard);
            let work = Work {
                dashboard,
                apply,
                pulls: &mut pulls,
                budget: &mut budget,
                out: &mut out,
            };
            job.all(work, found, requests, &mut acted);
        }
        if mode.auto {
            let work = Work {
                dashboard,
                apply: bot.apply,
                pulls: &mut pulls,
                budget: &mut budget,
                out: &mut out,
            };
            job.routines(work, &mut acted);
        }
        let page = Page {
            dashboard,
            now: &now,
            run: run.as_deref(),
            pulls: &pulls,
            pending: &acted.pending,
            recorded: &acted.recorded,
            deferred: &acted.deferred,
            partial: false,
        };
        let body = issue::body(&page, found.map(|i| i.body.as_str()));
        let (write, written) = write_issue(forge, bot, dashboard, found, &body)?;
        let _ = writeln!(out, "{}: dashboard issue {write}", dashboard.series);
        listed.push(json!({
            "series": dashboard.series,
            "issue": {
                "write": write,
                "number": written.as_ref().map(|i| i.number),
                "url": written.map(|i| i.url),
            },
            "requests": acted.done,
            "routine": acted.routine,
        }));
    }
    let value = json!({
        "schema": 1,
        "repository": format!("{}/{}", forge.repo.host, forge.repo.path),
        "apply": bot.apply,
        "base": base,
        "promotions": promotions,
        "dashboards": listed,
    });
    Ok(Report {
        out: if global.json {
            format!("{value:#}\n")
        } else {
            out
        },
        warnings,
        ..Report::default()
    })
}

/// Create or update the dashboard issue: what was written (`created`,
/// `updated`, `unchanged`; `would …` without `--apply`) and the issue.
fn write_issue(
    forge: &Forge<'_>,
    bot: &BotArgs,
    dashboard: &Dashboard,
    found: Option<&Issue>,
    body: &str,
) -> Result<(String, Option<Issue>), Report> {
    let Some(existing) = found else {
        if !bot.apply {
            return Ok(("would be created".to_owned(), None));
        }
        let title = issue::title(&dashboard.series);
        let created = forge
            .client
            .create_issue(&forge.repo, forge.kind, &title, body, &[issue::LABEL])
            .map_err(forge_failure)?;
        return Ok((format!("#{} created", created.number), Some(created)));
    };
    let reopen = existing.state == IssueState::Closed;
    if !reopen && issue::same(&existing.body, body) {
        return Ok((
            format!("#{} unchanged", existing.number),
            Some(existing.clone()),
        ));
    }
    if !bot.apply {
        return Ok((
            format!("#{} would be updated", existing.number),
            Some(existing.clone()),
        ));
    }
    let edit = IssueEdit {
        title: None,
        body: Some(body),
        state: reopen.then_some(IssueState::Open),
    };
    let edited = forge
        .client
        .edit_issue(&forge.repo, forge.kind, existing.number, &edit)
        .map_err(forge_failure)?;
    Ok((format!("#{} updated", edited.number), Some(edited)))
}

/// Everything acting on the project needs.
struct Job<'a> {
    ctx: &'a Context<'a>,
    global: &'a GlobalArgs,
    prepared: &'a Prepared,
    forge: &'a Forge<'a>,
    dashboards: &'a [Dashboard],
    specs: &'a Specs,
    base: &'a str,
    remote: &'a str,
}

/// What a run works through on one dashboard, and keeps current.
struct Work<'a> {
    dashboard: &'a Dashboard,
    /// Act, rather than only plan.
    apply: bool,
    /// The bot's pull requests.
    pulls: &'a mut Vec<Issue>,
    /// Pull requests the run may still open.
    budget: &'a mut usize,
    out: &'a mut String,
}

/// What acting on a dashboard did.
#[derive(Default)]
struct Acted {
    /// Each box and what became of it.
    done: Vec<Value>,
    /// Each routine change and what became of it.
    routine: Vec<Value>,
    /// The boxes that stay ticked.
    pending: Vec<(String, Action)>,
    /// Those done without a pull request, and how.
    recorded: Vec<(String, Action, String)>,
    /// What waits for the next run.
    deferred: Vec<String>,
}

/// What acting on a box wrote.
enum Written {
    /// Nothing in the project (a mute in the verdict repository).
    Nothing(String),
    /// The marker's branch, pushed, to propose into `base`.
    Branch {
        base: String,
        title: String,
        body: String,
    },
}

/// A change a run proposes on its own (`[tool.odx.bot].auto`), one pull
/// request for everything a dashboard section found.
struct Routine {
    kind: Kind,
    auto: BotAction,
    /// The section whose entries it takes care of.
    section: SectionKind,
    /// Which of them.
    wanted: fn(&Entry) -> bool,
    /// What `odx dashboard act` does for each (none: `odx lock --update`).
    action: Option<Action>,
    /// How many entries that takes (`odx lock --auto-update` takes all).
    take: usize,
    title: &'static str,
    intro: &'static str,
}

/// Whether a pinned pull request's head moved upstream.
fn moved(entry: &Entry) -> bool {
    ["changed", "orphaned", "rebased-same-patch"]
        .iter()
        .any(|head| entry.state.ends_with(&format!(", head {head}")))
}

/// Whether an entry offers anything.
const fn actionable(entry: &Entry) -> bool {
    !entry.actions.is_empty()
}

const ROUTINES: [Routine; 3] = [
    Routine {
        kind: Kind::DropOverride,
        auto: BotAction::DropOverride,
        section: SectionKind::Droppable,
        wanted: actionable,
        action: Some(Action::Drop),
        take: usize::MAX,
        title: "chore(deps): drop overrides merged upstream",
        intro: "Drops the overrides merged upstream (`odx drop`, then `odx lock`):",
    },
    Routine {
        kind: Kind::Repin,
        auto: BotAction::Repin,
        section: SectionKind::Pinned,
        wanted: moved,
        action: None,
        take: usize::MAX,
        title: "chore(deps): take new pull request heads",
        intro: "Takes the new heads the guard rail allows (`odx lock --update`) of:",
    },
    Routine {
        kind: Kind::AutoUpdate,
        auto: BotAction::AutoUpdate,
        section: SectionKind::AutoUpdate,
        wanted: actionable,
        action: Some(Action::Follow),
        take: 1,
        title: "chore(deps): index updates under the auto-update policy",
        intro: "Takes the index releases the auto-update policy allows (`odx lock --auto-update`):",
    },
];

impl Job<'_> {
    /// Act on the boxes ticked on the dashboard issue `issue` (only plan
    /// them, unless `apply`).
    fn all(&self, work: Work<'_>, issue: &Issue, requests: Vec<Request>, acted: &mut Acted) {
        let Work {
            dashboard,
            apply,
            pulls,
            budget,
            out,
        } = work;
        for request in requests {
            let outcome = if apply {
                self.run(dashboard, &request, issue, pulls, budget)
            } else {
                Outcome::Planned
            };
            let Request { id, action, .. } = request;
            track(&outcome, pulls);
            match &outcome {
                Outcome::Recorded(text) => acted.recorded.push((id.clone(), action, text.clone())),
                Outcome::Proposed(..) => {}
                _ => acted.pending.push((id.clone(), action)),
            }
            if matches!(outcome, Outcome::Deferred) {
                acted.deferred.push(format!("{} `{id}`", action.name()));
            }
            let _ = writeln!(
                out,
                "{}: {} {id}: {}",
                dashboard.series,
                action.name(),
                outcome.text()
            );
            acted
                .done
                .push(json!({"id": id, "action": action.name(), "result": outcome.json()}));
        }
    }

    /// Act on one ticked box: its pull request opened (within `budget`)
    /// or updated.
    fn run(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        issue: &Issue,
        pulls: &[Issue],
        budget: &mut usize,
    ) -> Outcome {
        self.propose(&request.marker, pulls, budget, || {
            if request.action == Action::Promote {
                self.promotion(dashboard, request, issue)
            } else {
                self.change(dashboard, request, issue)
            }
        })
    }

    /// What `write` wrote, proposed as the pull request of `marker`: its
    /// open one updated, else one opened within `budget`.
    fn propose(
        &self,
        marker: &Marker,
        pulls: &[Issue],
        budget: &mut usize,
        write: impl FnOnce() -> Result<Written, Report>,
    ) -> Outcome {
        let open = match earlier(pulls, marker) {
            Earlier::Open(pull) => Some(pull),
            Earlier::Closed(_) | Earlier::None => None,
        };
        if open.is_none() && *budget == 0 {
            return Outcome::Deferred;
        }
        let result = write().and_then(|written| match written {
            Written::Nothing(text) => Ok(Outcome::Recorded(text)),
            Written::Branch { base, title, body } => {
                self.pull_request(&base, &title, &body, marker, open)
            }
        });
        spend(result, budget)
    }

    /// The description of a box's pull request.
    fn body(request: &Request, issue: &Issue, details: &str) -> String {
        format!(
            "{}\nAsked for on the dashboard (#{}): `{} {}`.\n\n```\n{details}```\n\nOpened by `odx bot`; to ask anew after closing it, tick the box again.\n",
            request.marker.render(),
            issue.number,
            request.action.name(),
            request.id,
        )
    }

    /// Push `commit` of the project to `branch`.
    fn push(&self, commit: &str, branch: &str) -> Result<(), Report> {
        let refspec = format!("+{commit}:refs/heads/{branch}");
        super::run_checked(
            &git_in(&self.prepared.project.root).args(["push", "--quiet", self.remote, &refspec]),
            "git push",
        )
        .map_err(failed)
    }

    /// `work` in a worktree of the base on the local `branch`, the project
    /// there as its context.
    fn in_worktree<T>(
        &self,
        branch: &str,
        name: &str,
        work: impl FnOnce(&Context<'_>, &Path) -> Result<T, Report>,
    ) -> Result<T, Report> {
        let root = self.prepared.project.root.as_path();
        let tracking = format!("refs/remotes/{}/{}", self.remote, self.base);
        // CI checks the base out with its remote; locally it may only be a
        // branch.
        let start = git(root, &["rev-parse", "--verify", "--quiet", &tracking])
            .map_or_else(|_| self.base.to_owned(), |_| tracking.clone());
        worktree(root, branch, &start, &format!("bot-{name}")).and_then(|worktree| {
            let local = Context {
                fs: self.ctx.fs,
                env: self.ctx.env,
                dir: worktree.dir.clone(),
                config: None,
            };
            work(&local, &worktree.dir)
        })
    }

    /// The box's action taken in a worktree of the base, as `odx
    /// dashboard act` takes it, and committed; a project that did not
    /// change (a mute in the verdict repository) has nothing to propose.
    fn change(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        issue: &Issue,
    ) -> Result<Written, Report> {
        let Request { id, action, marker } = request;
        let branch = marker.branch();
        self.in_worktree(&branch, &marker.key, |local, wt| {
            let verb = if *action == Action::Mute {
                "muted"
            } else {
                "asked for"
            };
            let act = self.act_args(
                id,
                *action,
                format!("{verb} from the dashboard (#{})", issue.number),
            );
            // A glue addon is ignored in the project's own configuration,
            // as any other change.
            let done = if *action == Action::Mute && !id.starts_with("glue:") {
                self.mute(&act, wt)
            } else {
                prepare(self.global, local).and_then(|prepared| {
                    run_act(
                        &act,
                        self.dashboards,
                        self.specs,
                        self.global,
                        local,
                        &prepared,
                    )
                    .and_then(checked)
                })
            };
            let locked = done.and_then(|done| {
                if matches!(action, Action::Include | Action::Drop) {
                    checked(run_lock(&lock_args(), self.global, local)).map(|_| done)
                } else {
                    Ok(done)
                }
            });
            locked.and_then(|done| {
                let title = title(*action, id, dashboard);
                let body = Self::body(request, issue, &done.out);
                // Nothing staged: the change lives outside the project (a
                // mute in the verdict repository). A failing commit is an
                // error, so the box stays asked for.
                if staged(wt).is_empty() {
                    return Ok(Written::Nothing(done.out.trim_end().to_owned()));
                }
                commit(wt, &title, &body, 0, self.base)
                    .and_then(|head| self.push(&head, &branch))
                    .map(|()| Written::Branch {
                        base: self.base.to_owned(),
                        title,
                        body,
                    })
            })
        })
    }

    /// `odx dashboard act` for `id`, a mute expiring as
    /// `[tool.odx.bot].mute_expiry` says.
    fn act_args(&self, id: &str, action: Action, reason: String) -> DashboardActArgs {
        let until = self
            .prepared
            .project
            .config
            .bot
            .as_ref()
            .and_then(|bot| bot.mute_expiry)
            .map_or(Duration::from_secs(MUTE_DAYS * 86_400), Span::as_duration);
        DashboardActArgs {
            id: id.to_owned(),
            include: action == Action::Include,
            port: false,
            open_pr: false,
            mute: action == Action::Mute,
            drop: action == Action::Drop,
            follow: action == Action::Follow,
            promote: false,
            reason: Some(reason),
            kind: VerdictKind::NotWanted,
            until: Some(date(SystemTime::now() + until)),
        }
    }

    /// A mute recorded from the project itself, where the suggestion it is
    /// about can be found again (a worktree has no aggregated
    /// repositories); what it wrote to the project's own mutes moves to
    /// the worktree `wt`, and the project is left as it was.
    fn mute(&self, act: &DashboardActArgs, wt: &Path) -> Result<Report, Report> {
        let file = Path::new(".odx").join("mutes.toml");
        let ours = self.prepared.project.root.join(&file);
        let before = std::fs::read(&ours).ok();
        let done = run_act(
            act,
            self.dashboards,
            self.specs,
            self.global,
            self.ctx,
            self.prepared,
        );
        let after = std::fs::read(&ours).ok();
        if after != before {
            // What cannot be moved is not proposed, and shows as nothing
            // to propose.
            let _ = std::fs::create_dir_all(wt.join(".odx"))
                .and_then(|()| std::fs::write(wt.join(&file), after.unwrap_or_default()));
            let _ = before.map_or_else(
                || std::fs::remove_file(&ours),
                |before| std::fs::write(&ours, before),
            );
        }
        done.and_then(checked)
    }

    /// A drift entry's addon promoted as `odx promote --commit` writes it,
    /// proposed into the next environment's branch.
    fn promotion(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        issue: &Issue,
    ) -> Result<Written, Report> {
        // `drift:<from>-><to>:<kind>/<name>`, as the dashboard writes it.
        let rest = request.id.strip_prefix("drift:").unwrap_or_default();
        let (from, after) = rest.split_once("->").unwrap_or_default();
        let to = after.split_once(':').unwrap_or_default().0;
        let modules: Vec<String> = dashboard
            .entry(&request.id)
            .and_then(|entry| entry.addon.clone())
            .into_iter()
            .collect();
        let args = PromoteArgs {
            modules: modules.clone(),
            ..promote_args(from, to, self.remote)
        };
        self.promote(&args, &request.marker.branch(), |base, items| {
            Written::Branch {
                base,
                title: format!("chore: promote {} from {from} to {to}", modules.join(", ")),
                body: Self::body(request, issue, items),
            }
        })
    }

    /// What the merged pull request `number` changed in `from`, promoted
    /// into `to`.
    fn merged_promotion(
        &self,
        number: u64,
        [from, to]: [&str; 2],
        marker: &Marker,
    ) -> Result<Written, Report> {
        let args = PromoteArgs {
            from_pr: Some(number),
            ..promote_args(from, to, self.remote)
        };
        self.promote(&args, &marker.branch(), |base, items| Written::Branch {
            base,
            title: format!("chore: promote #{number} from {from} to {to}"),
            body: format!(
                "{}\nPromotes what #{number} merged into {from}: it carries the promotion label.\n\n```\n{items}```\n\nOpened by `odx bot`; closed, it is not proposed again.\n",
                marker.render()
            ),
        })
    }

    /// `args` written as `odx promote --commit` writes them and pushed to
    /// `branch`, then `written` of the branch to propose it into and what it
    /// promotes; nothing when the target has it all already.
    fn promote(
        &self,
        args: &PromoteArgs,
        branch: &str,
        written: impl FnOnce(String, &str) -> Written,
    ) -> Result<Written, Report> {
        let json = GlobalArgs {
            json: true,
            ..self.global.clone()
        };
        checked(super::promote::run_promote(args, &json, self.ctx)).and_then(|report| {
            let value: Value = serde_json::from_str(&report.out).unwrap_or_default();
            let text = |pointer: &str| {
                value
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let items = value
                .get("items")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .fold(String::new(), |mut items, item| {
                    let _ = writeln!(
                        items,
                        "{} {}",
                        text_of(item, "addon"),
                        text_of(item, "change")
                    );
                    items
                });
            // A promotion with nothing to write commits nothing, and an
            // empty commit would push a deletion of the branch.
            let commit = text("/applied/commit");
            if commit.is_empty() {
                return Ok(Written::Nothing(format!(
                    "nothing to promote: {} has it already",
                    text("/to/env")
                )));
            }
            self.push(&commit, branch)
                .map(|()| written(text("/to/branch"), &items))
        })
    }

    /// The promotions asked for by labelling merged pull requests into the
    /// base (`[tool.odx.bot].promote_label`), one into each environment the
    /// base's promotes into, each proposed once.
    fn promotions(
        &self,
        apply: bool,
        pulls: &mut Vec<Issue>,
        budget: &mut usize,
        out: &mut String,
    ) -> Result<Vec<Value>, Report> {
        let config = &self.prepared.project.config;
        let label = config
            .bot
            .as_ref()
            .and_then(|bot| bot.promote_label.clone())
            .unwrap_or_else(|| PROMOTE_LABEL.to_owned());
        let model = Model::resolve(config).ok().flatten();
        let edges: Vec<[String; 2]> = model
            .iter()
            .flat_map(|model| {
                model
                    .of_branch(self.base, None)
                    .into_iter()
                    .flat_map(|from| {
                        model
                            .targets(&from.name)
                            .into_iter()
                            .map(|to| [from.name.clone(), to.to_owned()])
                    })
            })
            .collect();
        let Forge { client, repo, kind } = self.forge;
        let labelled = client
            .labelled(repo, *kind, &label, true)
            .map_err(forge_failure)?;
        let mut done = Vec::new();
        for pull in labelled
            .iter()
            .filter(|pull| pull.state == IssueState::Closed)
        {
            for [from, to] in &edges {
                let marker = Marker::of_merged(pull.number, to);
                if earlier(pulls, &marker) != Earlier::None {
                    continue;
                }
                let outcome = if apply {
                    self.propose(&marker, pulls, budget, || {
                        self.merged_promotion(pull.number, [from, to], &marker)
                    })
                } else {
                    Outcome::Planned
                };
                track(&outcome, pulls);
                let _ = writeln!(
                    out,
                    "promote #{} ({from} -> {to}): {}",
                    pull.number,
                    outcome.text()
                );
                done.push(json!({
                    "pull_request": pull.number,
                    "from": from,
                    "to": to,
                    "result": outcome.json(),
                }));
            }
        }
        Ok(done)
    }

    /// Propose the routine changes `[tool.odx.bot].auto` lists (all of
    /// them by default) that the dashboard's sections found.
    fn routines(&self, work: Work<'_>, acted: &mut Acted) {
        let Work {
            dashboard,
            apply,
            pulls,
            budget,
            out,
        } = work;
        let auto = self
            .prepared
            .project
            .config
            .bot
            .as_ref()
            .and_then(|bot| bot.auto.clone())
            .unwrap_or_else(|| ROUTINES.iter().map(|routine| routine.auto).collect());
        for routine in ROUTINES
            .iter()
            .filter(|routine| auto.contains(&routine.auto))
        {
            let ids: Vec<String> = dashboard
                .sections
                .iter()
                .filter(|section| section.kind == routine.section)
                .flat_map(|section| &section.entries)
                .filter(|entry| (routine.wanted)(entry))
                .map(|entry| entry.id.clone())
                .collect();
            if ids.is_empty() {
                continue;
            }
            let outcome = if apply {
                self.routine(routine, &ids, pulls, budget)
            } else {
                Outcome::Planned
            };
            track(&outcome, pulls);
            let name = routine.kind.name();
            let listed = ids.join(", ");
            if matches!(outcome, Outcome::Deferred) {
                acted.deferred.push(format!("{name} ({listed})"));
            }
            let _ = writeln!(
                out,
                "{}: {name} ({listed}): {}",
                dashboard.series,
                outcome.text()
            );
            acted
                .routine
                .push(json!({"kind": name, "entries": ids, "result": outcome.json()}));
        }
    }

    /// One routine change for `ids`, in a worktree of the base: its open
    /// pull request updated (when the change differs), else one opened
    /// within `budget`, unless a person closed the same change.
    fn routine(
        &self,
        routine: &Routine,
        ids: &[String],
        pulls: &[Issue],
        budget: &mut usize,
    ) -> Outcome {
        let open = match earlier(pulls, &Marker::of_change(routine.kind, "")) {
            Earlier::Open(pull) => Some(pull),
            Earlier::Closed(_) | Earlier::None => None,
        };
        if open.is_none() && *budget == 0 {
            return Outcome::Deferred;
        }
        let name = routine.kind.name();
        let result = self.in_worktree(&format!("odx/{name}/next"), name, |local, wt| {
            self.routine_work(routine, ids, local, wt).and_then(|out| {
                let change = staged(wt);
                if change.is_empty() {
                    return Ok(nothing(&out));
                }
                let marker = Marker::of_change(routine.kind, &change);
                match earlier(pulls, &marker) {
                    Earlier::Open(pull)
                        if Marker::parse(&pull.body).is_some_and(|m| m.key == marker.key) =>
                    {
                        return Ok(Outcome::Unchanged(pull.clone()));
                    }
                    Earlier::Closed(pull) => return Ok(Outcome::Closed(pull.clone())),
                    Earlier::Open(_) | Earlier::None => {}
                }
                // An open pull request keeps its branch.
                let branch = open
                    .and_then(|pull| Marker::parse(&pull.body))
                    .map_or_else(|| marker.branch(), |earlier| earlier.branch());
                let mut body = format!("{}\n{}\n\n", marker.render(), routine.intro);
                for id in ids {
                    let _ = writeln!(body, "- `{id}` {}", covering(id));
                }
                let _ = write!(
                    body,
                    "\n```\n{out}```\n\nOpened by `odx bot`, which updates it while it is open; closed, the same change is not proposed again.\n"
                );
                commit(wt, routine.title, &body, 0, self.base)
                    .and_then(|head| self.push(&head, &branch))
                    .and_then(|()| self.pull_request(self.base, routine.title, &body, &marker, open))
            })
        });
        spend(result, budget)
    }

    /// What a routine change does in the worktree `wt`, then `odx lock`:
    /// its report.
    fn routine_work(
        &self,
        routine: &Routine,
        ids: &[String],
        local: &Context<'_>,
        wt: &Path,
    ) -> Result<String, Report> {
        prepare(self.global, local).and_then(|prepared| {
            routine.action.map_or_else(
                || self.repin(&prepared, local, wt),
                |action| {
                    ids.iter()
                        .take(routine.take)
                        .try_fold(String::new(), |out, id| {
                            let act = self.act_args(id, action, "merged upstream".to_owned());
                            run_act(
                                &act,
                                self.dashboards,
                                self.specs,
                                self.global,
                                local,
                                &prepared,
                            )
                            .and_then(checked)
                            .map(|report| out + &report.out)
                        })
                        .and_then(|out| {
                            checked(run_lock(&lock_args(), self.global, local))
                                .map(|report| out + &report.out)
                        })
                },
            )
        })
    }

    /// `odx lock --update` in the worktree `wt`, kept only when it takes a
    /// new head; the heads the guard rail kept go to the mirror with
    /// `[tool.odx.bot].vault_heads`.
    fn repin(&self, before: &Prepared, local: &Context<'_>, wt: &Path) -> Result<String, Report> {
        let update = LockArgs {
            update: true,
            ..lock_args()
        };
        let config = &self.prepared.project.config;
        let vaulting = config.bot.as_ref().and_then(|bot| bot.vault_heads) == Some(true);
        checked(run_lock(&update, self.global, local)).and_then(|locked| {
            prepare(self.global, local).map(|after| {
                let vaulted = config
                    .mirror
                    .as_ref()
                    .filter(|_| vaulting)
                    .map(|mirror| {
                        let (done, of) = vault_heads(self.ctx, &after, mirror);
                        format!("vaulted {done} of {of} observed heads on the mirror\n")
                    })
                    .unwrap_or_default();
                if heads(before.lock.as_ref()) == heads(after.lock.as_ref()) {
                    let _ = git(wt, &["reset", "--quiet", "--hard"]);
                    vaulted
                } else {
                    locked.out + &vaulted
                }
            })
        })
    }

    /// Open the pull request of the marker's branch into `base`, or update
    /// `open`, with its labels.
    fn pull_request(
        &self,
        base: &str,
        title: &str,
        body: &str,
        marker: &Marker,
        open: Option<&Issue>,
    ) -> Result<Outcome, Report> {
        let Forge { client, repo, kind } = self.forge;
        if let Some(open) = open {
            let edit = IssueEdit {
                title: Some(title),
                body: Some(body),
                state: None,
            };
            return client
                .edit_issue(repo, *kind, open.number, &edit)
                .map(|edited| {
                    Outcome::Proposed(
                        Issue {
                            pull: true,
                            ..edited
                        },
                        false,
                    )
                })
                .map_err(forge_failure);
        }
        let branch = marker.branch();
        let proposal = Proposal {
            head: repo,
            branch: &branch,
            base,
            title,
            body,
            draft: false,
        };
        let mut labels = vec![pr::LABEL.to_owned(), marker.kind.label()];
        labels.extend(
            self.prepared
                .project
                .config
                .bot
                .as_ref()
                .and_then(|bot| bot.labels.clone())
                .unwrap_or_default(),
        );
        let names: Vec<&str> = labels.iter().map(String::as_str).collect();
        client
            .open_pull_request(repo, *kind, &proposal)
            .and_then(|opened| {
                client
                    .add_labels(repo, *kind, opened.number, &names)
                    .map(|()| opened)
            })
            .map(|opened| {
                Outcome::Proposed(
                    Issue {
                        number: opened.number,
                        title: title.to_owned(),
                        body: body.to_owned(),
                        state: IssueState::Open,
                        labels,
                        url: opened.url,
                        pull: true,
                    },
                    true,
                )
            })
            .map_err(forge_failure)
    }
}

/// The outcome of proposing something, a pull request opened counted
/// against `budget`.
fn spend(result: Result<Outcome, Report>, budget: &mut usize) -> Outcome {
    match result {
        Ok(Outcome::Proposed(pull, true)) => {
            *budget = budget.saturating_sub(1);
            Outcome::Proposed(pull, true)
        }
        Ok(outcome) => outcome,
        Err(report) => Outcome::Failed(report.error.unwrap_or_default()),
    }
}

/// `pulls`, with the pull request `outcome` proposed as it is now.
fn track(outcome: &Outcome, pulls: &mut Vec<Issue>) {
    if let Outcome::Proposed(pull, _) = outcome {
        pulls.retain(|other| other.number != pull.number);
        pulls.push(pull.clone());
    }
}

/// A routine change that changed nothing, and what was done besides.
fn nothing(out: &str) -> Outcome {
    let besides = out.lines().fold(String::new(), |mut besides, line| {
        let _ = write!(besides, "; {line}");
        besides
    });
    Outcome::Recorded(format!("nothing to propose{besides}"))
}

/// What is staged in the worktree `wt` after staging everything: the lines
/// added and removed, which key a routine change.
fn staged(wt: &Path) -> String {
    let _ = git(wt, &["add", "-A"]);
    git(wt, &["diff", "--cached", "--unified=0", "--no-color"])
        .unwrap_or_default()
        .lines()
        .filter(|line| {
            (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
        })
        .fold(String::new(), |mut change, line| {
            let _ = writeln!(change, "{line}");
            change
        })
}

/// The commits `lock` takes from pull requests.
fn heads(lock: Option<&Lockfile>) -> Vec<String> {
    lock.into_iter()
        .flat_map(|lock| &lock.repositories)
        .flat_map(|repository| &repository.merges)
        .map(|merge| merge.sha.clone())
        .collect()
}

/// `odx promote --from <from> --to <to> --with-code --commit --yes`, of
/// nothing yet.
fn promote_args(from: &str, to: &str, remote: &str) -> PromoteArgs {
    PromoteArgs {
        modules: Vec::new(),
        from_pr: None,
        rollback: None,
        feature: None,
        search: None,
        select: Vec::new(),
        from: Some(from.to_owned()),
        to: to.to_owned(),
        remote: remote.to_owned(),
        allow_downgrade: false,
        allow_source_change: false,
        no_with_odoo: false,
        with_code: true,
        allow_transitive: false,
        include_branch_specific: Vec::new(),
        commit: true,
        open_pr: false,
        yes: true,
    }
}

/// The string at `key` of `value`, or nothing.
fn text_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// git in `dir`.
fn git_in(dir: &Path) -> odx_core::Invocation {
    odx_core::Invocation::new("git").arg("-C").arg(dir)
}

/// `report` when it succeeded, else its error.
fn checked(report: Report) -> Result<Report, Report> {
    if report.exit == Exit::Success {
        Ok(report)
    } else {
        Err(report)
    }
}

/// `odx lock` after the project changed.
fn lock_args() -> LockArgs {
    LockArgs {
        repo: None,
        migrate: false,
        check: false,
        update: false,
        follow: Vec::new(),
        bundle: None,
        no_uv: false,
        no_push: false,
        auto_update: false,
    }
}

/// The commit title of a box's pull request.
fn title(action: Action, id: &str, dashboard: &Dashboard) -> String {
    let subject = dashboard
        .entry(id)
        .and_then(|entry| entry.addon.clone())
        .map_or_else(|| id.to_owned(), |addon| format!("{addon} ({id})"));
    let verb = [
        (Action::Include, "chore(deps): add"),
        (Action::Drop, "chore(deps): drop"),
        (Action::Follow, "chore(deps): take"),
    ]
    .iter()
    .find(|(known, _)| *known == action)
    .map_or("chore: mute", |(_, verb)| verb);
    format!("{verb} {subject}")
}
