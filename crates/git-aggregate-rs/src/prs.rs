//! `show-closed-prs` and `show-all-prs`: the state upstream of every pull
//! request (or merge request) a `repos.yaml` merges, as git-aggregator
//! reports it: `<url> in state <state> (merged|not merged)`.

use std::fmt::Write as _;
use std::io::{self, Write};

use odx_config::compat::parse_repos;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, ForgeKind, PrState, RepoUrl, ReqwestHttp};

use crate::aggregate::{Log, load_text};
use crate::dirmatch::match_dir;
use crate::{Cli, Context};

/// The pull request number of a merged ref: `refs/pull/<n>/head`,
/// `pull/<n>/head` or `refs/merge-requests/<n>/head`.
fn number(reference: &str) -> Option<u64> {
    let rest = reference.strip_prefix("refs/").unwrap_or(reference);
    let rest = rest
        .strip_prefix("pull/")
        .or_else(|| rest.strip_prefix("merge-requests/"))?;
    rest.strip_suffix("/head")?.parse().ok()
}

/// The pull request's page, as git-aggregator prints it.
fn page(repo: &RepoUrl, kind: ForgeKind, number: u64) -> String {
    let base = format!("{}/{}", repo.origin(), repo.path);
    match kind {
        ForgeKind::Github => format!("{base}/pull/{number}"),
        ForgeKind::Gitlab => format!("{base}/-/merge_requests/{number}"),
        ForgeKind::Gitea => format!("{base}/pulls/{number}"),
    }
}

/// Run `show-closed-prs` (`all` false) or `show-all-prs`.
///
/// # Errors
///
/// When writing to `out` or `err` fails.
pub(crate) fn show(
    cli: &Cli,
    ctx: &Context<'_>,
    all: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> io::Result<Exit> {
    let mut log = Log::new(cli.log_level);
    let repos = load_text(cli, ctx, &mut log)
        .and_then(|text| parse_repos(&text).map_err(|e| format!("{}: {e}", cli.config.display())));
    let exit = match repos {
        Ok(file) => {
            let sleep = std::thread::sleep;
            let http = ReqwestHttp::new();
            let mut listed = String::new();
            for repo in file.repos.iter().filter(|repo| {
                cli.dirmatch
                    .as_deref()
                    .is_none_or(|pattern| match_dir(&ctx.dir, &repo.dir, pattern))
            }) {
                for (merge, number) in repo
                    .merges
                    .iter()
                    .filter_map(|merge| number(&merge.reference).map(|n| (merge, n)))
                {
                    let answer = RepoUrl::parse(&merge.url)
                        .ok_or_else(|| "not on a forge".to_owned())
                        .and_then(|url| {
                            let http = http.as_ref().map_err(ToString::to_string);
                            http.and_then(|http| {
                                let client = ForgeClient::new(http, ctx.env, &OsRunner, &sleep);
                                client
                                    .kind(&url)
                                    .and_then(|kind| {
                                        client
                                            .pull_request(&url, kind, number)
                                            .map(|pr| (page(&url, kind, number), pr))
                                    })
                                    .map_err(|e| e.to_string())
                            })
                        });
                    match answer {
                        Ok((page, pr)) if all || pr.state != PrState::Open => {
                            let (state, merged) = match pr.state {
                                PrState::Open => ("open", "not merged"),
                                PrState::Merged => ("closed", "merged"),
                                PrState::Closed => ("closed", "not merged"),
                            };
                            let _ = writeln!(listed, "{page} in state {state} ({merged})");
                        }
                        Ok(_) => {}
                        Err(message) => {
                            log.warning(&format!("{} {}: {message}", merge.url, merge.reference));
                        }
                    }
                }
            }
            out.write_all(listed.as_bytes())?;
            Exit::Success
        }
        Err(message) => {
            log.error(&message);
            Exit::Failure
        }
    };
    err.write_all(odx_core::redact(&log.text).as_bytes())?;
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use odx_core::fake::MapEnv;
    use odx_core::system::OsFs;

    #[test]
    fn unreachable_and_local_remotes_are_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("repos.yaml"),
            "./x:\n  remotes:\n    x: /srv/git/x\n    y: http://127.0.0.1:1/o/r.git\n  merges:\n    - x 17.0\n    - x refs/pull/1/head\n    - y refs/pull/2/head\n  target: x agg\n",
        )
        .unwrap();
        let env = MapEnv::default();
        let ctx = Context {
            fs: &OsFs,
            env: &env,
            dir: tmp.path().to_path_buf(),
        };
        for all in [false, true] {
            let cli = Cli::try_parse_from(["git-aggregate-rs", "-d", "./x"]).unwrap();
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let exit = show(&cli, &ctx, all, &mut out, &mut err).unwrap();
            let err = String::from_utf8(err).unwrap();
            assert_eq!((exit, out), (Exit::Success, Vec::new()));
            assert!(
                err.contains("/srv/git/x refs/pull/1/head: not on a forge"),
                "{err}"
            );
            assert!(err.contains("refs/pull/2/head: request failed"), "{err}");
        }
    }

    #[test]
    fn numbers_and_pages() {
        assert_eq!(number("refs/pull/12/head"), Some(12));
        assert_eq!(number("pull/7/head"), Some(7));
        assert_eq!(number("refs/merge-requests/3/head"), Some(3));
        assert_eq!(number("17.0"), None);
        assert_eq!(number("refs/pull/x/head"), None);
        assert_eq!(number("refs/pull/1/merge"), None);
        let repo = RepoUrl::parse("https://gitlab.com/g/r.git").unwrap();
        assert_eq!(
            page(&repo, ForgeKind::Gitlab, 3),
            "https://gitlab.com/g/r/-/merge_requests/3"
        );
        assert_eq!(
            page(&repo, ForgeKind::Gitea, 3),
            "https://gitlab.com/g/r/pulls/3"
        );
        assert_eq!(
            page(&repo, ForgeKind::Github, 3),
            "https://gitlab.com/g/r/pull/3"
        );
    }
}
