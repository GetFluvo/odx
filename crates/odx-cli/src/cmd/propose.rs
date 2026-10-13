//! Pushing a branch and opening its pull request, for `odx migrate pr` and
//! `odx port --open-pr`: to the remote when the token may push there, else
//! to the user's fork (found or created), or to a remote of their choice.

use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation};
use odx_forge::{ForgeClient, ForgeError, Http, Opened, Proposal, RepoUrl, ReqwestHttp};

use super::{Report, output_of};
use crate::Context;

pub(crate) fn git(ctx: &Context<'_>) -> Invocation {
    Invocation::new("git").arg("-C").arg(ctx.dir.as_os_str())
}

/// `upstream` when the clone has it, else `origin`.
pub(crate) fn default_remote(ctx: &Context<'_>) -> String {
    let remotes = output_of(&git(ctx).arg("remote")).unwrap_or_default();
    let upstream = remotes.lines().any(|remote| remote == "upstream");
    if upstream { "upstream" } else { "origin" }.to_owned()
}

/// The repository `remote` points at, when it is on a forge: the URL git
/// uses, or as configured when `insteadOf` turns it into something that is
/// not on a forge.
pub(crate) fn forge_repo(ctx: &Context<'_>, remote: &str) -> Option<RepoUrl> {
    let key = format!("remote.{remote}.url");
    let urls = [
        git(ctx).args(["remote", "get-url", remote]),
        git(ctx).args(["config", "--get", &key]),
    ];
    urls.iter()
        .find_map(|query| RepoUrl::parse(&output_of(query).unwrap_or_default()))
}

/// A pushed branch and the pull request opened for it.
#[derive(Debug)]
pub(crate) struct Pushed {
    /// Where the branch went: a remote or a URL.
    pub(crate) to: String,
    pub(crate) opened: Opened,
    pub(crate) draft: bool,
}

pub(crate) fn opened_text(pushed: &Pushed) -> String {
    let draft = if pushed.draft { " as a draft" } else { "" };
    format!(
        "pushed to {}\nopened {}{draft}\n",
        pushed.to, pushed.opened.url
    )
}

pub(crate) fn opened_json(pushed: &Pushed) -> serde_json::Value {
    serde_json::json!({
        "status": "opened",
        "pushed_to": pushed.to,
        "number": pushed.opened.number,
        "url": pushed.opened.url,
        "draft": pushed.draft,
    })
}

/// Where to push a branch for a fork: like the remote's URL, SSH or HTTPS.
fn push_url(remote_url: &str, fork: &RepoUrl) -> String {
    if remote_url.starts_with("git@") || remote_url.starts_with("ssh://") {
        format!("git@{}:{}.git", fork.host, fork.path)
    } else {
        format!("{}/{}.git", fork.origin(), fork.path)
    }
}

/// A forge error as the report that ends the command.
#[expect(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands the error over by value"
)]
fn failure(e: ForgeError) -> Report {
    Report::error(Exit::Failure, e.to_string())
}

/// A branch to push and the pull request to open for it.
#[derive(Debug)]
pub(crate) struct Proposed<'a> {
    /// The branch checked out.
    pub(crate) branch: &'a str,
    /// The branch the pull request targets.
    pub(crate) base: &'a str,
    /// The remote the base comes from.
    pub(crate) remote: &'a str,
    /// The repository `remote` points at.
    pub(crate) upstream: &'a RepoUrl,
    /// A remote to push to instead.
    pub(crate) push_remote: Option<&'a str>,
    /// Open it as a draft.
    pub(crate) draft: bool,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// Push the branch and open its pull request.
pub(crate) fn propose(ctx: &Context<'_>, proposed: &Proposed<'_>) -> Result<Pushed, Report> {
    let sleep = std::thread::sleep;
    ReqwestHttp::new().map_err(failure).and_then(|http| {
        let client = ForgeClient::new(&http as &dyn Http, ctx.env, &OsRunner, &sleep);
        push_and_open(ctx, &client, proposed)
    })
}

fn push_and_open(
    ctx: &Context<'_>,
    client: &ForgeClient<'_>,
    proposed: &Proposed<'_>,
) -> Result<Pushed, Report> {
    let upstream = proposed.upstream;
    let kind = client.kind(upstream).map_err(failure)?;
    let (target, head) = if let Some(other) = proposed.push_remote {
        let head = forge_repo(ctx, other).ok_or_else(|| {
            Report::error(
                Exit::Failure,
                format!("the remote {other} is not on a forge"),
            )
        })?;
        (other.to_owned(), head)
    } else if client.can_push(upstream, kind).map_err(failure)? {
        (proposed.remote.to_owned(), upstream.clone())
    } else {
        let fork = client.fork(upstream, kind).map_err(failure)?;
        let key = format!("remote.{}.url", proposed.remote);
        let url = output_of(&git(ctx).args(["config", "--get", &key])).unwrap_or_default();
        (push_url(&url, &fork), fork)
    };
    let refspec = format!("HEAD:refs/heads/{}", proposed.branch);
    super::run_checked(
        &git(ctx).args(["push", "--quiet", &target, &refspec]),
        "git push",
    )
    .map_err(|e| Report::error(Exit::Failure, e))?;
    let proposal = Proposal {
        head: &head,
        branch: proposed.branch,
        base: proposed.base,
        title: &proposed.title,
        body: &proposed.body,
        draft: proposed.draft,
    };
    let opened = client
        .open_pull_request(upstream, kind, &proposal)
        .map_err(failure)?;
    Ok(Pushed {
        to: target,
        opened,
        draft: proposed.draft,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_push_urls_follow_the_remote() {
        let fork = RepoUrl::parse("https://github.com/me/pos").unwrap();
        assert_eq!(
            push_url("git@github.com:OCA/pos.git", &fork),
            "git@github.com:me/pos.git"
        );
        assert_eq!(
            push_url("ssh://git@github.com/OCA/pos", &fork),
            "git@github.com:me/pos.git"
        );
        assert_eq!(
            push_url("https://github.com/OCA/pos", &fork),
            "https://github.com/me/pos.git"
        );
    }
}
