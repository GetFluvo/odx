//! `odx migrate addon <addon> --to <series>`: an addon's migration to
//! another series on a branch of the repository, the OCA way; `odx migrate
//! --continue` and `--abort` after a conflict in its history; `odx migrate
//! pr` to push the branch and open its pull request.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, Http, ReqwestHttp};
use odx_port::migrate::{Finding, Outcome, Plan, State, Summary, Tools, Worktree};
use odx_port::squash::Rules;

use super::deps::Waiting;
use super::pin::{self, previous};
use super::propose::{
    Proposed, Pushed, default_remote, forge_repo, git, opened_json, opened_text, propose,
};
use super::{Report, output_of};
use crate::Context;
use crate::cli::{GlobalArgs, MigrateAddonArgs, MigrateArgs, MigrateCommand, MigratePrArgs};

/// Run `odx migrate`.
pub(crate) fn run_migrate(args: &MigrateArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    migrate(args, global, ctx).unwrap_or_else(|report| report)
}

/// What `odx migrate --abort` says it did.
fn aborted(state: &State, json: bool) -> String {
    if json {
        format!(
            "{}\n",
            serde_json::json!({"status": "aborted", "branch": state.plan.branch()})
        )
    } else if state.update {
        format!(
            "aborted the update of {}: back where it was\n",
            state.plan.branch()
        )
    } else {
        format!(
            "aborted the migration of {}: deleted {}, back on {}\n",
            state.plan.addon,
            state.plan.branch(),
            state.previous
        )
    }
}

fn migrate(args: &MigrateArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let tree = Worktree::new(&OsRunner, &ctx.dir);
    let program =
        |var: &str, default: &str| ctx.env.var(var).as_deref().unwrap_or(default).to_owned();
    let tools = Tools {
        odl: program("ODX_ODL", "odl"),
        pre_commit: Some(program("ODX_PRE_COMMIT", "pre-commit")),
    };
    let mut warnings = Vec::new();
    let outcome = match (&args.command, args.resume, args.abort) {
        (_, _, true) => {
            let state = tree.abort().map_err(|e| Report::error(Exit::Failure, e))?;
            return Ok(Report {
                out: aborted(&state, global.json),
                ..Report::default()
            });
        }
        (_, true, _) => tree.resume(&tools),
        (Some(MigrateCommand::Own { to, no_pre_commit }), _, _) => {
            let tools = Tools {
                pre_commit: tools.pre_commit.filter(|_| !no_pre_commit),
                ..tools
            };
            return own(to.as_deref(), &tools, global, ctx);
        }
        (Some(MigrateCommand::Plan { to, from, remote }), _, _) => {
            if super::in_project(global, ctx) {
                return super::upgrade::migrate_plan(to.as_deref(), global, ctx);
            }
            let Some(to) = to else {
                return Err(Report::error(
                    Exit::Usage,
                    "outside an odx project, name the target series with --to".to_owned(),
                ));
            };
            return repository_plan(ctx, global, to, from.as_deref(), remote.as_deref());
        }
        (Some(MigrateCommand::Pin), _, _) => return pin_command(global, ctx),
        (Some(MigrateCommand::Update(update)), _, _) => {
            return super::update::run(update, global, ctx, &tools);
        }
        (Some(MigrateCommand::Pr(pr)), _, _) => {
            let opened = open_pr(ctx, &tools, pr)?;
            return Ok(Report {
                out: if global.json {
                    format!("{}\n", opened_json(&opened))
                } else {
                    opened_text(&opened)
                },
                ..Report::default()
            });
        }
        (Some(MigrateCommand::Addon(addon)), _, _) => {
            let plan = plan(addon, ctx)?;
            if !addon.force && !global.offline {
                migration_pr(ctx, &plan, &mut warnings)?;
            }
            tree.start(plan, &tools)
        }
        (None, _, _) => {
            return Err(Report::error(
                Exit::Usage,
                "name what to migrate: `odx migrate addon <addon> --to <series>`".to_owned(),
            ));
        }
    };
    let outcome = outcome.map_err(|e| Report::error(Exit::Failure, e))?;
    Ok(match outcome {
        Outcome::Done(summary) => finished(args, global, ctx, &tools, &summary, warnings),
        Outcome::Stopped(message) => Report {
            exit: Exit::Failure,
            out: if global.json {
                format!(
                    "{}\n",
                    serde_json::json!({"status": "stopped", "message": message})
                )
            } else {
                String::new()
            },
            warnings,
            error: Some(message),
        },
    })
}

/// The report of a finished migration, after opening its pull request when
/// `--open-pr` asks for it.
fn finished(
    args: &MigrateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    tools: &Tools,
    summary: &Summary,
    mut warnings: Vec<Problem>,
) -> Report {
    warnings.extend(summary.warnings.iter().map(|message| Problem {
        key: "migrate".to_owned(),
        message: message.clone(),
    }));
    // Dependencies the target lacks, pinned the OCA way when asked.
    let branch = pin::current(ctx).ok();
    let waits = branch
        .as_ref()
        .map(|branch| pin::waiting(ctx, global, branch))
        .unwrap_or_default();
    let pinning =
        matches!(&args.command, Some(MigrateCommand::Addon(addon)) if addon.pin_unreleased);
    let pinned = if pinning {
        match pin::pin(ctx, &waits) {
            Ok(lines) => lines,
            Err(report) => return report,
        }
    } else {
        Vec::new()
    };
    let wanted = matches!(&args.command, Some(MigrateCommand::Addon(addon)) if addon.open_pr);
    let pr = wanted.then(|| {
        let defaults = MigratePrArgs {
            push_remote: None,
            ready: false,
        };
        open_pr(ctx, tools, &defaults)
    });
    let (opened, error) = match pr {
        Some(Ok(opened)) => (Some(opened), None),
        Some(Err(report)) => (None, report.error),
        None => (None, None),
    };
    let mut out = if global.json {
        json(summary, &waits, &pinned, opened.as_ref())
    } else if summary.update {
        super::update::text(summary, None)
    } else {
        text(summary, &waits, &pinned)
    };
    if let Some(opened) = opened.as_ref().filter(|_| !global.json) {
        out.push_str(&opened_text(opened));
    }
    Report {
        exit: if error.is_some() {
            Exit::Failure
        } else {
            Exit::Success
        },
        out,
        warnings,
        error,
    }
}

/// `odx migrate pin`.
fn pin_command(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let branch = pin::current(ctx)?;
    let waits = pin::waiting(ctx, global, &branch);
    let pinned = pin::pin(ctx, &waits)?;
    let mut out = String::new();
    if pinned.is_empty() {
        let _ = writeln!(
            out,
            "nothing to pin: no dependency of {} waits on an open migration pull request",
            branch.addon
        );
    } else {
        let _ = writeln!(out, "pinned in a commit `{}`:", pin::SUBJECT);
        for line in &pinned {
            let _ = writeln!(out, "  {line}");
        }
        let _ = writeln!(out, "{}", pin::RED_CHECK);
    }
    Ok(Report {
        out: if global.json {
            format!(
                "{}\n",
                serde_json::json!({"pinned": pinned, "waits_on": waits})
            )
        } else {
            out
        },
        ..Report::default()
    })
}

/// `odx migrate own`.
fn own(
    to: Option<&str>,
    tools: &Tools,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = super::aggregate::prepare(global, ctx)?;
    let to = super::upgrade::target_series(to, &prepared)?;
    let root = &prepared.project.root;
    let dirs: Vec<String> = super::upgrade::addons(ctx, &prepared)
        .into_iter()
        .filter(|addon| addon.kind == super::upgrade::Kind::Own)
        .filter_map(|addon| addon.dir)
        .map(|dir| {
            dir.strip_prefix(root)
                .unwrap_or(&dir)
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    let (done, warnings) = Worktree::new(&OsRunner, root)
        .migrate_in_place(&dirs, &to, tools)
        .map_err(|e| Report::error(Exit::Failure, e))?;
    let out = if global.json {
        format!(
            "{}\n",
            serde_json::json!({"target": to, "addons": done, "warnings": warnings})
        )
    } else {
        let mut out = format!("own addons migrated in place to {to}:\n");
        for addon in &done {
            let what = if addon.committed {
                "[MIG] committed"
            } else {
                "nothing changed"
            };
            let _ = writeln!(
                out,
                "  {}: {what}, {} finding(s) left to fix by hand",
                addon.addon,
                addon.findings.len()
            );
            for finding in &addon.findings {
                let _ = writeln!(
                    out,
                    "    {}:{}: {} {}",
                    finding.file, finding.line, finding.code, finding.message
                );
            }
        }
        out
    };
    Ok(Report {
        out,
        warnings: warnings
            .into_iter()
            .map(|message| Problem {
                key: "migrate".to_owned(),
                message,
            })
            .collect(),
        ..Report::default()
    })
}

/// `odx migrate plan` in a plain clone of an addons repository: the
/// addons on `<remote>/<from>` that `<remote>/<to>` lacks, those with an
/// open migration pull request apart, the rest dependencies first.
fn repository_plan(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    to: &str,
    from: Option<&str>,
    remote: Option<&str>,
) -> Result<Report, Report> {
    let Some(from) = from.map(str::to_owned).or_else(|| previous(to)) else {
        return Err(Report::error(
            Exit::Usage,
            format!("cannot tell the series before {to}; name it with --from"),
        ));
    };
    let remote = remote.map_or_else(|| default_remote(ctx), str::to_owned);
    let refname = |series: &str| format!("refs/remotes/{remote}/{series}");
    let (source, target) = (refname(&from), refname(to));
    Worktree::new(&OsRunner, &ctx.dir)
        .fetch(
            &remote,
            &[
                format!("+refs/heads/{from}:{source}"),
                format!("+refs/heads/{to}:{target}"),
            ],
        )
        .map_err(|e| Report::error(Exit::Failure, e))?;
    let runner = OsRunner;
    let repo = odx_port::git::Repo::new(&runner, &ctx.dir);
    let manifest = |rev: &str, addon: &str| repo.show(rev, &format!("{addon}/__manifest__.py"));
    let listed = output_of(&git(ctx).args(["ls-tree", "--name-only", &source])).unwrap_or_default();
    let (ready, missing): (Vec<&str>, Vec<&str>) = listed
        .lines()
        .filter(|addon| manifest(&source, addon).is_some())
        .partition(|addon| manifest(&target, addon).is_some());
    let mut warnings = Vec::new();
    let open = if global.offline {
        Vec::new()
    } else {
        open_migrations(ctx, &remote, to, &mut warnings)
    };
    let pr_of = |addon: &str| {
        open.iter()
            .find(|(_, title)| title.contains("[MIG]") && title.contains(addon))
            .map(|(number, _)| *number)
    };
    let (prs, todo): (Vec<&str>, Vec<&str>) = missing
        .into_iter()
        .partition(|addon| pr_of(addon).is_some());
    let names: Vec<String> = todo.iter().map(|addon| (*addon).to_owned()).collect();
    let depends = |addon: &str| -> Vec<String> {
        manifest(&source, addon)
            .and_then(|text| odx_analyze::manifest::parse_manifest(&text).ok())
            .map(|parsed| parsed.depends)
            .unwrap_or_default()
    };
    let order = super::upgrade::dependencies_first(&names, &depends);
    // What each addon waits for that the target does not have yet.
    let mut lookup = super::deps::Lookup::new(ctx, global.offline);
    let available =
        |addon: &str| manifest(&target, addon).is_some() || names.iter().any(|name| name == addon);
    let waits: BTreeMap<&String, Vec<Waiting>> = order
        .iter()
        .map(|addon| {
            (
                addon,
                lookup.waiting(&depends(addon), &from, to, &available),
            )
        })
        .filter(|(_, waiting)| !waiting.is_empty())
        .collect();
    let prs: Vec<(&str, u64)> = prs
        .into_iter()
        .map(|addon| (addon, pr_of(addon).unwrap_or_default()))
        .collect();
    let plan = RepositoryPlan {
        remote: &remote,
        from: &from,
        to,
        ready: &ready,
        prs: &prs,
        order: &order,
        waits: &waits,
    };
    Ok(Report {
        out: if global.json {
            plan.json()
        } else {
            plan.text()
        },
        warnings,
        ..Report::default()
    })
}

/// What `odx migrate plan` found in a plain addons repository.
struct RepositoryPlan<'a> {
    remote: &'a str,
    from: &'a str,
    to: &'a str,
    ready: &'a [&'a str],
    prs: &'a [(&'a str, u64)],
    order: &'a [String],
    waits: &'a BTreeMap<&'a String, Vec<Waiting>>,
}

impl RepositoryPlan<'_> {
    fn json(&self) -> String {
        let value = serde_json::json!({
            "source": format!("{}/{}", self.remote, self.from),
            "target": self.to,
            "available": self.ready,
            "pull_requests": self.prs.iter().map(|(addon, number)| serde_json::json!({
                "addon": addon, "number": number,
            })).collect::<Vec<_>>(),
            "to_migrate": self.order,
            "waits_on": self.waits,
        });
        format!("{value}\n")
    }

    fn text(&self) -> String {
        let (remote, from, to) = (self.remote, self.from, self.to);
        let mut out = format!("migration plan {remote}/{from} -> {remote}/{to}\n");
        let _ = writeln!(
            out,
            "already on {to} ({}): {}",
            self.ready.len(),
            self.ready.join(", ")
        );
        for (addon, number) in self.prs {
            let _ = writeln!(
                out,
                "open migration pull request: {addon} #{number}; review, test and contribute there"
            );
        }
        let _ = writeln!(
            out,
            "to migrate, dependencies first ({}):",
            self.order.len()
        );
        for addon in self.order {
            let _ = writeln!(out, "  {addon}");
            for waiting in self.waits.get(addon).into_iter().flatten() {
                let _ = writeln!(out, "    {}", waiting.describe(to));
            }
        }
        let _ = writeln!(
            out,
            "next: `odx migrate addon <addon> --to {to} --from {from} --remote {remote}`"
        );
        out
    }
}

/// The open pull requests targeting `to` on `remote`'s forge, as number
/// and title; a forge that cannot be asked is a warning.
fn open_migrations(
    ctx: &Context<'_>,
    remote: &str,
    to: &str,
    warnings: &mut Vec<Problem>,
) -> Vec<(u64, String)> {
    let Some(repo) = forge_repo(ctx, remote) else {
        return Vec::new();
    };
    let sleep = std::thread::sleep;
    let open = ReqwestHttp::new()
        .and_then(|http| {
            let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &sleep);
            client
                .kind(&repo)
                .and_then(|kind| client.open_pull_requests(&repo, kind, to))
        })
        .map_err(|e| e.to_string());
    match open {
        Ok(prs) => prs.into_iter().map(|pr| (pr.number, pr.title)).collect(),
        Err(e) => {
            warnings.push(Problem {
                key: "migrate".to_owned(),
                message: format!("cannot look for open migration pull requests: {e}"),
            });
            Vec::new()
        }
    }
}

/// What makes a commit administrative: the OCA bots, or the ones
/// `[tool.odx.port]` of a project around the clone names.
pub(crate) fn squash_rules(ctx: &Context<'_>) -> Rules {
    let project = odx_config::find_pyproject(ctx.fs, &ctx.dir).ok();
    let port = project
        .as_ref()
        .and_then(|project| project.config.port.as_ref());
    let defaults = Rules::default();
    Rules {
        authors: port
            .and_then(|port| port.squash_authors.clone())
            .unwrap_or(defaults.authors),
        subjects: port
            .and_then(|port| port.squash_subjects.clone())
            .unwrap_or(defaults.subjects),
    }
}

/// The plan for `args`, with the defaults filled in.
fn plan(args: &MigrateAddonArgs, ctx: &Context<'_>) -> Result<Plan, Report> {
    let Some(from) = args.from.clone().or_else(|| previous(&args.to)) else {
        return Err(Report::error(
            Exit::Usage,
            format!(
                "cannot tell the series before {}; name it with --from",
                args.to
            ),
        ));
    };
    let remote = args.remote.clone().unwrap_or_else(|| default_remote(ctx));
    let squash = squash_rules(ctx);
    Ok(Plan {
        addon: args.addon.clone(),
        from,
        to: args.to.clone(),
        remote,
        no_pre_commit: args.no_pre_commit,
        no_squash: args.no_squash,
        squash,
    })
}

/// Refuse when an open pull request on the remote's forge already migrates
/// the addon: the work goes there instead. A forge that cannot be asked is
/// a warning.
fn migration_pr(ctx: &Context<'_>, plan: &Plan, warnings: &mut Vec<Problem>) -> Result<(), Report> {
    let Some(repo) = forge_repo(ctx, &plan.remote) else {
        return Ok(());
    };
    let sleep = std::thread::sleep;
    let open = ReqwestHttp::new()
        .and_then(|http| {
            let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &sleep);
            client
                .kind(&repo)
                .and_then(|kind| client.open_pull_requests(&repo, kind, &plan.to))
        })
        .map_err(|e| e.to_string());
    match open {
        Ok(prs) => {
            let existing = prs
                .iter()
                .find(|pr| pr.title.contains("[MIG]") && pr.title.contains(&plan.addon));
            if let Some(pr) = existing {
                return Err(Report::error(
                    Exit::Failure,
                    format!(
                        "{}#{} already migrates {} to {}: \"{}\"; review and contribute there (`odx add {}#{}` uses it in a project), or pass --force",
                        repo.path, pr.number, plan.addon, plan.to, pr.title, repo.path, pr.number
                    ),
                ));
            }
        }
        Err(e) => warnings.push(Problem {
            key: "migrate".to_owned(),
            message: format!("cannot look for an open migration pull request: {e}"),
        }),
    }
    Ok(())
}

fn text(summary: &Summary, waits: &[Waiting], pinned: &[String]) -> String {
    let mut out = format!(
        "migrated on branch {} from {}: {} commit(s) of history",
        summary.branch, summary.source, summary.history
    );
    if summary.squashed > 0 {
        let _ = write!(
            out,
            " ({} administrative commit(s) squashed into them)",
            summary.squashed
        );
    }
    out.push('\n');
    for commit in &summary.commits {
        let _ = writeln!(out, "  {commit}");
    }
    if summary.findings.is_empty() {
        out.push_str("odoo-lint finds nothing left to do by hand\n");
    } else {
        let _ = writeln!(
            out,
            "{} finding(s) left to fix by hand:",
            summary.findings.len()
        );
        for finding in &summary.findings {
            let _ = writeln!(
                out,
                "  {}:{}: {} {}",
                finding.file, finding.line, finding.code, finding.message
            );
        }
    }
    out.push_str(&timings(&summary.timings));
    if let Some(waiting) = waits.first() {
        let to = summary.branch.split_once("-mig-").map_or("", |(to, _)| to);
        let _ = writeln!(out, "{} dependencies are not on {to} yet:", waits.len());
        for waiting in waits {
            let _ = writeln!(out, "  {}", waiting.describe(to));
        }
        if pinned.is_empty() && waiting.pull_request.is_some() {
            out.push_str("  `odx migrate pin` pins them to their pull requests in test-requirements.txt, the OCA way\n");
        }
    }
    if !pinned.is_empty() {
        let _ = writeln!(out, "pinned in a commit `{}`:", pin::SUBJECT);
        for line in pinned {
            let _ = writeln!(out, "  {line}");
        }
        let _ = writeln!(out, "{}", pin::RED_CHECK);
    }
    out.push_str(
        "next: fix what is left and run the tests; then `odx migrate pr` pushes the branch and opens a draft pull request\n",
    );
    out
}

/// `took 10.7 s: history 0.4 s, squash 0.2 s, pre-commit 4.7 s, odoo-lint
/// 0.3 s`, with why pre-commit is slow the first time.
pub(crate) fn timings(timings: &[(String, u64)]) -> String {
    let seconds = |ms: u64| format!("{}.{} s", ms / 1000, ms % 1000 / 100);
    let total: u64 = timings.iter().map(|(_, ms)| ms).sum();
    let phases: Vec<String> = timings
        .iter()
        .map(|(phase, ms)| format!("{phase} {}", seconds(*ms)))
        .collect();
    let mut line = format!("took {}: {}\n", seconds(total), phases.join(", "));
    let slow = timings
        .iter()
        .any(|(phase, ms)| phase == "pre-commit" && *ms > 20_000);
    if slow {
        line.push_str("  pre-commit builds its hook environments the first time it meets a configuration; later runs reuse them\n");
    }
    line
}

fn json(
    summary: &Summary,
    waits: &[Waiting],
    pinned: &[String],
    opened: Option<&Pushed>,
) -> String {
    #[derive(serde::Serialize)]
    struct Done<'a> {
        status: &'static str,
        #[serde(flatten)]
        summary: &'a Summary,
        waits_on: &'a [Waiting],
        pinned: &'a [String],
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_request: Option<serde_json::Value>,
    }
    let done = Done {
        status: "done",
        summary,
        waits_on: waits,
        pinned,
        pull_request: opened.map(opened_json),
    };
    format!("{}\n", serde_json::to_string(&done).unwrap_or_default())
}

/// The pull request's description.
fn body(addon: &str, from: &str, to: &str, findings: &[Finding], depends: &[String]) -> String {
    let mut body = format!(
        "Migration of `{addon}` from {from} to {to}, made with odx:\n\n\
         - the history of `{addon}` on {from}, authors kept\n\
         - the repository's pre-commit fixes\n\
         - odoo-lint's upgrade fixes and the version {to}.1.0.0\n\n"
    );
    if findings.is_empty() {
        body.push_str("odoo-lint finds nothing left to fix by hand.\n");
    } else {
        body.push_str("### Left to fix by hand\n\n");
        for finding in findings {
            let _ = writeln!(
                body,
                "- [ ] `{}:{}` {} {}",
                finding.file, finding.line, finding.code, finding.message
            );
        }
    }
    if !depends.is_empty() {
        body.push_str("\nDepends on:\n");
        for page in depends {
            let _ = writeln!(body, "- [ ] {page}");
        }
        let _ = writeln!(body, "\n{}.", pin::RED_CHECK);
    }
    let _ = write!(
        body,
        "\n<!-- odx:migrate addon {addon} --to {to} --from {from} -->\n"
    );
    body
}

/// Push the migration branch checked out and open its pull request.
fn open_pr(ctx: &Context<'_>, tools: &Tools, args: &MigratePrArgs) -> Result<Pushed, Report> {
    let pin::MigrationBranch {
        name: branch,
        to,
        addon,
        from,
        remote,
    } = pin::current(ctx)?;
    let (to, addon) = (to.as_str(), addon.as_str());
    let Some(upstream) = forge_repo(ctx, &remote) else {
        return Err(Report::error(
            Exit::Failure,
            format!("the remote {remote} is not on a forge"),
        ));
    };
    let findings = Worktree::new(&OsRunner, &ctx.dir).findings(addon, to, tools);
    propose(
        ctx,
        &Proposed {
            branch: &branch,
            base: to,
            remote: &remote,
            upstream: &upstream,
            push_remote: args.push_remote.as_deref(),
            draft: !args.ready,
            title: format!("[{to}][MIG] {addon}: Migration to {to}"),
            body: body(addon, &from, to, &findings, &pin::depends_on(ctx)),
        },
    )
    .inspect(|pushed| {
        // Remembered for `odx migrate update --push`.
        pin::remember(ctx, &branch, &pushed.to, &branch);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_before() {
        assert_eq!(previous("19.0").as_deref(), Some("18.0"));
        assert_eq!(previous("0.0"), None);
        assert_eq!(previous("x.0"), None);
        assert_eq!(previous("19"), None);
    }

    #[test]
    fn descriptions_name_what_they_depend_on() {
        let pages = ["https://github.com/OCA/s/pull/3".to_owned()];
        let text = body("pos_x", "18.0", "19.0", &[], &pages);
        assert!(
            text.contains("\nDepends on:\n- [ ] https://github.com/OCA/s/pull/3\n\nOCA CI's check"),
            "{text}"
        );
    }

    #[test]
    fn timing_lines() {
        let phases = |pre_commit: u64| {
            vec![
                ("history".to_owned(), 400),
                ("pre-commit".to_owned(), pre_commit),
            ]
        };
        assert_eq!(
            timings(&phases(4_700)),
            "took 5.1 s: history 0.4 s, pre-commit 4.7 s\n"
        );
        assert!(timings(&phases(64_000)).contains("the first time"));
    }

    #[test]
    fn descriptions_list_the_work_left() {
        let finding = Finding {
            file: "pos_x/models/a.py".to_owned(),
            line: 3,
            code: "U1908".to_owned(),
            message: "use `Domain`".to_owned(),
        };
        assert_eq!(
            body("pos_x", "18.0", "19.0", &[finding], &[]),
            "Migration of `pos_x` from 18.0 to 19.0, made with odx:\n\n\
             - the history of `pos_x` on 18.0, authors kept\n\
             - the repository's pre-commit fixes\n\
             - odoo-lint's upgrade fixes and the version 19.0.1.0.0\n\n\
             ### Left to fix by hand\n\n\
             - [ ] `pos_x/models/a.py:3` U1908 use `Domain`\n\n\
             <!-- odx:migrate addon pos_x --to 19.0 --from 18.0 -->\n"
        );
    }
}
