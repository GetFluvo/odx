//! `odx port pr <owner>/<repo>#<n> --to <series>` and `odx port addon
//! <addon> --from <series> --to <series>`: commits cherry-picked onto a
//! branch of the target series with their authors, then odoo-lint's
//! upgrade fixes; `odx port --continue` and `--abort` after a conflict;
//! `--open-pr` to push the branch and open its pull request.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, ForgeError, ForgeKind, Http, RepoUrl, ReqwestHttp};
use odx_port::analysis::{Class, Group, Query, Settings, analyse};
use odx_port::blacklist::{self, Blacklist};
use odx_port::git::Repo;
use odx_port::migrate::{Finding, Tools, Worktree};
use odx_port::port::{PortOutcome, PortPlan, Ported};
use schemars::JsonSchema;
use serde::Serialize;

use super::aggregate::Prepared;
use super::propose::{
    Proposed, Pushed, default_remote, forge_repo, git, opened_json, opened_text, propose,
};
use super::suggest::{Found, repository_name};
use super::{Report, output_of};
use crate::Context;
use crate::cli::{GlobalArgs, PortArgs, PortCommand, PortOptions};

/// Run `odx port`.
pub(crate) fn run_port(args: &PortArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    port(args, global, ctx).unwrap_or_else(|report| report)
}

pub(crate) fn usage(message: String) -> Report {
    Report::error(Exit::Usage, message)
}

pub(crate) fn failed(message: String) -> Report {
    Report::error(Exit::Failure, message)
}

/// A forge error as the report that ends the command.
#[expect(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands the error over by value"
)]
fn forge_failure(e: ForgeError) -> Report {
    failed(e.to_string())
}

fn port(args: &PortArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let tree = Worktree::new(&OsRunner, &ctx.dir);
    let tools = Tools {
        odl: ctx
            .env
            .var("ODX_ODL")
            .as_deref()
            .unwrap_or("odl")
            .to_owned(),
        pre_commit: None,
    };
    let mut out = String::new();
    let (outcome, meta) = match (&args.command, args.resume, args.abort) {
        (_, _, true) => return aborted(&tree, global),
        (_, true, _) => {
            let meta = tree
                .port_state()
                .map(|state| state.plan.meta)
                .unwrap_or_default();
            (tree.port_resume(&tools), meta)
        }
        (Some(PortCommand::Pr { pr, options }), _, _) => {
            let plan = pr_plan(ctx, pr, options)?;
            let meta = plan.meta.clone();
            (tree.port(plan, &tools), meta)
        }
        (
            Some(PortCommand::Addon {
                addon,
                from,
                options,
            }),
            _,
            _,
        ) => {
            let Some(plan) = addon_plan(ctx, addon, from, options, &mut out)? else {
                return Ok(Report {
                    out: if global.json {
                        super::to_json(&PortJson {
                            schema: 1,
                            outcome: PortJsonOutcome::NothingToPort,
                        })
                    } else {
                        out
                    },
                    ..Report::default()
                });
            };
            let meta = plan.meta.clone();
            (tree.port(plan, &tools), meta)
        }
        (
            Some(PortCommand::Blacklist {
                addon,
                entry,
                reason,
            }),
            _,
            _,
        ) => return add_blacklist(&tree, addon, entry, reason),
        (Some(PortCommand::Sweep(sweep)), _, _) => {
            return super::sweep::run_sweep(sweep, global, ctx, &tree);
        }
        (None, _, _) => {
            return Err(usage(
                "name what to port: `odx port pr <owner>/<repo>#<n> --to <series>` or `odx port addon <addon> --from <series> --to <series>`".to_owned(),
            ));
        }
    };
    match outcome.map_err(failed)? {
        PortOutcome::Done(ported) => Ok(finished(ctx, global, &ported, &meta, out)),
        PortOutcome::Stopped(message) => Ok(Report {
            exit: Exit::Failure,
            out: if global.json {
                super::to_json(&PortJson {
                    schema: 1,
                    outcome: PortJsonOutcome::Stopped { message: &message },
                })
            } else {
                out
            },
            error: Some(message),
            ..Report::default()
        }),
    }
}

/// `odx port --abort`.
fn aborted(tree: &Worktree<'_>, global: &GlobalArgs) -> Result<Report, Report> {
    let state = tree.port_abort().map_err(failed)?;
    let out = if global.json {
        super::to_json(&PortJson {
            schema: 1,
            outcome: PortJsonOutcome::Aborted {
                branch: &state.plan.branch,
            },
        })
    } else {
        format!(
            "aborted the port: deleted {}, back on {}\n",
            state.plan.branch, state.previous
        )
    };
    Ok(Report {
        out,
        ..Report::default()
    })
}

/// `odx port blacklist`.
fn add_blacklist(
    tree: &Worktree<'_>,
    addon: &str,
    entry: &str,
    reason: &str,
) -> Result<Report, Report> {
    let key = if entry == "orphaned" {
        blacklist::ORPHANED.to_owned()
    } else if parse_pr(entry).is_some() {
        entry.to_owned()
    } else {
        return Err(usage(format!(
            "{entry:?}: name the pull request as <owner>/<repo>#<number>, or `orphaned`"
        )));
    };
    if reason.trim().is_empty() {
        return Err(usage(format!("{key}: a blacklist entry needs a reason")));
    }
    let subject = tree
        .blacklist(addon, &[(key, reason.to_owned())])
        .map_err(failed)?;
    Ok(Report {
        out: format!("{subject}\n"),
        ..Report::default()
    })
}

/// The report of a finished port, after opening its pull request when the
/// port asked for it.
fn finished(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    ported: &Ported,
    meta: &BTreeMap<String, String>,
    mut out: String,
) -> Report {
    let warnings: Vec<Problem> = ported
        .warnings
        .iter()
        .map(|message| Problem {
            key: "port".to_owned(),
            message: message.clone(),
        })
        .collect();
    let pr = meta
        .contains_key("open_pr")
        .then(|| open_pr(ctx, ported, meta));
    let (opened, error) = match pr {
        Some(Ok(pushed)) => (Some(pushed), None),
        Some(Err(report)) => (None, report.error),
        None => (None, None),
    };
    if global.json {
        out = super::to_json(&PortJson {
            schema: 1,
            outcome: PortJsonOutcome::Done {
                ported,
                pull_request: opened.as_ref().map(opened_json),
            },
        });
    } else {
        out.push_str(&text(ported));
        if let Some(opened) = &opened {
            out.push_str(&opened_text(opened));
        }
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

/// Push the port's branch and open its pull request.
fn open_pr(
    ctx: &Context<'_>,
    ported: &Ported,
    meta: &BTreeMap<String, String>,
) -> Result<Pushed, Report> {
    let field = |key: &str| meta.get(key).cloned().unwrap_or_default();
    let remote = field("remote");
    let upstream = forge_repo(ctx, &remote)
        .ok_or_else(|| failed(format!("the remote {remote} is not on a forge")))?;
    let mut body = field("intro");
    findings_list(&mut body, &ported.findings);
    let _ = write!(body, "\n<!-- odx:{} -->\n", field("marker"));
    let to = field("to");
    let push_remote = meta.get("push_remote").map(String::as_str);
    propose(
        ctx,
        &Proposed {
            branch: &ported.branch,
            base: &to,
            remote: &remote,
            upstream: &upstream,
            push_remote,
            draft: true,
            replace: false,
            title: field("title"),
            body,
        },
    )
}

/// What odoo-lint leaves to fix by hand, as a checklist.
fn findings_list(body: &mut String, findings: &[Finding]) {
    if findings.is_empty() {
        body.push_str("\nodoo-lint finds nothing left to fix by hand.\n");
        return;
    }
    body.push_str("\n### Left to fix by hand\n\n");
    for finding in findings {
        let _ = writeln!(
            body,
            "- [ ] `{}:{}` {} {}",
            finding.file, finding.line, finding.code, finding.message
        );
    }
}

/// The metadata `--open-pr` needs, kept with the port.
fn meta(
    options: &PortOptions,
    remote: &str,
    title: String,
    intro: String,
    marker: String,
) -> BTreeMap<String, String> {
    let mut meta = BTreeMap::from([
        ("remote".to_owned(), remote.to_owned()),
        ("to".to_owned(), options.to.clone()),
        ("title".to_owned(), title),
        ("intro".to_owned(), intro),
        ("marker".to_owned(), marker),
    ]);
    if options.open_pr {
        meta.insert("open_pr".to_owned(), "true".to_owned());
    }
    if let Some(remote) = &options.push_remote {
        meta.insert("push_remote".to_owned(), remote.clone());
    }
    meta
}

/// `owner/repo#n`.
pub(crate) fn parse_pr(pr: &str) -> Option<(&str, u64)> {
    let (path, number) = pr.rsplit_once('#')?;
    let number = number.parse().ok()?;
    path.contains('/').then_some((path, number))
}

/// The remote of the clone that points at `path` on a forge.
pub(crate) fn remote_for(ctx: &Context<'_>, path: &str) -> Option<String> {
    output_of(&git(ctx).arg("remote"))
        .unwrap_or_default()
        .lines()
        .find(|remote| {
            forge_repo(ctx, remote).is_some_and(|repo| repo.path.eq_ignore_ascii_case(path))
        })
        .map(str::to_owned)
}

/// The commits of `shas`, oldest first.
fn oldest_first(ctx: &Context<'_>, shas: &[String]) -> Result<Vec<String>, Report> {
    let order = git(ctx)
        .args(["rev-list", "--no-walk=sorted", "--reverse"])
        .args(shas);
    output_of(&order)
        .map(|list| list.lines().map(str::to_owned).collect())
        .map_err(|e| failed(format!("cannot read the commits: {e}")))
}

/// The top-level directories the commits change.
fn touched(ctx: &Context<'_>, shas: &[String]) -> Vec<String> {
    let mut dirs = BTreeSet::new();
    for sha in shas {
        let files = output_of(&git(ctx).args([
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
            sha,
        ]))
        .unwrap_or_default();
        dirs.extend(
            files
                .lines()
                .filter_map(|file| file.split_once('/').map(|(dir, _)| dir.to_owned())),
        );
    }
    dirs.into_iter().collect()
}

/// The pull request's title, base and commits, as the forge has them.
fn pull_request(
    client: &ForgeClient<'_>,
    repo: &RepoUrl,
    number: u64,
) -> Result<(ForgeKind, String, String, Vec<String>), ForgeError> {
    let kind = client.kind(repo)?;
    let pr = client.pull_request(repo, kind, number)?;
    let commits = client.pull_request_commits(repo, kind, number)?;
    Ok((kind, pr.title, pr.base, commits))
}

/// How long a port waits for another one in the same clone.
const PORT_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `odx dashboard act <id> --port`: the suggestion's commits ported in a
/// clone of the target repository under the cache (sharing the cache's
/// objects), on `<to>-port-<n>`, then its pull request with `open_pr`.
pub(crate) fn port_suggestion(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    prepared: &Prepared,
    found: &Found,
    group: &Group,
    open_pr: bool,
) -> Report {
    let name: String = repository_name(&found.target_url)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let clone = prepared.cache.root().join("ports").join(&name);
    // One port at a time per clone: they share its branches and index.
    let lock = prepared
        .cache
        .root()
        .join("ports")
        .join(format!("{name}.lock"));
    let _lock = match odx_git::RepoLock::acquire(&lock, PORT_LOCK_TIMEOUT) {
        Ok(lock) => lock,
        Err(e) => return failed(format!("{}: {e}", lock.display())),
    };
    if !clone.join(".git").exists() {
        // Dissociated: the clone keeps its own objects, so collecting the
        // cache never takes any from under it.
        let made = super::run_checked(
            &odx_core::Invocation::new("git")
                .args(["clone", "--quiet", "--dissociate", "--reference"])
                .arg(found.repo_dir.as_os_str())
                .arg(&found.target_url)
                .arg(clone.as_os_str()),
            "git clone",
        );
        if let Err(e) = made {
            return failed(e);
        }
    }
    let here = Context {
        fs: ctx.fs,
        env: ctx.env,
        dir: clone.clone(),
        config: None,
    };
    let to = &found.target;
    let key: String = group
        .key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let options = PortOptions {
        to: to.clone(),
        remote: None,
        no_migrate: false,
        open_pr,
        push_remote: None,
    };
    let commits: Vec<String> = group.commits.iter().rev().map(|c| c.sha.clone()).collect();
    let plan = PortPlan {
        branch: format!("{to}-port-{}-{key}", found.addon),
        to: to.clone(),
        remote: "origin".to_owned(),
        commits,
        addons: vec![found.addon.clone()],
        no_migrate: false,
        meta: meta(
            &options,
            "origin",
            format!("[{to}][FW] {}", group.title),
            format!(
                "Port of {} ({}) to {to}, made with odx: {} commit(s) cherry-picked with their authors, then odoo-lint's upgrade fixes.\n",
                group.key,
                found.source,
                group.commits.len()
            ),
            format!("port of {} --to {to}", group.key),
        ),
    };
    let tools = Tools {
        odl: ctx
            .env
            .var("ODX_ODL")
            .as_deref()
            .unwrap_or("odl")
            .to_owned(),
        pre_commit: None,
    };
    let meta = plan.meta.clone();
    // Committed as the user, or as odx when git knows no one.
    let identity = |key: &str, default: &str| {
        output_of(&git(&here).args(["config", "--default", default, key])).unwrap_or_default()
    };
    let outcome = Worktree::new(&OsRunner, &clone)
        .with_identity(
            &identity("user.name", "odx"),
            &identity("user.email", "unknown"),
        )
        .port(plan, &tools);
    let where_ = format!("in {}:\n", clone.display());
    match outcome {
        Ok(PortOutcome::Done(ported)) => finished(&here, global, &ported, &meta, where_),
        Ok(PortOutcome::Stopped(message)) | Err(message) => failed(format!("{where_}{message}")),
    }
}

/// The plan for `odx port pr`.
fn pr_plan(ctx: &Context<'_>, pr: &str, options: &PortOptions) -> Result<PortPlan, Report> {
    let Some((path, number)) = parse_pr(pr) else {
        return Err(usage(format!(
            "{pr:?}: name the pull request as <owner>/<repo>#<number>"
        )));
    };
    let remote = options
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
    let (kind, title, base, commits) = ReqwestHttp::new()
        .and_then(|http| {
            let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &sleep);
            pull_request(&client, &repo, number)
        })
        .map_err(forge_failure)?;
    let head = match kind {
        ForgeKind::Gitlab => format!("refs/merge-requests/{number}/head"),
        ForgeKind::Github | ForgeKind::Gitea => format!("refs/pull/{number}/head"),
    };
    let tree = Worktree::new(&OsRunner, &ctx.dir);
    tree.fetch(&remote, &[head]).map_err(failed)?;
    let commits = oldest_first(ctx, &commits)?;
    let to = &options.to;
    let intro = format!(
        "Port of {path}#{number} ({base}) to {to}, made with odx: {} commit(s) cherry-picked with their authors, then odoo-lint's upgrade fixes.\n",
        commits.len()
    );
    Ok(PortPlan {
        branch: format!("{to}-port-{number}"),
        to: to.clone(),
        remote: remote.clone(),
        addons: touched(ctx, &commits),
        commits,
        no_migrate: options.no_migrate,
        meta: meta(
            options,
            &remote,
            format!("[{to}][FW] {title}"),
            intro,
            format!("port pr {path}#{number} --to {to}"),
        ),
    })
}

/// `<owner>/<repo>` of `remote`, for blacklist keys.
pub(crate) fn repository(ctx: &Context<'_>, remote: &str) -> String {
    let url = output_of(&git(ctx).args(["remote", "get-url", remote])).unwrap_or_default();
    forge_repo(ctx, remote).map_or_else(|| repository_name(&url), |repo| repo.path)
}

/// How commits are classified: `[tool.odx.port]` of a project around the
/// clone, if any.
pub(crate) fn settings(ctx: &Context<'_>) -> Result<Settings, Report> {
    let project = odx_config::find_pyproject(ctx.fs, &ctx.dir).ok();
    let port = project
        .as_ref()
        .and_then(|project| project.config.port.as_ref());
    Settings::new(
        port.and_then(|port| port.migration_commit_patterns.as_deref()),
        port.and_then(|port| port.repo_wide_threshold),
    )
    .map_err(usage)
}

/// The plan for `odx port addon`, after writing what the analysis found to
/// `out`; `None` when nothing is portable.
fn addon_plan(
    ctx: &Context<'_>,
    addon: &str,
    from: &str,
    options: &PortOptions,
    out: &mut String,
) -> Result<Option<PortPlan>, Report> {
    let to = &options.to;
    let remote = options
        .remote
        .clone()
        .unwrap_or_else(|| default_remote(ctx));
    let source = format!("refs/remotes/{remote}/{from}");
    let target = format!("refs/remotes/{remote}/{to}");
    let tree = Worktree::new(&OsRunner, &ctx.dir);
    tree.fetch(
        &remote,
        &[
            format!("+refs/heads/{from}:{source}"),
            format!("+refs/heads/{to}:{target}"),
        ],
    )
    .map_err(failed)?;
    let repository = repository(ctx, &remote);
    let runner = OsRunner;
    let repo = Repo::new(&runner, &ctx.dir);
    let blacklist = repo
        .show(&target, &blacklist::path(addon))
        .and_then(|text| Blacklist::parse(&text).ok())
        .unwrap_or_default();
    let query = Query {
        source: &source,
        targets: std::slice::from_ref(&target),
        source_dir: addon,
        target_dir: addon,
        repository: &repository,
        blacklist: &blacklist,
    };
    let settings = settings(ctx)?;
    // Both branches were just fetched: git reads them.
    let groups = analyse(&repo, &query, &settings).unwrap_or_default();
    let portable: Vec<&Group> = groups
        .iter()
        .filter(|group| group.class == Class::Portable)
        .collect();
    for group in &groups {
        let what = match group.class {
            Class::Portable => "port",
            other => other.name(),
        };
        let _ = writeln!(out, "{what}: {} {}", group.key, group.title);
    }
    if portable.is_empty() {
        let _ = writeln!(out, "nothing of {addon} on {from} to port to {to}");
        return Ok(None);
    }
    let wanted: BTreeSet<&str> = portable
        .iter()
        .flat_map(|group| group.commits.iter().map(|commit| commit.sha.as_str()))
        .collect();
    let range = format!("{target}..{source}");
    let history =
        output_of(&git(ctx).args(["rev-list", "--reverse", "--no-merges", &range, "--", addon]))
            .unwrap_or_default();
    let commits: Vec<String> = history
        .lines()
        .filter(|sha| wanted.contains(sha))
        .map(str::to_owned)
        .collect();
    let mut intro = format!(
        "Port of the changes `{addon}` has on {from} and lacks on {to}, made with odx, cherry-picked with their authors:\n\n| Source | Title | Commits |\n| --- | --- | --- |\n"
    );
    for group in &portable {
        let _ = writeln!(
            intro,
            "| {} | {} | {} |",
            group.key,
            group.title,
            group.commits.len()
        );
    }
    Ok(Some(PortPlan {
        branch: format!("{to}-port-{addon}"),
        to: to.clone(),
        remote: remote.clone(),
        commits,
        addons: vec![addon.to_owned()],
        no_migrate: options.no_migrate,
        meta: meta(
            options,
            &remote,
            format!("[{to}][FW] {addon}: port from {from}"),
            intro,
            format!("port addon {addon} --from {from} --to {to}"),
        ),
    }))
}

fn text(ported: &Ported) -> String {
    let mut out = format!(
        "ported on branch {}: {} commit(s) cherry-picked\n",
        ported.branch, ported.picked
    );
    for commit in &ported.commits {
        let _ = writeln!(out, "  {commit}");
    }
    if ported.findings.is_empty() {
        out.push_str("odoo-lint finds nothing left to do by hand\n");
    } else {
        let _ = writeln!(
            out,
            "{} finding(s) left to fix by hand:",
            ported.findings.len()
        );
        for finding in &ported.findings {
            let _ = writeln!(
                out,
                "  {}:{}: {} {}",
                finding.file, finding.line, finding.code, finding.message
            );
        }
    }
    out
}

/// What `odx --json port pr`, `port addon`, `--continue` and `--abort`
/// print.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx port")]
pub(crate) struct PortJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// How the port ended.
    #[serde(flatten)]
    outcome: PortJsonOutcome<'a>,
}

/// How a port ended, by its `status`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum PortJsonOutcome<'a> {
    /// The port branch is ready for review.
    Done {
        /// The port.
        #[serde(flatten)]
        ported: &'a Ported,
        /// The pull request opened, with `--open-pr`.
        #[serde(skip_serializing_if = "Option::is_none")]
        pull_request: Option<serde_json::Value>,
    },
    /// `port addon` found nothing portable.
    #[serde(rename = "nothing to port")]
    NothingToPort,
    /// A conflict stopped the port.
    Stopped {
        /// What happened and how to go on.
        message: &'a str,
    },
    /// `--abort` put the repository back.
    Aborted {
        /// The port branch.
        branch: &'a str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_names() {
        assert_eq!(parse_pr("OCA/pos#12"), Some(("OCA/pos", 12)));
        assert_eq!(parse_pr("pos#12"), None);
        assert_eq!(parse_pr("OCA/pos#x"), None);
        assert_eq!(parse_pr("OCA/pos"), None);
    }
}
