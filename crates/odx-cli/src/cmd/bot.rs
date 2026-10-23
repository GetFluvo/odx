//! `odx bot`: the dashboard as an issue of the project repository, and the
//! boxes ticked there acted on as pull requests (or mutes), each run
//! picking up what the last one left.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use odx_aggregate::date;
use odx_bot::issue::{self, Page, Request};
use odx_bot::pr::{self, Earlier, Marker, earlier};
use odx_config::Span;
use odx_config::environments::Model;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_dashboard::{Action, Dashboard};
use odx_forge::{
    CachedHttp, ForgeClient, ForgeError, ForgeKind, Http, Issue, IssueEdit, IssueState, Proposal,
    RepoUrl, ReqwestHttp, Visibility,
};
use serde_json::{Value, json};

use super::Report;
use super::aggregate::{Prepared, prepare, run_lock};
use super::checkout::git;
use super::dashboard::{Specs, build, run_act};
use super::promote_apply::{commit, failed, worktree};
use super::propose::forge_repo;
use crate::Context;
use crate::cli::{
    BotArgs, BotCommand, DashboardActArgs, DashboardArgs, DashboardFormat, GlobalArgs, LockArgs,
    PromoteArgs, VerdictKind,
};

/// Pull requests a run opens at most, without `--max-prs` or
/// `[tool.odx.bot].max_prs_per_run`.
const MAX_PRS: usize = 10;

/// Mutes from the dashboard expire after this many days, without
/// `[tool.odx.bot].mute_expiry`.
const MUTE_DAYS: u64 = 180;

/// `odx bot`.
pub(crate) fn run_bot(command: &BotCommand, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    let (bot, act, max_prs, plan) = match command {
        BotCommand::Run { bot, max_prs } | BotCommand::Act { bot, max_prs, .. } => {
            (bot.clone(), true, *max_prs, false)
        }
        BotCommand::Dashboard { bot } => (bot.clone(), false, None, false),
        BotCommand::Plan { series, remote } => (
            BotArgs {
                apply: false,
                series: series.clone(),
                remote: remote.clone(),
                reopen: false,
            },
            true,
            None,
            true,
        ),
    };
    let global = GlobalArgs {
        json: global.json || plan,
        fresh: true,
        ..global.clone()
    };
    cycle(&bot, act, max_prs, &global, ctx).unwrap_or_else(|report| report)
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

/// What became of one ticked box.
#[derive(Debug)]
enum Outcome {
    /// A dry run: what would be done.
    Planned,
    /// Left for the next run (`--max-prs`).
    Deferred,
    /// Its pull request, opened or updated.
    Proposed(Issue, bool),
    /// Done without a pull request (a mute in the verdict repository).
    Recorded(String),
    /// It failed; the box stays ticked.
    Failed(String),
}

impl Outcome {
    fn json(&self) -> Value {
        match self {
            Self::Planned => json!({"outcome": "planned"}),
            Self::Deferred => json!({"outcome": "deferred"}),
            Self::Proposed(pull, opened) => json!({
                "outcome": if *opened { "opened" } else { "updated" },
                "pull_request": {"number": pull.number, "url": pull.url},
            }),
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

fn cycle(
    bot: &BotArgs,
    act: bool,
    max_prs: Option<usize>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let sleep = std::thread::sleep;
    ReqwestHttp::new()
        .map(|http| CachedHttp::new(http, prepared.cache.root().join("http")))
        .map_err(forge_failure)
        .and_then(|http| {
            project_forge(ctx, &prepared, &bot.remote, &http, &sleep)
                .and_then(|forge| cycle_on(&forge, bot, act, max_prs, global, ctx, &prepared))
        })
}

/// [`cycle`] on the project repository's forge.
fn cycle_on(
    forge: &Forge<'_>,
    bot: &BotArgs,
    act: bool,
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
        let apply = act && bot.apply;
        let acted = found.map_or_else(Acted::default, |found| {
            let job = Job {
                ctx,
                global,
                prepared,
                forge,
                dashboards: &dashboards,
                specs: &specs,
                base: &base,
                remote: &bot.remote,
                issue: found,
            };
            let requests = issue::requests(&found.body, dashboard);
            job.all(
                dashboard,
                requests,
                apply,
                &mut pulls,
                &mut budget,
                &mut out,
            )
        });
        let page = Page {
            dashboard,
            now: &now,
            run: run.as_deref(),
            pulls: &pulls,
            pending: &acted.pending,
            recorded: &acted.recorded,
            deferred: &[],
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
        }));
    }
    let value = json!({
        "schema": 1,
        "repository": format!("{}/{}", forge.repo.host, forge.repo.path),
        "apply": bot.apply,
        "base": base,
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

/// Everything acting on one box needs.
struct Job<'a> {
    ctx: &'a Context<'a>,
    global: &'a GlobalArgs,
    prepared: &'a Prepared,
    forge: &'a Forge<'a>,
    dashboards: &'a [Dashboard],
    specs: &'a Specs,
    base: &'a str,
    remote: &'a str,
    issue: &'a Issue,
}

/// What acting on a dashboard's boxes did.
#[derive(Default)]
struct Acted {
    /// Each box and what became of it.
    done: Vec<Value>,
    /// Those that stay ticked.
    pending: Vec<(String, Action)>,
    /// Those done without a pull request, and how.
    recorded: Vec<(String, Action, String)>,
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

impl Job<'_> {
    /// Act on the boxes ticked on `dashboard` (only plan them, unless
    /// `apply`), keeping `pulls` current: what became of each, and those
    /// that stay ticked.
    fn all(
        &self,
        dashboard: &Dashboard,
        requests: Vec<Request>,
        apply: bool,
        pulls: &mut Vec<Issue>,
        budget: &mut usize,
        out: &mut String,
    ) -> Acted {
        let mut done = Vec::new();
        let mut pending = Vec::new();
        let mut recorded = Vec::new();
        for request in requests {
            let outcome = if apply {
                self.run(dashboard, &request, pulls, budget)
            } else {
                Outcome::Planned
            };
            let Request { id, action, .. } = request;
            match &outcome {
                Outcome::Proposed(pull, _) => {
                    pulls.retain(|other| other.number != pull.number);
                    pulls.push(pull.clone());
                }
                Outcome::Recorded(text) => recorded.push((id.clone(), action, text.clone())),
                _ => pending.push((id.clone(), action)),
            }
            let _ = writeln!(
                out,
                "{}: {} {id}: {}",
                dashboard.series,
                action.name(),
                outcome.text()
            );
            done.push(json!({"id": id, "action": action.name(), "result": outcome.json()}));
        }
        Acted {
            done,
            pending,
            recorded,
        }
    }

    /// Act on one ticked box: its pull request opened (within `budget`)
    /// or updated.
    fn run(
        &self,
        dashboard: &Dashboard,
        request: &Request,
        pulls: &[Issue],
        budget: &mut usize,
    ) -> Outcome {
        let open = match earlier(pulls, &request.marker) {
            Earlier::Open(pull) => Some(pull),
            Earlier::Closed(_) | Earlier::None => None,
        };
        if open.is_none() && *budget == 0 {
            return Outcome::Deferred;
        }
        let written = if request.action == Action::Promote {
            self.promotion(dashboard, request)
        } else {
            self.change(dashboard, request)
        };
        let result = written.and_then(|written| match written {
            Written::Nothing(text) => Ok(Outcome::Recorded(text)),
            Written::Branch { base, title, body } => {
                self.pull_request(&base, &title, &body, &request.marker, open)
            }
        });
        match result {
            Ok(Outcome::Proposed(pull, true)) => {
                *budget = budget.saturating_sub(1);
                Outcome::Proposed(pull, true)
            }
            Ok(outcome) => outcome,
            Err(report) => Outcome::Failed(report.error.unwrap_or_default()),
        }
    }

    /// The description of a box's pull request.
    fn body(&self, request: &Request, details: &str) -> String {
        format!(
            "{}\nAsked for on the dashboard ({}): `{} {}`.\n\n```\n{details}```\n\nOpened by `odx bot`; to ask anew after closing it, tick the box again.\n",
            request.marker.render(),
            self.issue_ref(),
            request.action.name(),
            request.id,
        )
    }

    /// Push `commit` of the project to the marker's branch.
    fn push(&self, commit: &str, request: &Request) -> Result<(), Report> {
        let refspec = format!("+{commit}:refs/heads/{}", request.marker.branch());
        super::run_checked(
            &git_in(&self.prepared.project.root).args(["push", "--quiet", self.remote, &refspec]),
            "git push",
        )
        .map_err(failed)
    }

    /// The box's action taken in a worktree of the base, as `odx
    /// dashboard act` takes it, and committed; a project that did not
    /// change (a mute in the verdict repository) has nothing to propose.
    fn change(&self, dashboard: &Dashboard, request: &Request) -> Result<Written, Report> {
        let Request { id, action, marker } = request;
        let root = self.prepared.project.root.as_path();
        let tracking = format!("refs/remotes/{}/{}", self.remote, self.base);
        // CI checks the base out with its remote; locally it may only be a
        // branch.
        let start = git(root, &["rev-parse", "--verify", "--quiet", &tracking])
            .map_or_else(|_| self.base.to_owned(), |_| tracking.clone());
        let name = format!("bot-{}", marker.key);
        worktree(root, &marker.branch(), &start, &name).and_then(|worktree| {
            let wt = worktree.dir.as_path();
            let local = Context {
                fs: self.ctx.fs,
                env: self.ctx.env,
                dir: wt.to_path_buf(),
                config: None,
            };
            let act = self.act_args(request);
            let done = if *action == Action::Mute {
                self.mute(&act, wt)
            } else {
                prepare(self.global, &local).and_then(|prepared| {
                    run_act(
                        &act,
                        self.dashboards,
                        self.specs,
                        self.global,
                        &local,
                        &prepared,
                    )
                    .and_then(checked)
                })
            };
            let locked = done.and_then(|done| {
                if matches!(action, Action::Include | Action::Drop) {
                    checked(run_lock(&lock_args(), self.global, &local)).map(|_| done)
                } else {
                    Ok(done)
                }
            });
            locked.and_then(|done| {
                let title = title(*action, id, dashboard);
                let body = self.body(request, &done.out);
                let Ok(head) = commit(wt, &title, &body, 0, self.base) else {
                    return Ok(Written::Nothing(done.out.trim_end().to_owned()));
                };
                self.push(&head, request).map(|()| Written::Branch {
                    base: self.base.to_owned(),
                    title,
                    body,
                })
            })
        })
    }

    /// `odx dashboard act` for a box, a mute expiring as
    /// `[tool.odx.bot].mute_expiry` says.
    fn act_args(&self, request: &Request) -> DashboardActArgs {
        let until = self
            .prepared
            .project
            .config
            .bot
            .as_ref()
            .and_then(|bot| bot.mute_expiry)
            .map_or(Duration::from_secs(MUTE_DAYS * 86_400), Span::as_duration);
        let action = request.action;
        DashboardActArgs {
            id: request.id.clone(),
            include: action == Action::Include,
            port: false,
            open_pr: false,
            mute: action == Action::Mute,
            drop: action == Action::Drop,
            follow: action == Action::Follow,
            promote: false,
            reason: Some(format!("muted from the dashboard ({})", self.issue_ref())),
            kind: VerdictKind::NotWanted,
            until: Some(date(SystemTime::now() + until)),
        }
    }

    /// The dashboard issue, for people: `#<n>`.
    fn issue_ref(&self) -> String {
        format!("#{}", self.issue.number)
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
    fn promotion(&self, dashboard: &Dashboard, request: &Request) -> Result<Written, Report> {
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
            from_pr: None,
            rollback: None,
            feature: None,
            search: None,
            select: Vec::new(),
            from: Some(from.to_owned()),
            to: to.to_owned(),
            remote: self.remote.to_owned(),
            allow_downgrade: false,
            allow_source_change: false,
            no_with_odoo: false,
            with_code: true,
            allow_transitive: false,
            include_branch_specific: Vec::new(),
            commit: true,
            open_pr: false,
            yes: true,
        };
        let json = GlobalArgs {
            json: true,
            ..self.global.clone()
        };
        checked(super::promote::run_promote(&args, &json, self.ctx)).and_then(|report| {
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
            self.push(&text("/applied/commit"), request)
                .map(|()| Written::Branch {
                    base: text("/to/branch"),
                    title: format!("chore: promote {} from {from} to {to}", modules.join(", ")),
                    body: self.body(request, &items),
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
