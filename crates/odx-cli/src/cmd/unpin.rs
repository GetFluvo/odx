//! `odx migrate unpin`: the return trip of a pin. Once every pull request
//! `test-requirements.txt` pins is merged and released for the target
//! series, the `[DON'T MERGE] test-requirements.txt` commit is dropped;
//! `--push` sends the branch back to its pull request, and `--merge`
//! merges that pull request once its checks are green. Nothing is watched
//! in the background, and nothing is merged without `--merge`.

use std::fmt::Write as _;
use std::path::PathBuf;

use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{CiState, ForgeClient, Http, PrState, RepoUrl, ReqwestHttp};
use odx_index::sources::package_index;

use super::pin::{self, SUBJECT};
use super::port::{parse_pr, remote_for};
use super::propose::{forge_repo, git};
use super::{Report, output_of, run_checked};
use crate::Context;
use crate::cli::{GlobalArgs, MigrateUnpinArgs};

/// The commit that removes the pins when the pin commit cannot be dropped.
const REMOVED: &str = "[REM] test-requirements.txt: pins of released dependencies";

fn failed(message: String) -> Report {
    Report::error(Exit::Failure, message)
}

/// A line of `test-requirements.txt` pinning an addon to a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pinned {
    addon: String,
    /// The repository's URL, without `.git`.
    repository: String,
    number: u64,
    line: String,
}

/// The pin on `line`, if it is one.
fn pinned(line: &str) -> Option<Pinned> {
    let (name, rest) = line.split_once(" @ git+")?;
    let (repository, rest) = rest.split_once(".git@")?;
    let (reference, subdirectory) = rest.split_once("#subdirectory=").unwrap_or((rest, ""));
    let number = reference
        .strip_prefix("refs/pull/")
        .or_else(|| reference.strip_prefix("refs/merge-requests/"))?
        .strip_suffix("/head")?
        .parse()
        .ok()?;
    let addon = if subdirectory.is_empty() {
        name.trim()
            .strip_prefix("odoo-addon-")
            .unwrap_or_else(|| name.trim())
            .replace('-', "_")
    } else {
        subdirectory.trim().to_owned()
    };
    Some(Pinned {
        addon,
        repository: repository.to_owned(),
        number,
        line: line.to_owned(),
    })
}

/// Where a pinned pull request stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Upstream {
    Open,
    Closed,
    /// Merged, not released for the target series yet.
    Merged,
    Released,
    /// The forge or the package index could not tell.
    Unknown(String),
}

impl Upstream {
    fn describe(&self, to: &str) -> String {
        match self {
            Self::Open => "open".to_owned(),
            Self::Closed => "closed without being merged: pin another pull request".to_owned(),
            Self::Merged => format!("merged, not released for {to} yet"),
            Self::Released => format!("merged and released for {to}"),
            Self::Unknown(why) => format!("unknown: {why}"),
        }
    }

    const fn key(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Merged => "merged",
            Self::Released => "released",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// A forge client over `http`.
fn client<'a>(ctx: &'a Context<'a>, http: &'a dyn Http) -> ForgeClient<'a> {
    ForgeClient::new(http, ctx.env, &OsRunner, &std::thread::sleep)
}

/// Where `pin` stands for `to`.
fn upstream(ctx: &Context<'_>, http: &dyn Http, pin: &Pinned, to: &str) -> Upstream {
    let Some(repo) = RepoUrl::parse(&pin.repository) else {
        return Upstream::Unknown(format!("{} is no forge repository", pin.repository));
    };
    let client = client(ctx, http);
    let found = client
        .kind(&repo)
        .and_then(|kind| client.pull_request(&repo, kind, pin.number));
    match found.map(|pr| pr.state) {
        Err(e) => Upstream::Unknown(e.to_string()),
        Ok(PrState::Open) => Upstream::Open,
        Ok(PrState::Closed) => Upstream::Closed,
        Ok(PrState::Merged) => match package_index(http, &super::deps::pypi(ctx), &pin.addon, to) {
            Ok(Some(_)) => Upstream::Released,
            Ok(None) => Upstream::Merged,
            Err(e) => Upstream::Unknown(e),
        },
    }
}

/// The top of the worktree.
fn root(ctx: &Context<'_>) -> PathBuf {
    PathBuf::from(output_of(&git(ctx).args(["rev-parse", "--show-toplevel"])).unwrap_or_default())
}

/// The pull request of the branch checked out: `--pr`, else the one `odx
/// migrate pr` or `odx migrate update` recorded.
fn pull_request(
    ctx: &Context<'_>,
    args: &MigrateUnpinArgs,
    branch: &str,
) -> Result<Option<(RepoUrl, u64)>, Report> {
    let Some(pr) = &args.pr else {
        return Ok(pin::recorded_pull_request(ctx, branch));
    };
    let Some((path, number)) = parse_pr(pr) else {
        return Err(Report::error(
            Exit::Usage,
            format!("{pr:?}: name the pull request as <owner>/<repo>#<number>"),
        ));
    };
    let repo = remote_for(ctx, path)
        .and_then(|remote| forge_repo(ctx, &remote))
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                format!("no remote of this clone points at {path} on a forge"),
            )
        })?;
    Ok(Some((repo, number)))
}

/// What became of the pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dropped {
    /// The pin commit was dropped from the branch.
    Commit,
    /// The pins were removed in a commit of their own.
    Lines,
}

/// Remove `pins` from the branch: drop the pin commit when it holds them
/// and nothing after it conflicts, else remove the lines in a commit.
fn drop_pins(ctx: &Context<'_>, pins: &[Pinned]) -> Result<Dropped, Report> {
    // The guard in front of `rebase` and `reset --hard`: when git cannot
    // tell, nothing is changed.
    let status = output_of(&git(ctx).args(["status", "--porcelain", "--untracked-files=no"]))
        .map_err(failed)?;
    if !status.is_empty() {
        return Err(failed(
            "the worktree has uncommitted changes: commit or stash them first".to_owned(),
        ));
    }
    let history = output_of(&git(ctx).args([
        "log",
        "--max-count=500",
        "--format=%H %s",
        "HEAD",
        "--",
        "test-requirements.txt",
    ]))
    .unwrap_or_default();
    let commit = history
        .lines()
        .find_map(|line| {
            line.split_once(' ')
                .filter(|(_, subject)| *subject == SUBJECT)
        })
        .map(|(sha, _)| sha.to_owned());
    if let Some(sha) = commit {
        // The commit goes only when that leaves the file as it is now
        // without the pins: a pin commit can hold more, or less.
        let file = root(ctx).join("test-requirements.txt");
        let now = std::fs::read_to_string(&file).unwrap_or_default();
        let wanted = without(&now, pins);
        let before = output_of(&git(ctx).args(["rev-parse", "HEAD"])).unwrap_or_default();
        let parent = format!("{sha}^");
        let rebased = run_checked(
            &git(ctx).args(["rebase", "--quiet", "--onto", &parent, &sha]),
            "git rebase",
        );
        let left = std::fs::read_to_string(&file).unwrap_or_default();
        if rebased.is_ok() && left.lines().eq(wanted.iter().copied()) {
            return Ok(Dropped::Commit);
        }
        if rebased.is_err() {
            let _ = output_of(&git(ctx).args(["rebase", "--abort"]));
        } else {
            let _ = output_of(&git(ctx).args(["reset", "--quiet", "--hard", &before]));
        }
    }
    remove_lines(ctx, pins).map(|()| Dropped::Lines)
}

/// The lines of `text` that are none of `pins`.
fn without<'a>(text: &'a str, pins: &[Pinned]) -> Vec<&'a str> {
    text.lines()
        .filter(|line| !pins.iter().any(|pin| pin.line == *line))
        .collect()
}

/// Remove the lines of `pins` from `test-requirements.txt` in a commit.
fn remove_lines(ctx: &Context<'_>, pins: &[Pinned]) -> Result<(), Report> {
    let file = root(ctx).join("test-requirements.txt");
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let kept = without(&text, pins);
    let path = file.to_string_lossy().into_owned();
    let staged = if kept.is_empty() {
        run_checked(&git(ctx).args(["rm", "--quiet", "--", &path]), "git rm")
    } else {
        // A file that cannot be written leaves nothing to commit: that
        // commit fails.
        let _ = std::fs::write(&file, format!("{}\n", kept.join("\n")));
        run_checked(&git(ctx).args(["add", "--", &path]), "git add")
    };
    staged
        .and_then(|()| {
            run_checked(
                &git(ctx).args(["commit", "--quiet", "--no-verify", "-m", REMOVED]),
                "git commit",
            )
        })
        .map_err(failed)
}

/// Where the branch's pull request comes from: the target, the branch
/// there and its commit (empty when it does not exist), refused when that
/// has commits the branch does not.
fn push_target(ctx: &Context<'_>, branch: &str) -> Result<(String, String, String), Report> {
    let (to, head) = pin::pushed_to(ctx, branch).ok_or_else(|| {
        failed(format!(
            "odx does not know where {branch} is pushed (`odx migrate pr` records it); push it with git"
        ))
    })?;
    let there = output_of(&git(ctx).args(["ls-remote", &to, &format!("refs/heads/{head}")]))
        .map_err(failed)?;
    let there = there
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned();
    let ours = there.is_empty()
        || output_of(&git(ctx).args(["merge-base", "--is-ancestor", &there, "HEAD"])).is_ok();
    if !ours {
        return Err(failed(format!(
            "{to} {head} has commits this branch does not: fetch and rebase on them first"
        )));
    }
    Ok((to, head, there))
}

/// Push the branch to `target`, replacing what is there only when it is
/// still what [`push_target`] saw.
fn push(ctx: &Context<'_>, (to, head, there): &(String, String, String)) -> Result<String, Report> {
    let lease = format!("--force-with-lease=refs/heads/{head}:{there}");
    let refspec = format!("HEAD:refs/heads/{head}");
    run_checked(
        &git(ctx).args(["push", "--quiet", &lease, to, &refspec]),
        "git push",
    )
    .map(|()| format!("{to} {head}"))
    .map_err(failed)
}

/// What `odx --json migrate unpin` prints: what it found and did.
#[derive(Debug, Default, serde::Serialize, schemars::JsonSchema)]
#[schemars(title = "odx migrate unpin")]
pub(crate) struct Unpinned {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// `no pins`, `waiting`, `ready` or `unpinned`.
    status: &'static str,
    /// The pins in `test-requirements.txt`.
    pins: Vec<PinJson>,
    /// How the pins went: `commit` (the pin commit dropped) or `lines`
    /// (removed in a commit).
    dropped: Option<&'static str>,
    /// Where `--push` sent the branch.
    pushed_to: Option<String>,
    /// The page of the branch's pull request.
    pull_request: Option<String>,
    /// Its checks: `green`, `red`, `pending` or `unknown`.
    ci: Option<&'static str>,
    /// Its failing checks.
    failing: Vec<String>,
    /// Whether `--merge` merged it.
    merged: bool,
    /// What the command prints without `--json`.
    #[serde(skip)]
    text: String,
}

/// A dependency pinned to its migration pull request.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct PinJson {
    /// The addon.
    addon: String,
    /// The repository's URL.
    repository: String,
    /// The pull request's number.
    number: u64,
    /// Where it stands upstream: `open`, `closed`, `merged`, `released`
    /// or `unknown`.
    state: &'static str,
}

const fn ci_key(state: CiState) -> &'static str {
    match state {
        CiState::Green => "green",
        CiState::Red => "red",
        CiState::Pending => "pending",
        CiState::Unknown => "unknown",
    }
}

/// Run `odx migrate unpin`.
pub(crate) fn run(
    args: &MigrateUnpinArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let http = ReqwestHttp::new()
        .ok()
        .filter(|_| !global.offline)
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                "odx migrate unpin asks the forges and the package index; it cannot run offline"
                    .to_owned(),
            )
        })?;
    let http = &http as &dyn Http;
    let branch = output_of(&git(ctx).args(["branch", "--show-current"])).unwrap_or_default();
    let pull_request = pull_request(ctx, args, &branch)?;
    let text = std::fs::read_to_string(root(ctx).join("test-requirements.txt")).unwrap_or_default();
    let pins: Vec<Pinned> = text.lines().filter_map(pinned).collect();
    let mut out = Unpinned {
        schema: 1,
        ..Unpinned::default()
    };
    if pins.is_empty() {
        out.status = "no pins";
        out.text.push_str("no pins in test-requirements.txt\n");
    } else {
        settle(ctx, http, args, &branch, &pins, &mut out)?;
    }
    if let Some((repo, number)) = &pull_request {
        checks(ctx, http, args, (repo, *number), &pins, &mut out)?;
    }
    if out.status == "unpinned" && out.pushed_to.is_none() {
        out.text
            .push_str("next: `odx migrate unpin --push` sends the branch to its pull request\n");
    }
    Ok(Report {
        out: if global.json {
            super::to_json(&out)
        } else {
            out.text
        },
        ..Report::default()
    })
}

/// Where `pins` stand, and once every one is released, the pins dropped
/// and the branch pushed when `--push` asks.
fn settle(
    ctx: &Context<'_>,
    http: &dyn Http,
    args: &MigrateUnpinArgs,
    branch: &str,
    pins: &[Pinned],
    out: &mut Unpinned,
) -> Result<(), Report> {
    let to = args
        .to
        .clone()
        .or_else(|| branch.split_once("-mig-").map(|(to, _)| to.to_owned()))
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                format!("{branch:?} names no series: name the target with --to"),
            )
        })?;
    let states: Vec<Upstream> = pins
        .iter()
        .map(|pin| upstream(ctx, http, pin, &to))
        .collect();
    out.text.push_str("pins in test-requirements.txt:\n");
    for (pin, state) in pins.iter().zip(&states) {
        let page = pin
            .repository
            .split_once("://")
            .map_or(&*pin.repository, |(_, rest)| rest);
        let _ = writeln!(
            out.text,
            "  {}: {page}#{} {}",
            pin.addon,
            pin.number,
            state.describe(&to)
        );
        out.pins.push(PinJson {
            addon: pin.addon.clone(),
            repository: pin.repository.clone(),
            number: pin.number,
            state: state.key(),
        });
    }
    if states.iter().any(|state| *state != Upstream::Released) {
        out.status = "waiting";
        out.text.push_str(
            "keep the pins until every pull request is merged and released; run `odx migrate unpin` again then\n",
        );
    } else if args.check {
        out.status = "ready";
        out.text
            .push_str("every pin can go: `odx migrate unpin` drops them\n");
    } else {
        let target = if args.push {
            Some(push_target(ctx, branch)?)
        } else {
            None
        };
        let dropped = drop_pins(ctx, pins)?;
        out.status = "unpinned";
        out.dropped = Some(match dropped {
            Dropped::Commit => {
                let _ = writeln!(out.text, "dropped the commit {SUBJECT}");
                "commit"
            }
            Dropped::Lines => {
                let _ = writeln!(out.text, "removed the pins in a commit {REMOVED}");
                "lines"
            }
        });
        if let Some(target) = &target {
            let pushed = push(ctx, target)?;
            let _ = writeln!(out.text, "pushed to {pushed}");
            out.pushed_to = Some(pushed);
        }
    }
    Ok(())
}

/// The checks of the pull request on the branch's head, and the merge
/// when `--merge` asks for it and they allow it.
fn checks(
    ctx: &Context<'_>,
    http: &dyn Http,
    args: &MigrateUnpinArgs,
    (repo, number): (&RepoUrl, u64),
    pins: &[Pinned],
    out: &mut Unpinned,
) -> Result<(), Report> {
    let page = format!("{}/{}#{number}", repo.origin(), repo.path);
    out.pull_request = Some(page.clone());
    let client = client(ctx, http);
    let head = output_of(&git(ctx).args(["rev-parse", "HEAD"])).unwrap_or_default();
    let read = client.kind(repo).and_then(|kind| {
        let found = client.pull_request(repo, kind, number)?;
        let report = client.ci(repo, kind, &head)?;
        Ok((kind, found.head_sha == head, report))
    });
    let (kind, pushed, report) = match read {
        Ok(read) => read,
        Err(e) => {
            let _ = writeln!(out.text, "{page}: cannot read its checks: {e}");
            return if args.merge {
                Err(failed(format!("{}not merging {page}", out.text)))
            } else {
                Ok(())
            };
        }
    };
    out.ci = Some(ci_key(report.state));
    out.failing.clone_from(&report.failing);
    let _ = write!(out.text, "{page}: checks {}", ci_key(report.state));
    if !report.failing.is_empty() {
        let _ = write!(out.text, " ({} failing)", report.failing.join(", "));
    }
    if !pushed {
        out.text
            .push_str(", on a commit the pull request does not have yet");
    }
    out.text.push('\n');
    if !args.merge {
        return Ok(());
    }
    let refused = |why: &str| failed(format!("{}not merging {page}: {why}", out.text));
    if out.status != "unpinned" && !pins.is_empty() {
        return Err(refused("test-requirements.txt still pins pull requests"));
    }
    if !pushed {
        return Err(refused("push the branch first (`--push`)"));
    }
    if report.state != CiState::Green {
        return Err(refused("its checks are not green"));
    }
    client.merge_pull_request(repo, kind, number).map_err(|e| {
        refused(&format!(
            "{e}; on OCA, a maintainer merges with `/ocabot merge`"
        ))
    })?;
    out.merged = true;
    let _ = writeln!(out.text, "merged {page}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_in_requirement_lines() {
        let line = "odoo-addon-module_analysis @ git+https://github.com/OCA/server-tools.git@refs/pull/3631/head#subdirectory=module_analysis";
        assert_eq!(
            pinned(line),
            Some(Pinned {
                addon: "module_analysis".to_owned(),
                repository: "https://github.com/OCA/server-tools".to_owned(),
                number: 3631,
                line: line.to_owned(),
            })
        );
        let gitlab =
            pinned("odoo-addon-dep-x @ git+https://gitlab.com/g/r.git@refs/merge-requests/4/head")
                .unwrap();
        assert_eq!((gitlab.addon.as_str(), gitlab.number), ("dep_x", 4));
        let bare = pinned("dep_y @ git+https://github.com/o/r.git@refs/pull/2/head").unwrap();
        assert_eq!(bare.addon, "dep_y");
        for line in [
            "requests",
            "odoo-addon-x @ git+https://github.com/o/r",
            "odoo-addon-x @ git+https://github.com/o/r.git@main",
            "odoo-addon-x @ git+https://github.com/o/r.git@refs/pull/x/head",
            "odoo-addon-x @ git+https://github.com/o/r.git@refs/pull/2",
        ] {
            assert_eq!(pinned(line), None, "{line}");
        }
        for (state, key) in [
            (Upstream::Open, "open"),
            (Upstream::Closed, "closed"),
            (Upstream::Merged, "merged"),
            (Upstream::Released, "released"),
            (Upstream::Unknown("x".to_owned()), "unknown"),
        ] {
            assert_eq!(state.key(), key);
            assert_ne!(state.describe("19.0"), "");
        }
        for (state, key) in [
            (CiState::Green, "green"),
            (CiState::Red, "red"),
            (CiState::Pending, "pending"),
            (CiState::Unknown, "unknown"),
        ] {
            assert_eq!(ci_key(state), key);
        }
    }
}
