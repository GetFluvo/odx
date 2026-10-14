//! `odx migrate update [<owner>/<repo>#<n>]`: an open migration brought up
//! to date with what the addon got on the source series since, on the
//! migration branch checked out or on someone else's migration pull
//! request; `--push` sends it back where the pull request comes from.

use std::fmt::Write as _;

use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, Http, ReqwestHttp};
use odx_port::migrate::{Outcome, Plan, Summary, Tools, Worktree};

use super::deps::refs;
use super::migrate::{squash_rules, timings};
use super::pin::{self, MigrationBranch, previous};
use super::port::{parse_pr, remote_for};
use super::propose::{forge_repo, git};
use super::{Report, output_of};
use crate::Context;
use crate::cli::{GlobalArgs, MigrateUpdateArgs};

fn failed(message: String) -> Report {
    Report::error(Exit::Failure, message)
}

fn usage(message: String) -> Report {
    Report::error(Exit::Usage, message)
}

/// The addon a migration pull request's title names: `[19.0][MIG] x: ...`.
fn migrated_addon(title: &str) -> Option<String> {
    let (_, rest) = title.split_once("[MIG]")?;
    let addon: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!addon.is_empty()).then_some(addon)
}

/// Check out someone's migration pull request as `<to>-mig-<addon>` and
/// remember where it comes from.
fn check_out(
    ctx: &Context<'_>,
    pr: &str,
    args: &MigrateUpdateArgs,
) -> Result<MigrationBranch, Report> {
    let Some((path, number)) = parse_pr(pr) else {
        return Err(usage(format!(
            "{pr:?}: name the pull request as <owner>/<repo>#<number>"
        )));
    };
    let remote = args
        .remote
        .clone()
        .or_else(|| remote_for(ctx, path))
        .ok_or_else(|| {
            usage(format!(
                "no remote of this clone points at {path}: add one, or name it with --remote"
            ))
        })?;
    let repo = forge_repo(ctx, &remote)
        .ok_or_else(|| failed(format!("the remote {remote} is not on a forge")))?;
    let sleep = std::thread::sleep;
    let (kind, found) = ReqwestHttp::new()
        .and_then(|http| {
            let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &sleep);
            client.kind(&repo).and_then(|kind| {
                client
                    .pull_request(&repo, kind, number)
                    .map(|pr| (kind, pr))
            })
        })
        .map_err(|e| failed(e.to_string()))?;
    let addon = migrated_addon(&found.title).ok_or_else(|| {
        usage(format!(
            "{pr}: \"{}\" names no `[MIG] <addon>`; is it a migration pull request?",
            found.title
        ))
    })?;
    let to = found.base;
    let name = format!("{to}-mig-{addon}");
    let exists = output_of(&git(ctx).args(["rev-parse", "--verify", "--quiet", &name])).is_ok();
    if exists {
        return Err(usage(format!(
            "the branch {name} exists here: check it out and run `odx migrate update` on it"
        )));
    }
    let (_, head) = refs(kind, "", number);
    Worktree::new(&OsRunner, &ctx.dir)
        .fetch(&remote, &[format!("{head}:refs/heads/{name}")])
        .and_then(|()| {
            super::run_checked(&git(ctx).args(["switch", "--quiet", &name]), "git switch")
        })
        .map_err(failed)?;
    let from = args
        .from
        .clone()
        .or_else(|| previous(&to))
        .unwrap_or_default();
    for (key, value) in [("odxMigrateFrom", &from), ("odxMigrateRemote", &remote)] {
        let _ = output_of(&git(ctx).args(["config", &format!("branch.{name}.{key}"), value]));
    }
    pin::remember_pull_request(ctx, &name, &repo, number);
    if let Some(head_repository) = &found.head_repository {
        pin::remember(
            ctx,
            &name,
            &format!("{head_repository}.git"),
            &found.head_branch,
        );
    }
    Ok(MigrationBranch {
        name,
        to,
        addon,
        from,
        remote,
    })
}

/// `odx migrate update --json`.
#[derive(serde::Serialize)]
struct Updated<'a> {
    status: &'static str,
    pushed_to: Option<&'a str>,
    #[serde(flatten)]
    summary: &'a Summary,
}

/// Run `odx migrate update`.
pub(crate) fn run(
    args: &MigrateUpdateArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    tools: &Tools,
) -> Result<Report, Report> {
    let branch = if let Some(pr) = &args.pr {
        check_out(ctx, pr, args)?
    } else {
        let mut branch = pin::current(ctx)?;
        if let Some(from) = &args.from {
            branch.from.clone_from(from);
        }
        if let Some(remote) = &args.remote {
            branch.remote.clone_from(remote);
        }
        branch
    };
    let plan = Plan {
        addon: branch.addon.clone(),
        from: branch.from.clone(),
        to: branch.to.clone(),
        remote: branch.remote.clone(),
        no_pre_commit: args.no_pre_commit,
        no_squash: args.no_squash,
        squash: squash_rules(ctx),
    };
    let outcome = Worktree::new(&OsRunner, &ctx.dir)
        .update(plan, args.since.as_deref(), tools)
        .map_err(failed)?;
    let summary = match outcome {
        Outcome::Done(summary) => summary,
        Outcome::Stopped(message) => return Err(failed(message)),
    };
    let mut report = Report {
        warnings: summary
            .warnings
            .iter()
            .map(|message| Problem {
                key: "migrate".to_owned(),
                message: message.clone(),
            })
            .collect(),
        ..Report::default()
    };
    let pushed = if args.push {
        Some(push(ctx, &branch.name)?)
    } else {
        None
    };
    report.out = if global.json {
        let value = Updated {
            status: "updated",
            pushed_to: pushed.as_deref(),
            summary: &summary,
        };
        format!("{}\n", serde_json::to_string(&value).unwrap_or_default())
    } else {
        text(&summary, pushed.as_deref())
    };
    Ok(report)
}

/// Push `branch` where its pull request comes from.
fn push(ctx: &Context<'_>, branch: &str) -> Result<String, Report> {
    let (to, head) = pin::pushed_to(ctx, branch).ok_or_else(|| {
        failed(format!(
            "odx does not know where {branch} is pushed (`odx migrate pr` records it); push it with git"
        ))
    })?;
    let refspec = format!("HEAD:refs/heads/{head}");
    super::run_checked(&git(ctx).args(["push", "--quiet", &to, &refspec]), "git push")
        .map_err(|e| {
            failed(format!(
                "{e}\nwhen the pull request is not yours and its author does not let maintainers push, open a pull request against their branch instead"
            ))
        })?;
    Ok(format!("{to} {head}"))
}

pub(crate) fn text(summary: &Summary, pushed: Option<&str>) -> String {
    let mut out = if summary.history == 0 {
        format!("{} is up to date with {}\n", summary.branch, summary.source)
    } else {
        format!(
            "updated {} from {}: {} new commit(s)",
            summary.branch, summary.source, summary.history
        )
    };
    if summary.history > 0 {
        if summary.squashed > 0 {
            let _ = write!(
                out,
                " ({} administrative commit(s) squashed into them)",
                summary.squashed
            );
        }
        out.push('\n');
    }
    for commit in &summary.commits {
        let _ = writeln!(out, "  {commit}");
    }
    for finding in &summary.findings {
        let _ = writeln!(
            out,
            "  left by hand: {}:{}: {} {}",
            finding.file, finding.line, finding.code, finding.message
        );
    }
    out.push_str(&timings(&summary.timings));
    match pushed {
        Some(target) => {
            let _ = writeln!(out, "pushed to {target}");
        }
        None if summary.history > 0 => {
            out.push_str("next: review the new commits and run the tests; `--push` sends them to the pull request\n");
        }
        None => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addons_from_titles() {
        assert_eq!(
            migrated_addon("[19.0][MIG] contract: Migration to 19.0").as_deref(),
            Some("contract")
        );
        assert_eq!(
            migrated_addon("[19.0][MIG]  sale_x").as_deref(),
            Some("sale_x")
        );
        assert_eq!(migrated_addon("[19.0][FIX] contract"), None);
        assert_eq!(migrated_addon("[19.0][MIG] : x"), None);
    }
}
