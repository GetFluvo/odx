//! Writing a promotion plan: a commit on `odx/promote/<to>-<selection>`,
//! made in a temporary worktree of the target branch, and its pull request.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_config::environments::Model;
use odx_config::lock::Lockfile;
use odx_core::{Exit, Invocation};
use odx_env::copy;
use odx_env::promote::Plan;
use odx_env::{Change, Kind, Unit};
use odx_forge::RepoUrl;

use super::Report;
use super::aggregate::uv_program;
use super::checkout::{Checkout, git};
use super::propose::{Proposed, Pushed, forge_repo, propose};
use crate::Context;
use crate::cli::PromoteArgs;

/// What writing the plan did.
#[derive(Debug)]
pub(crate) struct Applied {
    /// The branch written.
    pub(crate) branch: String,
    /// Its head.
    pub(crate) commit: String,
    /// Commits cherry-picked onto it.
    pub(crate) picked: usize,
    /// Branch-specific paths included on request.
    pub(crate) included: Vec<String>,
    /// The pull request opened for it.
    pub(crate) pushed: Option<Pushed>,
}

/// A temporary worktree, removed when dropped.
struct Worktree<'a> {
    root: &'a Path,
    dir: PathBuf,
}

impl Drop for Worktree<'_> {
    fn drop(&mut self) {
        let dir = self.dir.to_string_lossy().into_owned();
        let _ = git(self.root, &["worktree", "remove", "--force", &dir]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn failed(message: String) -> Report {
    Report::error(Exit::Failure, message)
}

/// The commits of `from` on `paths` that `to` lacks (by patch id), oldest
/// first, as `<sha> <subject>`.
pub(crate) fn missing_commits(root: &Path, from: &str, to: &str, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let range = format!("{to}...{from}");
    let mut args = vec![
        "log",
        "--reverse",
        "--no-merges",
        "--cherry-pick",
        "--right-only",
        "--format=%H %s",
        &range,
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    git(root, &args)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The folders of the closure's project addons on the source.
pub(crate) fn addon_paths(from: &Checkout, plan: &Plan) -> Vec<String> {
    plan.items
        .iter()
        .filter(|item| item.from.kind == Kind::Addon)
        .filter_map(|item| {
            from.addons
                .iter()
                .find(|addon| addon.name == item.addon)
                .map(|addon| addon.path.clone())
        })
        .collect()
}

/// The branch a promotion of `plan` into `to` is written on.
fn branch_of(args: &PromoteArgs, plan: &Plan, to: &str) -> String {
    let key = args
        .feature
        .clone()
        .unwrap_or_else(|| plan.selection.join("-"));
    let key: String = key.chars().take(60).collect();
    format!("odx/promote/{to}-{key}")
}

/// Write `plan` as a commit, and open its pull request with `--open-pr`.
pub(crate) fn apply(
    ctx: &Context<'_>,
    root: &Path,
    model: &Model,
    args: &PromoteArgs,
    sides: [&Checkout; 2],
    plan: &Plan,
) -> Result<Applied, Report> {
    let [from, to] = sides;
    if !args.yes {
        return Err(Report::error(
            Exit::Usage,
            "--commit and --open-pr write a branch: add --yes to confirm".to_owned(),
        ));
    }
    if let Some(path) = args
        .include_branch_specific
        .iter()
        .find(|path| !model.is_branch_specific(path))
    {
        return Err(Report::error(
            Exit::Usage,
            format!(
                "{path} is not branch-specific (`branch_specific` in [tool.odx.environments]): nothing to include"
            ),
        ));
    }
    let branch = branch_of(args, plan, &to.env.name);
    // In the repository's own git directory, so promotions of other
    // repositories never meet; one left over from an interrupted run goes.
    let common = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .unwrap_or_default();
    let dir = PathBuf::from(common).join("odx-promote").join(&to.env.name);
    let dir_text = dir.to_string_lossy().into_owned();
    let _ = git(root, &["worktree", "remove", "--force", &dir_text]);
    let _ = std::fs::remove_dir_all(&dir);
    git(
        root,
        &[
            "worktree", "add", "--quiet", "-B", &branch, &dir_text, &to.from,
        ],
    )
    .map_err(|e| failed(format!("cannot prepare {branch}: {e}")))?;
    let worktree = Worktree { root, dir };
    let wt = worktree.dir.as_path();
    let picked = if args.with_code {
        cherry_pick(wt, from, to, plan)?
    } else {
        0
    };
    restore_branch_specific(wt, model, &to.from, &args.include_branch_specific);
    for path in &args.include_branch_specific {
        include(wt, &from.from, path);
    }
    write_pins(ctx, wt, args, sides, plan)?;
    let title = title(plan, from, to);
    let body = body(plan, from, to, &args.include_branch_specific);
    commit(wt, &title, &body, picked, &to.env.name).and_then(|commit| {
        let pushed = if args.open_pr {
            Some(open(ctx, wt, args, sides, plan, &branch)?)
        } else {
            None
        };
        Ok(Applied {
            branch,
            commit,
            picked,
            included: args.include_branch_specific.clone(),
            pushed,
        })
    })
}

/// Commit what the worktree has and return the head: refused when there is
/// nothing, and no commit was cherry-picked either.
fn commit(wt: &Path, title: &str, body: &str, picked: usize, to: &str) -> Result<String, Report> {
    // What cannot be staged leaves nothing to commit.
    let _ = git(wt, &["add", "-A"]);
    let staged = git(wt, &["diff", "--cached", "--quiet"]).is_err();
    if !staged && picked == 0 {
        return Err(failed(format!(
            "nothing to write: {to} has the pins already (--with-code promotes the code)"
        )));
    }
    let committed = if staged {
        git(
            wt,
            &["commit", "--quiet", "--no-verify", "-m", title, "-m", body],
        )
        .map(drop)
    } else {
        Ok(())
    };
    committed
        .and_then(|()| git(wt, &["rev-parse", "HEAD"]))
        .map_err(failed)
}

/// Cherry-pick the source's commits on the closure's project addons that
/// the target lacks; how many.
fn cherry_pick(wt: &Path, from: &Checkout, to: &Checkout, plan: &Plan) -> Result<usize, Report> {
    let commits = missing_commits(wt, &from.from, &to.from, &addon_paths(from, plan));
    for line in &commits {
        let sha = line.split(' ').next().unwrap_or(line);
        if let Err(e) = git(wt, &["cherry-pick", "--allow-empty", sha]) {
            let _ = git(wt, &["cherry-pick", "--abort"]);
            return Err(failed(format!(
                "{line} does not apply on {}: {e}",
                to.env.name
            )));
        }
    }
    Ok(commits.len())
}

/// Put the branch-specific files the cherry-picks changed back as the
/// target has them, but those `included`.
fn restore_branch_specific(wt: &Path, model: &Model, base: &str, included: &[String]) {
    let changed = git(wt, &["diff", "--name-only", base, "HEAD"]).unwrap_or_default();
    for path in changed.lines() {
        let named = included.iter().any(|include| {
            let include = include.trim_end_matches('/');
            path == include || path.starts_with(&format!("{include}/"))
        });
        // The worktree has the path, and the target has it when it is
        // checked out from there: neither fails.
        if model.is_branch_specific(path) && !named {
            if git(wt, &["cat-file", "-e", &format!("{base}:{path}")]).is_ok() {
                let _ = git(wt, &["checkout", base, "--", path]);
            } else {
                let _ = git(wt, &["rm", "--quiet", "--", path]);
            }
        }
    }
}

/// Copy `path` from `from`, or remove it when the source has none.
fn include(wt: &Path, from: &str, path: &str) {
    // The source has the path when it is checked out from there.
    let _ = git(wt, &["rm", "-r", "--quiet", "--ignore-unmatch", "--", path]);
    if git(wt, &["cat-file", "-e", &format!("{from}:./{path}")]).is_ok() {
        let _ = git(wt, &["checkout", from, "--", path]);
    }
}

/// Copy the pins of the plan's packages and the Odoo core into the
/// worktree, and check that `uv lock` keeps them.
fn write_pins(
    ctx: &Context<'_>,
    wt: &Path,
    args: &PromoteArgs,
    sides: [&Checkout; 2],
    plan: &Plan,
) -> Result<(), Report> {
    let [from, to] = sides;
    let dists: Vec<String> = plan
        .items
        .iter()
        .filter(|item| item.from.kind == Kind::Package && item.change != Change::Identical)
        .map(|item| item.from.name.clone())
        .collect();
    // A file of the worktree that cannot be written fails `uv lock`, or
    // leaves its change out of the commit.
    let save = |path: &str, text: &str| {
        let _ = std::fs::write(wt.join(path), text);
    };
    if !dists.is_empty() {
        let pyproject = copy::pyproject(&from.pyproject, &to.pyproject, &dists).map_err(failed)?;
        save("pyproject.toml", &pyproject);
        let ours = from.uv.as_ref().map_or("", |(text, _)| text.as_str());
        let theirs = to.uv.as_ref().map_or(ours, |(text, _)| text.as_str());
        let expected = copy::uv_lock(ours, theirs, &dists);
        save("uv.lock", &expected);
        let uv = uv_program(ctx.env);
        super::run_checked(
            &Invocation::new(&uv).arg("lock").current_dir(wt),
            &format!("{uv} lock"),
        )
        .map_err(failed)?;
        let actual = std::fs::read_to_string(wt.join("uv.lock")).unwrap_or_default();
        let drift = copy::drift(&expected, &actual);
        if !drift.is_empty() && !args.allow_transitive {
            return Err(failed(format!(
                "transitive drift: `uv lock` re-resolved {} (--allow-transitive keeps it)",
                drift.join(", ")
            )));
        }
    }
    if !dists.is_empty() || plan.core.is_some() {
        let mut lock = to
            .lock
            .as_ref()
            .map(|(_, lock)| lock.clone())
            .unwrap_or_default();
        let empty = Lockfile::new();
        let ours = from.lock.as_ref().map_or(&empty, |(_, lock)| lock);
        copy::odx_lock(ours, &mut lock, &dists, plan.core.is_some());
        let path = to.config.lockfile().to_string_lossy().replace('\\', "/");
        save(&path, &lock.render());
    }
    Ok(())
}

fn title(plan: &Plan, from: &Checkout, to: &Checkout) -> String {
    format!(
        "promote {} from {} to {}",
        plan.selection.join(", "),
        from.env.name,
        to.env.name
    )
}

fn pinned(unit: Option<&Unit>) -> String {
    unit.map_or_else(
        || "-".to_owned(),
        |unit| {
            let pin: String = unit.pin.chars().take(12).collect();
            match &unit.version {
                Some(version) if *version != unit.pin => format!("{version} `{pin}`"),
                _ => format!("`{pin}`"),
            }
        },
    )
}

/// The commit message body and pull request description.
fn body(plan: &Plan, from: &Checkout, to: &Checkout, included: &[String]) -> String {
    let mut out = format!(
        "Promoted with `odx promote` from {} to {} (series {}).\n\n\
         | addon | change | {} | {} |\n| --- | --- | --- | --- |\n",
        from.env.name, to.env.name, from.env.series, from.env.name, to.env.name
    );
    for item in &plan.items {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            item.addon,
            super::promote::status(item.change),
            pinned(Some(&item.from)),
            pinned(item.to.as_ref())
        );
    }
    if let Some((ours, theirs)) = &plan.core {
        let _ = write!(
            out,
            "\nOdoo core: {} on {}, {} on {} (copied).\n",
            pinned(Some(ours)),
            from.env.name,
            pinned(theirs.as_ref()),
            to.env.name
        );
    }
    if !plan.external.is_empty() {
        let _ = write!(
            out,
            "\nNot copied (Odoo's own addons, or of aggregated repositories): {}.\n",
            plan.external.join(", ")
        );
    }
    for warning in &plan.warnings {
        let _ = write!(out, "\nWarning: {warning}\n");
    }
    if !included.is_empty() {
        let _ = write!(
            out,
            "\n**Branch-specific content included**: {}\n",
            included.join(", ")
        );
    }
    out
}

fn open(
    ctx: &Context<'_>,
    wt: &Path,
    args: &PromoteArgs,
    sides: [&Checkout; 2],
    plan: &Plan,
    branch: &str,
) -> Result<Pushed, Report> {
    let [from, to] = sides;
    let worktree = Context {
        fs: ctx.fs,
        env: ctx.env,
        dir: wt.to_path_buf(),
        config: ctx.config,
    };
    let (remote, upstream) = to.env.repo.as_ref().map_or_else(
        || (args.remote.clone(), forge_repo(&worktree, &args.remote)),
        |repo| (repo.clone(), RepoUrl::parse(repo)),
    );
    let upstream = upstream.ok_or_else(|| failed(format!("{remote} is not on a forge")))?;
    propose(
        &worktree,
        &Proposed {
            branch,
            base: &to.env.branch,
            remote: &remote,
            upstream: &upstream,
            push_remote: None,
            draft: false,
            title: title(plan, from, to),
            body: body(plan, from, to, &args.include_branch_specific),
        },
    )
}
