//! `odx status` (alias `odx track`): where every aggregated pull request
//! and every package from a pull request stands upstream.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use odx_aggregate::{Planned, plan_config};
use odx_config::lock::MergeStatus;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{
    CachedHttp, ForgeClient, ForgeError, ForgeKind, PrState, PullRequest, RepoUrl, ReqwestHttp,
};
use odx_git::{Forge, Merger, Source};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::{Prepared, prepare};
use crate::Context;
use crate::cli::{GlobalArgs, StatusArgs, StatusSort};

/// A pull request the project uses, with what `odx.lock` recorded for it.
struct Tracked {
    /// `<repository> #<n>` or `<distribution> #<n>`.
    name: String,
    /// The distribution, for a package from a pull request.
    dist: Option<String>,
    url: String,
    forge: Forge,
    pr: u32,
    /// The locked commit, its patch-id (merges only) and base, and whether
    /// the guard rail keeps it pinned.
    locked: Option<Locked>,
}

struct Locked {
    sha: String,
    patch_id: Option<String>,
    /// The locked base commit, and the cache ref of the base branch.
    base: Option<(String, String)>,
    pinned: bool,
}

/// One line of the report.
pub(crate) struct Row {
    /// `<repository> #<n>` or `<distribution> #<n>`.
    pub(crate) name: String,
    /// The distribution, for a package from a pull request.
    pub(crate) dist: Option<String>,
    pub(crate) url: String,
    pub(crate) pr: u32,
    pub(crate) upstream: Option<PullRequest>,
    error: Option<String>,
    pub(crate) head: &'static str,
    new_commits: Option<usize>,
    pub(crate) advice: &'static str,
}

/// `odx status`.
pub(crate) fn run_status(args: &StatusArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    status(args, global, ctx).unwrap_or_else(|report| report)
}

/// The pull requests to report: every merge from a pull request (unless
/// `track_upstream = false`) and every locked package from one.
fn tracked(prepared: &Prepared, planned: &[Planned]) -> Vec<Tracked> {
    let config = &prepared.project.config;
    let mut tracked = Vec::new();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let entry = prepared
            .locked()
            .iter()
            .find(|entry| entry.name == repo.name);
        for merge in &configured.merges {
            let Some(pr) = merge.pr.filter(|_| merge.track_upstream != Some(false)) else {
                continue;
            };
            let locked = entry.and_then(|entry| {
                entry
                    .merges
                    .iter()
                    .find(|locked| locked.source.matches(merge))
                    .map(|locked| Locked {
                        sha: locked.sha.clone(),
                        patch_id: Some(locked.patch_id.clone()),
                        base: Some((
                            entry.base_sha.clone(),
                            repo.base
                                .source
                                .local_ref(repo.forge, repo.base.remote.as_deref()),
                        )),
                        pinned: locked.status == MergeStatus::Pinned,
                    })
            });
            tracked.push(Tracked {
                name: format!("{} #{pr}", repo.name),
                dist: None,
                url: repo.url.clone(),
                forge: repo.forge,
                pr,
                locked,
            });
        }
    }
    let packages = prepared.lock.iter().flat_map(|lock| &lock.packages);
    for (dist, package) in packages {
        if let Some(pr) = package.source.pr {
            tracked.push(Tracked {
                name: format!("{dist} #{pr}"),
                dist: Some(dist.clone()),
                url: package.url.clone(),
                forge: Forge::detect(&package.url),
                pr,
                locked: Some(Locked {
                    sha: package.sha.clone(),
                    patch_id: None,
                    base: None,
                    pinned: false,
                }),
            });
        }
    }
    tracked
}

/// How the pull request's head relates to the locked commit, judged from
/// the cache, and how many commits it has on top of it.
fn head_state(
    merger: &Merger,
    repo: &Path,
    tracked: &Tracked,
    upstream_head: Option<&str>,
) -> (&'static str, Option<usize>) {
    let Some(locked) = &tracked.locked else {
        return ("not locked", None);
    };
    let cached = merger.commit(repo, &Source::Pr(tracked.pr).local_ref(tracked.forge, None));
    let Some(head) = upstream_head.map(str::to_owned).or(cached) else {
        return ("unknown", None);
    };
    if head == locked.sha {
        return ("same", None);
    }
    if merger.commit(repo, &head).is_none() {
        // Not fetched yet: only the forge knows it moved.
        return ("changed", None);
    }
    // A rebased head is compared with the base it was rebased on.
    let same_patch = locked
        .base
        .as_ref()
        .zip(locked.patch_id.as_deref())
        .is_some_and(|((base_sha, base_ref), patch_id)| {
            let current = merger.commit(repo, base_ref);
            let base = current.as_deref().unwrap_or(base_sha);
            merger
                .patch_id(repo, base, &head)
                .is_ok_and(|id| id == patch_id)
        });
    if same_patch {
        ("rebased-same-patch", None)
    } else if merger.contains(repo, &head, &locked.sha) {
        ("changed", merger.count(repo, &locked.sha, &head))
    } else {
        // Force-pushed: the locked commit survives only in the cache and
        // the vault.
        ("orphaned", None)
    }
}

fn advice(state: Option<PrState>, head: &str, pinned: bool) -> &'static str {
    match (state, head) {
        (Some(PrState::Merged), _) => "merged upstream: drop the override once the base has it",
        (Some(PrState::Closed), _) => "closed without merge: decide whether to keep it",
        (_, "not locked") => "not locked yet: run odx lock",
        (_, "rebased-same-patch") => "rebased, same change: odx lock --update adopts it",
        (_, "changed" | "orphaned") if pinned => {
            "pin kept by the guard rail; re-check with odx lock --update"
        }
        (_, "changed") => "new commits upstream: review, then odx lock --update",
        (_, "orphaned") => "force-pushed upstream: review the new head",
        _ => "",
    }
}

/// Every tracked pull request with its state upstream and its head.
pub(crate) fn rows(
    prepared: &Prepared,
    planned: &[Planned],
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Vec<Row> {
    let tracked = tracked(prepared, planned);
    let merger = Merger::new(Arc::new(OsRunner));
    let http =
        ReqwestHttp::new().map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let sleep = std::thread::sleep;
    let mut kinds: BTreeMap<String, Result<ForgeKind, ForgeError>> = BTreeMap::new();
    let mut rows: Vec<Row> = Vec::new();
    for item in &tracked {
        let answer = if global.offline {
            Err("offline".to_owned())
        } else {
            RepoUrl::parse(&item.url)
                .ok_or_else(|| "not on a forge".to_owned())
                .and_then(|repo| {
                    let http = http.as_ref().map_err(ToString::to_string);
                    http.and_then(|http| {
                        let client = ForgeClient::new(http, ctx.env, &OsRunner, &sleep);
                        let kind = kinds
                            .entry(repo.host.clone())
                            .or_insert_with(|| client.kind(&repo))
                            .clone();
                        kind.and_then(|kind| client.pull_request(&repo, kind, u64::from(item.pr)))
                            .map_err(|e| e.to_string())
                    })
                })
        };
        let (upstream, error) = match answer {
            Ok(pr) => (Some(pr), None),
            Err(e) => (None, Some(e)),
        };
        let repo = prepared.cache.repo_dir(&item.url);
        let (head, new_commits) = head_state(
            &merger,
            &repo,
            item,
            upstream.as_ref().map(|pr| pr.head_sha.as_str()),
        );
        let pinned = item.locked.as_ref().is_some_and(|locked| locked.pinned);
        rows.push(Row {
            name: item.name.clone(),
            dist: item.dist.clone(),
            url: item.url.clone(),
            pr: item.pr,
            advice: advice(upstream.as_ref().map(|pr| pr.state), head, pinned),
            upstream,
            error,
            head,
            new_commits,
        });
    }
    rows
}

fn status(args: &StatusArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    // Loaded configurations are validated, so planning all of them works.
    let planned =
        plan_config(&prepared.project.config, &prepared.project.root, None).unwrap_or_default();
    let merger = Merger::new(Arc::new(OsRunner));
    let mut report = Report::default();
    let mut rows = rows(&prepared, &planned, global, ctx);
    sort(&mut rows, args.sort_by);
    let modified = modified(&prepared, &planned, &merger);
    let merged_upstream = rows
        .iter()
        .filter(|row| {
            row.upstream
                .as_ref()
                .is_some_and(|pr| pr.state == PrState::Merged)
        })
        .count();
    report.out = if global.json {
        json(&rows, &modified)
    } else {
        text(&rows, &modified)
    };
    // Offline, no pull request is read on purpose.
    let unread = rows.iter().filter(|_| !global.offline);
    for row in unread {
        if let Some(error) = &row.error {
            report.warnings.push(odx_config::Problem {
                key: row.name.clone(),
                message: format!("cannot read the pull request: {error}"),
            });
        }
    }
    if args.fail_on_merged && merged_upstream > 0 {
        report.exit = Exit::Failure;
        report.error = Some(format!(
            "{merged_upstream} pull requests are merged upstream (--fail-on-merged)"
        ));
    }
    Ok(report)
}

/// Aggregated working trees with local changes: name, directory, changes.
fn modified(
    prepared: &Prepared,
    planned: &[Planned],
    merger: &Merger,
) -> Vec<(String, String, usize)> {
    let root = &prepared.project.root;
    planned
        .iter()
        .map(|repo| (repo, merger.modified(&repo.target_dir)))
        .filter(|(_, changes)| !changes.is_empty())
        .map(|(repo, changes)| {
            let dir = repo
                .target_dir
                .strip_prefix(root)
                .unwrap_or(&repo.target_dir)
                .display()
                .to_string();
            (repo.name.clone(), dir, changes.len())
        })
        .collect()
}

/// Order rows by name, by state (merged, closed, open, unknown), or by
/// merge date (merged first, oldest first).
fn sort(rows: &mut [Row], by: StatusSort) {
    let rank = |row: &Row| match row.upstream.as_ref().map(|pr| pr.state) {
        Some(PrState::Merged) => 0,
        Some(PrState::Closed) => 1,
        Some(PrState::Open) => 2,
        None => 3,
    };
    match by {
        StatusSort::Name => rows.sort_by(|a, b| (&a.name, a.pr).cmp(&(&b.name, b.pr))),
        StatusSort::Status => rows.sort_by_key(|row| (rank(row), row.name.clone())),
        StatusSort::Date => rows.sort_by_key(|row| {
            let merged_at = row.upstream.as_ref().and_then(|pr| pr.merged_at.clone());
            (merged_at.is_none(), merged_at, row.name.clone())
        }),
    }
}

fn text(rows: &[Row], modified: &[(String, String, usize)]) -> String {
    let mut out = String::new();
    for row in rows {
        let state = row
            .upstream
            .as_ref()
            .map_or("unknown", |pr| pr.state.name());
        let _ = write!(out, "{}: {state}, head {}", row.name, row.head);
        if let Some(count) = row.new_commits {
            let _ = write!(out, " (+{count} commits)");
        }
        if let Some(pr) = &row.upstream {
            if let Some(date) = &pr.merged_at {
                let _ = write!(out, ", merged {}", date.get(..10).unwrap_or(date));
            }
            if !pr.labels.is_empty() {
                let _ = write!(out, ", labels: {}", pr.labels.join(", "));
            }
            let _ = write!(out, " - {}", pr.title);
        }
        out.push('\n');
        if !row.advice.is_empty() {
            let _ = writeln!(out, "  {}", row.advice);
        }
    }
    if rows.is_empty() {
        out.push_str("no pull requests to track\n");
    }
    for (name, dir, changes) in modified {
        let _ = writeln!(out, "{name}: {changes} local changes in {dir}");
    }
    out
}

/// What `odx --json status` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx status")]
pub(crate) struct StatusJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// Every tracked pull request.
    pull_requests: Vec<PullRequestJson<'a>>,
    /// Aggregated working trees with local changes.
    modified: Vec<ModifiedJson<'a>>,
}

/// A tracked pull request.
#[derive(Debug, Serialize, JsonSchema)]
struct PullRequestJson<'a> {
    /// `<repository> #<n>` or `<distribution> #<n>`.
    name: &'a str,
    /// The repository URL.
    url: &'a str,
    /// The pull request number.
    pr: u32,
    /// `open`, `merged` or `closed`; null when the forge could not be asked.
    state: Option<&'a str>,
    /// Its title upstream.
    title: Option<&'a str>,
    /// When it was merged.
    merged_at: Option<&'a str>,
    /// Its labels upstream.
    labels: &'a [String],
    /// How its head relates to the locked commit: `same`,
    /// `rebased-same-patch`, `changed`, `orphaned` or `not locked`.
    head: &'a str,
    /// Commits on the head the lock does not have, when the cache knows.
    new_commits: Option<usize>,
    /// What to do about it; empty when nothing.
    advice: &'a str,
    /// Why the forge could not be asked.
    error: Option<&'a str>,
}

/// A working tree with local changes.
#[derive(Debug, Serialize, JsonSchema)]
struct ModifiedJson<'a> {
    /// The repository.
    name: &'a str,
    /// Its working tree, relative to the project root.
    dir: &'a str,
    /// Changed files.
    changes: usize,
}

fn json<'a>(rows: &'a [Row], modified: &'a [(String, String, usize)]) -> String {
    let value = StatusJson {
        schema: 1,
        pull_requests: rows
            .iter()
            .map(|row| PullRequestJson {
                name: &row.name,
                url: &row.url,
                pr: row.pr,
                state: row.upstream.as_ref().map(|pr| pr.state.name()),
                title: row.upstream.as_ref().map(|pr| pr.title.as_str()),
                merged_at: row.upstream.as_ref().and_then(|pr| pr.merged_at.as_deref()),
                labels: row.upstream.as_ref().map_or(&[], |pr| pr.labels.as_slice()),
                head: row.head,
                new_commits: row.new_commits,
                advice: row.advice,
                error: row.error.as_deref(),
            })
            .collect(),
        modified: modified
            .iter()
            .map(|(name, dir, changes)| ModifiedJson {
                name,
                dir,
                changes: *changes,
            })
            .collect(),
    };
    super::to_json(&value)
}
