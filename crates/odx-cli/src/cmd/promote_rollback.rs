//! `odx promote --rollback <n>`: revert a merged promotion on its target,
//! however it was merged (merge commit, rebase or squash).

use std::fmt::Write as _;
use std::path::Path;

use odx_config::environments::{Environment, Model};
use odx_core::{Exit, Invocation};

use super::Report;
use super::checkout::{Checkout, git, read};
use super::output_of;
use super::promote_apply::{commit, failed, open, worktree};
use super::promote_forge::{Merged, merged};
use super::propose::{opened_json, opened_text};
use crate::Context;
use crate::cli::{GlobalArgs, PromoteArgs};

/// The patch id of the change from `a` to `b`.
fn patch_id(root: &Path, a: &str, b: &str) -> String {
    let diff = git(root, &["diff", a, b]).unwrap_or_default();
    output_of(
        &Invocation::new("git")
            .arg("-C")
            .arg(root)
            .args(["patch-id", "--stable"])
            .stdin(format!("{diff}\n")),
    )
    .unwrap_or_default()
    .split(' ')
    .next()
    .unwrap_or_default()
    .to_owned()
}

/// The commits of `target` that carry the pull request, newest first:
/// its own commits when they are on the target (merged with a merge
/// commit), else the target's commits since they parted with the same
/// change as one of them, or as all of them at once (squashed).
fn carrying(root: &Path, target: &str, merged: &Merged) -> Vec<String> {
    let on_target = |sha: &str| git(root, &["merge-base", "--is-ancestor", sha, target]).is_ok();
    if merged.commits.iter().all(|sha| on_target(sha)) {
        return merged.commits.iter().rev().cloned().collect();
    }
    let first = merged.commits.first().map_or("", String::as_str);
    let mut wanted: Vec<String> = merged
        .commits
        .iter()
        .map(|sha| patch_id(root, &format!("{sha}^"), sha))
        .collect();
    wanted.push(patch_id(root, &format!("{first}^"), &merged.head));
    // An id git could not compute matches every other one it could not.
    wanted.retain(|id| !id.is_empty());
    let fork = git(root, &["merge-base", target, &merged.head]).unwrap_or_default();
    git(
        root,
        &["rev-list", "--no-merges", &format!("{fork}..{target}")],
    )
    .unwrap_or_default()
    .lines()
    .filter(|sha| wanted.contains(&patch_id(root, &format!("{sha}^"), sha)))
    .map(str::to_owned)
    .collect()
}

fn subject(root: &Path, sha: &str) -> String {
    git(root, &["log", "-1", "--format=%h %s", sha]).unwrap_or_default()
}

/// `odx promote --rollback`.
pub(crate) fn rollback(
    ctx: &Context<'_>,
    root: &Path,
    model: &Model,
    args: &PromoteArgs,
    to: Environment,
    global: &GlobalArgs,
) -> Result<Report, Report> {
    let number = args.rollback.unwrap_or_default();
    let target = read(root, to, &args.remote)?;
    let found = merged(ctx, root, &target.env, &args.remote, number)?;
    let reverted = carrying(root, &target.from, &found);
    if reverted.is_empty() {
        return Err(failed(format!(
            "#{number} is not on {} ({}): nothing to roll back",
            target.env.name, target.from
        )));
    }
    let subjects: Vec<String> = reverted.iter().map(|sha| subject(root, sha)).collect();
    let mut out = format!(
        "roll back #{number} ({}) on {} ({}):\n",
        found.pr.title, target.env.name, target.from
    );
    for line in &subjects {
        let _ = writeln!(out, "  revert {line}");
    }
    let mut applied = serde_json::Value::Null;
    if !(args.commit || args.open_pr) {
        out.push_str("plan only (--commit or --open-pr with --yes writes it)\n");
    } else if !args.yes {
        return Err(Report::error(
            Exit::Usage,
            "--commit and --open-pr write a branch: add --yes to confirm".to_owned(),
        ));
    } else {
        let written = write(ctx, root, model, args, &target, &found, &reverted)?;
        let _ = write!(
            out,
            "committed {} on {}\n{}",
            written.commit.chars().take(7).collect::<String>(),
            written.branch,
            written.diff
        );
        if let Some(pushed) = &written.pushed {
            out.push_str(&opened_text(pushed));
        }
        applied = serde_json::json!({
            "branch": written.branch,
            "commit": written.commit,
            "env_diff": written.diff,
            "pull_request": written.pushed.as_ref().map(opened_json),
        });
    }
    let value = serde_json::json!({
        "schema": 1,
        "rollback": number,
        "title": found.pr.title,
        "to": { "env": target.env.name, "branch": target.env.branch, "from": target.from },
        "revert": reverted,
        "applied": applied,
    });
    Ok(Report {
        out: if global.json {
            format!("{value:#}\n")
        } else {
            out
        },
        ..Report::default()
    })
}

/// What writing the rollback did.
struct Written {
    branch: String,
    commit: String,
    /// `odx env diff` of the target and the rollback.
    diff: String,
    pushed: Option<super::propose::Pushed>,
}

fn write(
    ctx: &Context<'_>,
    root: &Path,
    model: &Model,
    args: &PromoteArgs,
    target: &Checkout,
    found: &Merged,
    reverted: &[String],
) -> Result<Written, Report> {
    let number = found.pr.number;
    let branch = format!("odx/rollback/{}-{number}", target.env.name);
    let worktree = worktree(root, &branch, &target.from, &target.env.name)?;
    let wt = worktree.dir.as_path();
    let mut revert = vec!["revert", "--no-edit", "--no-commit"];
    revert.extend(reverted.iter().map(String::as_str));
    if let Err(e) = git(wt, &revert) {
        let _ = git(wt, &["revert", "--abort"]);
        return Err(failed(format!(
            "#{number} does not revert cleanly on {}: {e}",
            target.env.name
        )));
    }
    let title = format!("revert promotion #{number}: {}", found.pr.title);
    let listed = reverted.iter().fold(String::new(), |mut listed, sha| {
        let _ = writeln!(listed, "- {}", subject(root, sha));
        listed
    });
    let body =
        format!("Rolls back #{number} with `odx promote --rollback`, reverting:\n\n{listed}");
    commit(wt, &title, &body, 1, &target.env.name).and_then(|head| {
        // The rollback as an environment, read as the branch it is on here.
        let rolled = Environment {
            branch: branch.clone(),
            repo: None,
            ..target.env.clone()
        };
        // Both read before, from the same refs.
        let diff = read(root, target.env.clone(), &args.remote)
            .and_then(|a| {
                read(root, rolled, &args.remote)
                    .map(|b| super::env_diff::between(root, &model.branch_specific, a, b))
            })
            .unwrap_or_default();
        let pushed = if args.open_pr {
            let description = format!(
                "{body}\nWhat changes on {}:\n\n```\n{diff}```\n",
                target.env.name
            );
            Some(open(
                ctx,
                wt,
                target,
                &args.remote,
                &branch,
                title,
                description,
            )?)
        } else {
            None
        };
        Ok(Written {
            branch,
            commit: head,
            diff,
            pushed,
        })
    })
}
