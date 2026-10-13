//! `odx migrate addon <addon> --to <series>`: an addon's migration to
//! another series on a branch of the repository, the OCA way; `odx migrate
//! --continue` and `--abort` after a conflict in its history.

use std::fmt::Write as _;

use odx_config::Problem;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation};
use odx_forge::{ForgeClient, Http, RepoUrl, ReqwestHttp};
use odx_port::migrate::{Outcome, Plan, Summary, Tools, Worktree};

use super::{Report, output_of};
use crate::Context;
use crate::cli::{GlobalArgs, MigrateAddonArgs, MigrateArgs, MigrateCommand};

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
        Outcome::Done(summary) => {
            warnings.extend(summary.warnings.iter().map(|message| Problem {
                key: "migrate".to_owned(),
                message: message.clone(),
            }));
            Report {
                out: if global.json {
                    json(&summary)
                } else {
                    text(&summary)
                },
                warnings,
                ..Report::default()
            }
        }
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
    let remotes = output_of(&git(ctx).arg("remote")).unwrap_or_default();
    let remote = args.remote.clone().unwrap_or_else(|| {
        let upstream = remotes.lines().any(|remote| remote == "upstream");
        if upstream { "upstream" } else { "origin" }.to_owned()
    });
    Ok(Plan {
        addon: args.addon.clone(),
        from,
        to: args.to.clone(),
        remote,
        no_pre_commit: args.no_pre_commit,
    })
}

fn git(ctx: &Context<'_>) -> Invocation {
    Invocation::new("git").arg("-C").arg(ctx.dir.as_os_str())
}

/// Refuse when an open pull request on the remote's forge already migrates
/// the addon: the work goes there instead. A forge that cannot be asked is
/// a warning.
fn migration_pr(ctx: &Context<'_>, plan: &Plan, warnings: &mut Vec<Problem>) -> Result<(), Report> {
    // The URL git uses, or as configured when `insteadOf` turns it into
    // something that is not on a forge.
    let key = format!("remote.{}.url", plan.remote);
    let urls = [
        git(ctx).args(["remote", "get-url", &plan.remote]),
        git(ctx).args(["config", "--get", &key]),
    ];
    let Some(repo) = urls
        .iter()
        .find_map(|query| RepoUrl::parse(&output_of(query).unwrap_or_default()))
    else {
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

fn json(summary: &Summary) -> String {
    #[derive(serde::Serialize)]
    struct Done<'a> {
        status: &'static str,
        #[serde(flatten)]
        summary: &'a Summary,
    }
    let done = Done {
        status: "done",
        summary,
    };
    format!("{}\n", serde_json::to_string(&done).unwrap_or_default())
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
}
