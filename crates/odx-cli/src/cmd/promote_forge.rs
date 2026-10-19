//! Merged pull requests of an environment, for `odx promote --from-pr` and
//! `--rollback`: what the forge says of them, and their head fetched.

use std::path::Path;

use odx_config::environments::Environment;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, ForgeKind, Http, PrState, PullRequest, RepoUrl, ReqwestHttp};

use super::Report;
use super::checkout::git;
use super::propose::forge_repo;
use crate::Context;

/// A merged pull request into an environment's branch.
#[derive(Debug)]
pub(crate) struct Merged {
    /// What the forge says of it.
    pub(crate) pr: PullRequest,
    /// Its commits, oldest first.
    pub(crate) commits: Vec<String>,
    /// The local ref of its head.
    pub(crate) head: String,
}

fn failed(message: String) -> Report {
    Report::error(Exit::Failure, message)
}

/// Pull request `number` of `env`'s repository (its `repo`, else
/// `remote`), which must be merged into `env`'s branch; its head fetched
/// as `refs/odx/pr/<number>`.
pub(crate) fn merged(
    ctx: &Context<'_>,
    root: &Path,
    env: &Environment,
    remote: &str,
    number: u64,
) -> Result<Merged, Report> {
    let here = Context {
        fs: ctx.fs,
        env: ctx.env,
        dir: root.to_path_buf(),
        config: ctx.config,
    };
    let (from, repo) = env.repo.as_ref().map_or_else(
        || (remote.to_owned(), forge_repo(&here, remote)),
        |repo| (repo.clone(), RepoUrl::parse(repo)),
    );
    let repo = repo.ok_or_else(|| failed(format!("{from} is not on a forge")))?;
    let found = ReqwestHttp::new().and_then(|http| {
        let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &std::thread::sleep);
        client.kind(&repo).and_then(|kind| {
            client.pull_request(&repo, kind, number).and_then(|pr| {
                client
                    .pull_request_commits(&repo, kind, number)
                    .map(|commits| (kind, pr, commits))
            })
        })
    });
    let (kind, pr, commits) = found.map_err(|e| failed(e.to_string()))?;
    if pr.state != PrState::Merged || pr.base != env.branch {
        return Err(Report::error(
            Exit::Usage,
            format!(
                "#{number} is no pull request merged into {} ({}): it is {} into {}",
                env.name,
                env.branch,
                pr.state.name(),
                pr.base
            ),
        ));
    }
    let head = format!("refs/odx/pr/{number}");
    let source = head_ref(kind, number);
    git(
        root,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            &from,
            &format!("+{source}:{head}"),
        ],
    )
    .map_err(|e| failed(format!("cannot fetch #{number} from {from}: {e}")))?;
    Ok(Merged { pr, commits, head })
}

/// Where the forge keeps the head of pull request `number`.
fn head_ref(kind: ForgeKind, number: u64) -> String {
    match kind {
        ForgeKind::Gitlab => format!("refs/merge-requests/{number}/head"),
        ForgeKind::Github | ForgeKind::Gitea => format!("refs/pull/{number}/head"),
    }
}

/// The files the commits change.
pub(crate) fn touched(root: &Path, commits: &[String]) -> Vec<String> {
    let mut files: Vec<String> = commits
        .iter()
        .flat_map(|sha| {
            git(
                root,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-only",
                    "-r",
                    "--root",
                    sha,
                ],
            )
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_per_forge() {
        assert_eq!(head_ref(ForgeKind::Gitlab, 3), "refs/merge-requests/3/head");
        assert_eq!(head_ref(ForgeKind::Gitea, 3), "refs/pull/3/head");
        assert_eq!(head_ref(ForgeKind::Github, 3), "refs/pull/3/head");
    }
}
