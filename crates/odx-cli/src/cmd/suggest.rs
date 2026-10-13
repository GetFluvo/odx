//! `odx suggest`: changes on other series that the addons the project uses
//! do not have yet, per environment series.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{CachedHttp, Http, ReqwestHttp};
use odx_git::{FetchPlan, Forge, GitBackend, Source, SystemGit, fetch};
use odx_index::{AddonIndex, series_key};
use odx_port::analysis::{Class, Group, Query, Settings, analyse};
use odx_port::blacklist::{self, Blacklist};
use odx_port::git::Repo;

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::upgrade::{Kind, addon_index, addons, name_on, renames, steps};
use crate::Context;
use crate::cli::{GlobalArgs, SuggestArgs};

/// Suggestions for one addon from one source series.
struct Found {
    addon: String,
    target: String,
    source: String,
    url: String,
    groups: Vec<Group>,
}

/// `odx suggest`.
pub(crate) fn run_suggest(args: &SuggestArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    suggest(args, global, ctx).unwrap_or_else(|report| report)
}

/// The series to suggest for: `--series`, else the series of the
/// environment `--env` (or of every environment), else the project's.
fn targets(args: &SuggestArgs, prepared: &Prepared) -> Result<Vec<String>, Report> {
    let config = &prepared.project.config;
    if let Some(series) = &args.series {
        return Ok(vec![series.clone()]);
    }
    let environments = config.environments.as_ref();
    let default = environments
        .and_then(|envs| envs.series.clone())
        .unwrap_or_else(|| config.odoo_version.clone());
    let series_of = |env: &odx_config::schema::Environment| {
        env.series.clone().unwrap_or_else(|| default.clone())
    };
    let all = environments.map(|envs| &envs.env);
    let mut targets: BTreeSet<String> = match &args.env {
        Some(name) => {
            let env = all.and_then(|envs| envs.get(name)).ok_or_else(|| {
                Report::error(
                    Exit::Usage,
                    format!("no environment named {name:?} in [tool.odx.environments.env]"),
                )
            })?;
            BTreeSet::from([series_of(env)])
        }
        None => all
            .iter()
            .flat_map(|envs| envs.values())
            .map(series_of)
            .collect(),
    };
    if targets.is_empty() {
        targets.insert(config.odoo_version.clone());
    }
    Ok(targets.into_iter().collect())
}

/// The series to look on for `target`: `--from`, else `[tool.odx.port]
/// default_from`, else the series just below and just above.
fn sources(args: &SuggestArgs, prepared: &Prepared, target: &str) -> Vec<String> {
    if !args.from.is_empty() {
        return args.from.clone();
    }
    if let Some(from) = prepared
        .project
        .config
        .port
        .as_ref()
        .and_then(|port| port.default_from.clone())
    {
        return from;
    }
    let major = series_key(target).0;
    vec![format!("{}.0", major - 1), format!("{}.0", major + 1)]
}

/// Every series involved: the targets and their sources.
fn involved(args: &SuggestArgs, prepared: &Prepared, targets: &[String]) -> BTreeSet<String> {
    targets
        .iter()
        .flat_map(|target| std::iter::once(target.clone()).chain(sources(args, prepared, target)))
        .collect()
}

/// `<owner>/<repo>` of a repository URL: its last two path segments,
/// without `.git`, as blacklist keys name it.
///
/// ```text
/// https://github.com/OCA/pos.git -> OCA/pos
/// git@github.com:OCA/pos         -> OCA/pos
/// ```
fn repository_name(url: &str) -> String {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplit(['/', ':']);
    let repo = parts.next().unwrap_or_default();
    let owner = parts.next().unwrap_or_default();
    format!("{owner}/{repo}")
}

/// The addon's name on `series`, `target` being where it is `addon`.
fn name_there(index: &AddonIndex, addon: &str, target: &str, series: &str) -> String {
    if series_key(series) > series_key(target) {
        name_on(&index.renames, addon, &steps(target, series))
    } else {
        index
            .aliases(addon, target)
            .into_iter()
            .find(|alias| index.locate(alias, series).is_some())
            .unwrap_or_else(|| addon.to_owned())
    }
}

/// What every (addon, target, source) analysis shares.
struct Scan<'a> {
    prepared: &'a Prepared,
    offline: bool,
    settings: &'a Settings,
    index: &'a AddonIndex,
}

impl Scan<'_> {
    /// The suggestions for `addon` (aggregated from `aggregated`, if so) on
    /// `target` (in `target_url`) from `source`; `None` when the addon is
    /// not known on `source`.
    fn one(
        &self,
        addon: &str,
        aggregated: Option<&str>,
        target: &str,
        target_url: &str,
        source: &str,
    ) -> Result<Option<Found>, String> {
        let prepared = self.prepared;
        let source_name = name_there(self.index, addon, target, source);
        // An aggregated repository is looked in on every series.
        let fallback = aggregated.map(|url| odx_index::Location {
            url: url.to_owned(),
            path: source_name.clone(),
            origin: odx_index::Origin::Cache,
        });
        let Some(location) = self
            .index
            .locate(&source_name, source)
            .cloned()
            .or(fallback)
        else {
            return Ok(None);
        };
        let forge = Forge::detect(&location.url);
        let other = (location.url != target_url).then(|| target_url.to_owned());
        if !self.offline {
            let plan = FetchPlan {
                url: location.url.clone(),
                forge,
                sources: vec![
                    (None, Source::Branch(source.to_owned())),
                    (other.clone(), Source::Branch(target.to_owned())),
                ],
                mirror: None,
                fallback: None,
                clone: None,
                max_age: prepared.max_age,
            };
            let backend = SystemGit::new(Arc::new(OsRunner));
            fetch(
                &prepared.cache,
                &backend as &dyn GitBackend,
                &plan,
                Duration::from_secs(10),
            )
            .map_err(|e| e.to_string())?;
        }
        let runner = OsRunner;
        let repo = Repo::new(&runner, prepared.cache.repo_dir(&location.url));
        let source_ref = Source::Branch(source.to_owned()).local_ref(forge, None);
        let target_ref = Source::Branch(target.to_owned()).local_ref(forge, other.as_deref());
        let blacklist = repo
            .show(&target_ref, &blacklist::path(addon))
            .and_then(|text| Blacklist::parse(&text).ok())
            .unwrap_or_default();
        let repository = repository_name(&location.url);
        let query = Query {
            source: &source_ref,
            targets: std::slice::from_ref(&target_ref),
            source_dir: &source_name,
            target_dir: addon,
            repository: &repository,
            blacklist: &blacklist,
        };
        let groups = analyse(&repo, &query, self.settings)?;
        Ok(Some(Found {
            addon: addon.to_owned(),
            target: target.to_owned(),
            source: source.to_owned(),
            url: location.url,
            groups,
        }))
    }
}

fn suggest(args: &SuggestArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let config = &prepared.project.config;
    let port = config.port.as_ref();
    let settings = Settings::new(
        port.and_then(|port| port.migration_commit_patterns.as_deref()),
        port.and_then(|port| port.repo_wide_threshold),
    )
    .map_err(|e| Report::error(Exit::Usage, e))?;
    let mut report = Report::default();
    let addons: Vec<_> = addons(ctx, &prepared)
        .into_iter()
        .filter(|addon| addon.kind != Kind::Own)
        .filter(|addon| {
            args.addon
                .as_deref()
                .is_none_or(|pattern| git_aggregate_rs::fnmatch(&addon.name, pattern))
        })
        .collect();
    let http =
        ReqwestHttp::new().map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let online: Option<&dyn Http> = http
        .as_ref()
        .ok()
        .filter(|_| !global.offline)
        .map(|http| http as &dyn Http);
    let targets = targets(args, &prepared)?;
    // Every series involved, for renames and the index.
    let all = involved(args, &prepared, &targets);
    let lowest = all
        .iter()
        .min_by_key(|series| series_key(series))
        .cloned()
        .unwrap_or_default();
    let highest = all
        .iter()
        .max_by_key(|series| series_key(series))
        .cloned()
        .unwrap_or_default();
    let known = renames(
        ctx,
        &prepared,
        online,
        &steps(&lowest, &highest),
        &mut report.warnings,
    );
    let names: Vec<String> = addons.iter().map(|addon| addon.name.clone()).collect();
    let wanted: Vec<(String, Vec<String>)> = all
        .iter()
        .map(|series| (series.clone(), names.clone()))
        .collect();
    let index = addon_index(
        ctx,
        &prepared,
        online,
        known,
        &wanted,
        global.refresh_index,
        &mut report.warnings,
    );
    let scan = Scan {
        prepared: &prepared,
        offline: global.offline,
        settings: &settings,
        index: &index,
    };
    let mut found: Vec<Found> = Vec::new();
    for target in &targets {
        for addon in &addons {
            let Some(target_url) = addon
                .url
                .clone()
                .filter(|_| *target == config.odoo_version)
                .or_else(|| index.locate(&addon.name, target).map(|l| l.url.clone()))
            else {
                continue;
            };
            for source in sources(args, &prepared, target) {
                let answer = scan.one(
                    &addon.name,
                    addon.url.as_deref(),
                    target,
                    &target_url,
                    &source,
                );
                match answer {
                    Ok(Some(item)) => found.push(item),
                    Ok(None) => {}
                    Err(message) => report.warnings.push(Problem {
                        key: format!("{} on {source}", addon.name),
                        message,
                    }),
                }
            }
        }
    }
    report.out = if global.json {
        format!("{}\n", json(&found))
    } else {
        text(&found, args.show_muted)
    };
    Ok(report)
}

fn text(found: &[Found], show_suppressed: bool) -> String {
    let mut out = String::new();
    let (mut shown, mut suppressed) = (0, 0);
    let mut heading = String::new();
    for item in found {
        let groups: Vec<&Group> = item
            .groups
            .iter()
            .filter(|group| {
                let hidden = group.class == Class::Blacklisted;
                if hidden {
                    suppressed += 1;
                }
                show_suppressed || !hidden
            })
            .collect();
        if groups.is_empty() {
            continue;
        }
        let title = format!("{} on {}", item.addon, item.target);
        if title != heading {
            let _ = writeln!(out, "{title}:");
            heading = title;
        }
        for group in groups {
            shown += 1;
            let commits = group.commits.len();
            let author = group
                .commits
                .last()
                .map(|c| c.author.as_str())
                .unwrap_or_default();
            let _ = write!(
                out,
                "  {} from {}: {} - {} ({commits} commits, {author})",
                group.key,
                item.source,
                group.class.name(),
                group.title
            );
            if let Some(reason) = &group.reason {
                let _ = write!(out, "; {reason}");
            }
            out.push('\n');
        }
    }
    let _ = writeln!(
        out,
        "{shown} suggestions ({suppressed} blacklisted upstream)"
    );
    out
}

fn json(found: &[Found]) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "suggestions": found.iter().flat_map(|item| item.groups.iter().map(move |group| serde_json::json!({
            "addon": item.addon,
            "target": item.target,
            "source": item.source,
            "repository": item.url,
            "key": group.key,
            "pr": group.pr,
            "title": group.title,
            "class": group.class.name(),
            "reason": group.reason,
            "commits": group.commits.iter().map(|c| serde_json::json!({
                "sha": c.sha, "subject": c.subject, "author": c.author, "date": c.date,
            })).collect::<Vec<_>>(),
        }))).collect::<Vec<_>>(),
    })
}
