//! `odx migrate addon <addon> --to <series>`: an addon's migration to
//! another series on a branch of the repository, the OCA way; `odx migrate
//! --continue` and `--abort` after a conflict in its history; `odx migrate
//! pr` to push the branch and open its pull request.

use std::fmt::Write as _;

use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, Http, ReqwestHttp};
use odx_port::migrate::{Finding, Outcome, Plan, Summary, Tools, Worktree};

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

/// The series before `series`: `18.0` for `19.0`.
fn previous(series: &str) -> Option<String> {
    let (major, _) = series.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    major.checked_sub(1).map(|before| format!("{before}.0"))
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
            let out = if global.json {
                format!(
                    "{}\n",
                    serde_json::json!({"status": "aborted", "branch": state.plan.branch()})
                )
            } else {
                format!(
                    "aborted the migration of {}: deleted {}, back on {}\n",
                    state.plan.addon,
                    state.plan.branch(),
                    state.previous
                )
            };
            return Ok(Report {
                out,
                ..Report::default()
            });
        }
        (_, true, _) => tree.resume(&tools),
        (Some(MigrateCommand::Plan { to }), _, _) => {
            return super::upgrade::migrate_plan(to.as_deref(), global, ctx);
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
        json(summary, opened.as_ref())
    } else {
        text(summary)
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
    Ok(Plan {
        addon: args.addon.clone(),
        from,
        to: args.to.clone(),
        remote,
        no_pre_commit: args.no_pre_commit,
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

fn text(summary: &Summary) -> String {
    let mut out = format!(
        "migrated on branch {}: {} commit(s) of history\n",
        summary.branch, summary.history
    );
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
    out.push_str(
        "next: fix what is left, run the tests, then push the branch and open the pull request\n",
    );
    out
}

fn json(summary: &Summary, opened: Option<&Pushed>) -> String {
    #[derive(serde::Serialize)]
    struct Done<'a> {
        status: &'static str,
        #[serde(flatten)]
        summary: &'a Summary,
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_request: Option<serde_json::Value>,
    }
    let done = Done {
        status: "done",
        summary,
        pull_request: opened.map(opened_json),
    };
    format!("{}\n", serde_json::to_string(&done).unwrap_or_default())
}

/// The pull request's description.
fn body(addon: &str, from: &str, to: &str, findings: &[Finding]) -> String {
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
    let _ = write!(
        body,
        "\n<!-- odx:migrate addon {addon} --to {to} --from {from} -->\n"
    );
    body
}

/// Push the migration branch checked out and open its pull request.
fn open_pr(ctx: &Context<'_>, tools: &Tools, args: &MigratePrArgs) -> Result<Pushed, Report> {
    let branch = output_of(&git(ctx).args(["branch", "--show-current"])).unwrap_or_default();
    let Some((to, addon)) = branch.split_once("-mig-") else {
        return Err(Report::error(
            Exit::Usage,
            format!("{branch:?} is no migration branch (`<series>-mig-<addon>`)"),
        ));
    };
    let config = |key: &str| {
        output_of(&git(ctx).args(["config", "--get", &format!("branch.{branch}.{key}")])).ok()
    };
    let from = config("odxMigrateFrom")
        .or_else(|| previous(to))
        .unwrap_or_default();
    let remote = config("odxMigrateRemote").unwrap_or_else(|| default_remote(ctx));
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
            body: body(addon, &from, to, &findings),
        },
    )
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
    fn descriptions_list_the_work_left() {
        let finding = Finding {
            file: "pos_x/models/a.py".to_owned(),
            line: 3,
            code: "U1908".to_owned(),
            message: "use `Domain`".to_owned(),
        };
        assert_eq!(
            body("pos_x", "18.0", "19.0", &[finding]),
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
