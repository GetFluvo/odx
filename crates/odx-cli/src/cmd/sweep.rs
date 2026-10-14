//! `odx port sweep --to <series> --from <series>,...`: every addon of the
//! target series checked against the older series, as a maintainer does
//! before a release; the false positives blacklisted on request.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use odx_core::system::OsRunner;
use odx_index::series_key;
use odx_port::analysis::{Class, Group, Query, analyse};
use odx_port::blacklist::{self, Blacklist};
use odx_port::git::Repo;
use odx_port::migrate::Worktree;

use super::Report;
use super::port::{failed, repository, settings, usage};
use super::propose::{Proposed, default_remote, forge_repo, opened_json, opened_text, propose};
use crate::Context;
use crate::cli::{GlobalArgs, SweepArgs};

/// What a sweep found for one addon on one source series.
#[derive(Debug, serde::Serialize)]
struct Row {
    addon: String,
    series: String,
    key: String,
    title: String,
    class: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// The reason a false positive is blacklisted with; `None` for what is
/// ported or already blacklisted.
fn reason(class: Class) -> Option<&'static str> {
    match class {
        Class::Migration => Some("migration commit; it cannot be ported"),
        Class::RepoWide => Some("repo-wide change attributed to every addon it touched"),
        Class::Translations => Some("translations only; Weblate syncs them"),
        Class::Portable | Class::Blacklisted => None,
    }
}

/// The addons on `target`, matching `glob`.
fn addons(repo: &Repo<'_>, ctx: &Context<'_>, target: &str, glob: Option<&str>) -> Vec<String> {
    let listing =
        super::output_of(&super::propose::git(ctx).args(["ls-tree", "--name-only", target]))
            .unwrap_or_default();
    listing
        .lines()
        .filter(|dir| glob.is_none_or(|pattern| git_aggregate_rs::fnmatch(dir, pattern)))
        .filter(|dir| {
            repo.show(target, &format!("{dir}/__manifest__.py"))
                .is_some()
        })
        .map(str::to_owned)
        .collect()
}

/// What a sweep found, and the blacklist entries per addon.
struct Swept {
    rows: Vec<Row>,
    entries: BTreeMap<String, Vec<(String, String)>>,
}

/// Every addon of `to` (matching `glob`) against every source series.
fn sweep(
    ctx: &Context<'_>,
    glob: Option<&str>,
    remote: &str,
    sources: &[String],
    to: &str,
) -> Result<Swept, Report> {
    let refname = |series: &str| format!("refs/remotes/{remote}/{series}");
    let settings = settings(ctx)?;
    let repository = repository(ctx, remote);
    let runner = OsRunner;
    let repo = Repo::new(&runner, &ctx.dir);
    let target = refname(to);
    let mut rows = Vec::new();
    let mut entries: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for addon in addons(&repo, ctx, &target, glob) {
        let listed = repo
            .show(&target, &blacklist::path(&addon))
            .and_then(|text| Blacklist::parse(&text).ok())
            .unwrap_or_default();
        for (at, source) in sources.iter().enumerate() {
            // What reached the target or a newer source series is there.
            let targets: Vec<String> = std::iter::once(target.clone())
                .chain(sources.iter().skip(at + 1).map(|series| refname(series)))
                .collect();
            let query = Query {
                source: &refname(source),
                targets: &targets,
                source_dir: &addon,
                target_dir: &addon,
                repository: &repository,
                blacklist: &listed,
            };
            // The series were just fetched: git reads them.
            let groups: Vec<Group> = analyse(&repo, &query, &settings).unwrap_or_default();
            for group in groups {
                let why = reason(group.class);
                if let Some(why) = why {
                    let mine = entries.entry(addon.clone()).or_default();
                    if !mine.iter().any(|(key, _)| *key == group.key) {
                        mine.push((group.key.clone(), why.to_owned()));
                    }
                }
                rows.push(Row {
                    addon: addon.clone(),
                    series: source.clone(),
                    key: group.key,
                    title: group.title,
                    class: group.class.name(),
                    reason: why.map(str::to_owned).or(group.reason),
                });
            }
        }
    }
    Ok(Swept { rows, entries })
}

/// Run `odx port sweep`.
pub(crate) fn run_sweep(
    args: &SweepArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    tree: &Worktree<'_>,
) -> Result<Report, Report> {
    let to = &args.to;
    let mut sources = args.from.clone();
    sources.sort_by_key(|series| series_key(series));
    sources.dedup();
    if let Some(newer) = sources
        .iter()
        .find(|series| series_key(series) >= series_key(to))
    {
        return Err(usage(format!("--from {newer} is not older than --to {to}")));
    }
    let remote = args.remote.clone().unwrap_or_else(|| default_remote(ctx));
    let refname = |series: &str| format!("refs/remotes/{remote}/{series}");
    let refspecs: Vec<String> = sources
        .iter()
        .chain(std::iter::once(to))
        .map(|series| format!("+refs/heads/{series}:{}", refname(series)))
        .collect();
    tree.fetch(&remote, &refspecs).map_err(failed)?;
    let swept = sweep(ctx, args.addon.as_deref(), &remote, &sources, to)?;
    let (rows, entries) = (swept.rows, swept.entries);
    let mut commits = Vec::new();
    let branch = format!("{to}-sweep");
    let written = args.write_blacklist && !entries.is_empty();
    if written {
        tree.branch_from(&remote, to, &branch).map_err(failed)?;
        for (addon, list) in &entries {
            commits.push(tree.blacklist(addon, list).map_err(failed)?);
        }
    }
    let opened = if written && args.open_pr {
        let upstream = forge_repo(ctx, &remote)
            .ok_or_else(|| failed(format!("the remote {remote} is not on a forge")))?;
        let series_list = sources.join(",");
        Some(propose(
            ctx,
            &Proposed {
                branch: &branch,
                base: to,
                remote: &remote,
                upstream: &upstream,
                push_remote: args.push_remote.as_deref(),
                draft: true,
                title: format!(
                    "[{to}][FW] blacklist port suggestions from {}",
                    sources.join("/")
                ),
                body: body(&entries, &rows, to, &series_list),
            },
        )?)
    } else {
        None
    };
    let out = if global.json {
        let value = serde_json::json!({
            "addons": rows,
            "branch": written.then_some(&branch),
            "commits": commits,
            "pull_request": opened.as_ref().map(opened_json),
        });
        format!("{value}\n")
    } else {
        let mut out = text(&rows, &sources, to);
        if written {
            let _ = writeln!(out, "blacklisted on branch {branch}:");
            for commit in &commits {
                let _ = writeln!(out, "  {commit}");
            }
        }
        if let Some(opened) = &opened {
            out.push_str(&opened_text(opened));
        }
        out
    };
    Ok(Report {
        out,
        ..Report::default()
    })
}

fn text(rows: &[Row], sources: &[String], to: &str) -> String {
    let mut out = String::new();
    let mut addon = "";
    for row in rows {
        if row.addon != addon {
            addon = &row.addon;
            let _ = writeln!(out, "{addon}");
        }
        let _ = writeln!(
            out,
            "  {}  {}: {} {}",
            row.series, row.class, row.key, row.title
        );
    }
    let portable = rows
        .iter()
        .filter(|row| row.class == Class::Portable.name())
        .count();
    let false_positives = rows
        .iter()
        .filter(|row| row.class != Class::Portable.name() && row.class != Class::Blacklisted.name())
        .count();
    let _ = writeln!(
        out,
        "{portable} portable, {false_positives} false positive(s) from {} against {to}",
        sources.join(", ")
    );
    if portable > 0 {
        let _ = writeln!(
            out,
            "port them with `odx port addon <addon> --from <series> --to {to}`"
        );
    }
    out
}

/// The pull request's description: the blacklist entries, their kinds and
/// the marker a later sweep finds its pull request by.
fn body(
    entries: &BTreeMap<String, Vec<(String, String)>>,
    rows: &[Row],
    to: &str,
    series_list: &str,
) -> String {
    let mut body = format!(
        "Blacklist entries for the false positives a sweep of {series_list} against {to} found, made with odx.\n\n## Blacklist\n\n| Addon | Entry | Kind | Reason |\n| --- | --- | --- | --- |\n"
    );
    for (addon, list) in entries {
        for (key, why) in list {
            let kind = rows
                .iter()
                .find(|row| row.addon == *addon && row.key == *key)
                .map_or("", |row| row.class);
            let _ = writeln!(body, "| {addon} | {key} | {kind} | {why} |");
        }
    }
    body.push_str(
        "\n### Blacklist kinds\n\n| Kind | Meaning |\n| --- | --- |\n\
         | repo-wide change | a commit that touched many addons or only repository files; not a change of this addon |\n\
         | migration commit | `[MIG]` or version-bump commits; cannot be ported |\n\
         | translations | `.po`/`.pot` only; synced by Weblate |\n",
    );
    let _ = write!(
        body,
        "\n<!-- odx:port sweep --to {to} --from {series_list} -->\n"
    );
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_per_class() {
        assert_eq!(reason(Class::Portable), None);
        assert_eq!(reason(Class::Blacklisted), None);
        assert!(reason(Class::Migration).unwrap().starts_with("migration"));
        assert!(reason(Class::RepoWide).unwrap().starts_with("repo-wide"));
        assert!(
            reason(Class::Translations)
                .unwrap()
                .starts_with("translations")
        );
    }
}
