//! `odx suggest`: changes on other series that the addons the project uses
//! do not have yet, per environment series.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
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
use odx_verdicts::{Entry, Standing};

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::upgrade::{Kind, addon_index, addons, name_on, renames, steps};
use crate::Context;
use crate::cli::{GlobalArgs, SuggestArgs};

/// Suggestions for one addon from one source series.
pub(crate) struct Found {
    pub(crate) addon: String,
    pub(crate) target: String,
    pub(crate) source: String,
    /// The repository the addon lives in on the source series.
    pub(crate) url: String,
    /// The repository of the target series.
    pub(crate) target_url: String,
    /// The cache repository the analysis ran in, and its target ref.
    pub(crate) repo_dir: PathBuf,
    pub(crate) target_ref: String,
    pub(crate) groups: Vec<Group>,
}

impl Found {
    /// Whether `group`'s commits cherry-pick cleanly onto the target.
    pub(crate) fn applies(&self, group: &Group) -> Option<bool> {
        let runner = OsRunner;
        let commits: Vec<String> = group.commits.iter().rev().map(|c| c.sha.clone()).collect();
        Repo::new(&runner, &self.repo_dir).applies(&self.target_ref, &commits)
    }

    /// The id of `files` on the target series now, for re-checks.
    pub(crate) fn tree(&self, files: &[String]) -> Option<String> {
        let runner = OsRunner;
        Repo::new(&runner, &self.repo_dir)
            .tree_of(&self.target_ref, files)
            .ok()
    }
}

/// `odx suggest`.
pub(crate) fn run_suggest(args: &SuggestArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    suggest(args, global, ctx).unwrap_or_else(|report| report)
}

/// The series to suggest for: `--series`, else the series of the
/// environment `--env` (or of every environment), else the project's.
pub(crate) fn targets(args: &SuggestArgs, prepared: &Prepared) -> Result<Vec<String>, Report> {
    let config = &prepared.project.config;
    if let Some(series) = &args.series {
        return Ok(vec![series.clone()]);
    }
    // The configuration was validated when it was loaded.
    let model = odx_config::environments::Model::resolve(config)
        .ok()
        .flatten();
    let all = model.as_ref().map(|model| &model.environments);
    let mut targets: BTreeSet<String> = match &args.env {
        Some(name) => {
            let env = all.and_then(|envs| envs.get(name)).ok_or_else(|| {
                Report::error(
                    Exit::Usage,
                    format!("no environment named {name:?} (`odx env list` shows them)"),
                )
            })?;
            BTreeSet::from([env.series.clone()])
        }
        None => all
            .iter()
            .flat_map(|envs| envs.values())
            .map(|env| env.series.clone())
            .collect(),
    };
    if targets.is_empty() {
        targets.insert(config.odoo_version.clone());
    }
    Ok(targets.into_iter().collect())
}

/// The series to look on for `target`: `--from`, else `[tool.odx.suggest]
/// series`, else `[tool.odx.port] default_from`, else the series just below
/// and just above.
fn sources(args: &SuggestArgs, prepared: &Prepared, target: &str) -> Vec<String> {
    if !args.from.is_empty() {
        return args.from.clone();
    }
    let config = &prepared.project.config;
    if let Some(from) = config
        .suggest
        .as_ref()
        .and_then(|suggest| suggest.series.clone())
        .or_else(|| {
            config
                .port
                .as_ref()
                .and_then(|port| port.default_from.clone())
        })
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
pub(crate) fn repository_name(url: &str) -> String {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.rsplit(['/', ':', '\\']);
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
                offline: false,
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
            url: location.url.clone(),
            target_url: target_url.to_owned(),
            repo_dir: prepared.cache.repo_dir(&location.url),
            target_ref,
            groups,
        }))
    }
}

fn suggest(args: &SuggestArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let mut report = Report::default();
    let found = collect(args, global, ctx, &prepared, &mut report.warnings)?;
    let entries = super::verdict::entries(ctx, &prepared, global, &mut report.warnings);
    let judged = judge(&found, &entries, args.env.as_deref());
    let applies: Vec<Option<bool>> = judged
        .iter()
        .map(|item| {
            let wanted = args.check_apply && item.group.class == Class::Portable;
            wanted.then(|| item.found.applies(item.group)).flatten()
        })
        .collect();
    report.out = if global.json {
        format!("{}\n", json(&judged, &applies))
    } else {
        text(&judged, &applies, args.show_muted)
    };
    Ok(report)
}

/// The suggestions for `args`, before verdicts and mutes.
pub(crate) fn collect(
    args: &SuggestArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
    warnings: &mut Vec<Problem>,
) -> Result<Vec<Found>, Report> {
    let config = &prepared.project.config;
    let port = config.port.as_ref();
    let settings = Settings::new(
        port.and_then(|port| port.migration_commit_patterns.as_deref()),
        port.and_then(|port| port.repo_wide_threshold),
    )
    .map_err(|e| Report::error(Exit::Usage, e))?;
    let addons: Vec<_> = addons(ctx, prepared)
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
    let targets = targets(args, prepared)?;
    // Every series involved, for renames and the index.
    let all = involved(args, prepared, &targets);
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
    let known = renames(ctx, prepared, online, &steps(&lowest, &highest), warnings);
    let names: Vec<String> = addons.iter().map(|addon| addon.name.clone()).collect();
    let wanted: Vec<(String, Vec<String>)> = all
        .iter()
        .map(|series| (series.clone(), names.clone()))
        .collect();
    let index = addon_index(
        ctx,
        prepared,
        online,
        known,
        &wanted,
        global.refresh_index,
        warnings,
    );
    let scan = Scan {
        prepared,
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
                .or_else(|| addon.url.clone())
            else {
                continue;
            };
            for source in sources(args, prepared, target) {
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
                    Err(message) => warnings.push(Problem {
                        key: format!("{} on {source}", addon.name),
                        message,
                    }),
                }
            }
        }
    }
    Ok(found)
}

/// A suggestion with the verdict or mute that concerns it, if any.
pub(crate) struct Judged<'a> {
    pub(crate) found: &'a Found,
    pub(crate) group: &'a Group,
    pub(crate) verdict: Option<(&'a Entry, Standing)>,
}

impl Judged<'_> {
    /// Hidden unless suppressed ones are asked for: blacklisted upstream,
    /// or covered by a verdict or mute in force.
    pub(crate) fn hidden(&self) -> bool {
        self.group.class == Class::Blacklisted
            || self
                .verdict
                .is_some_and(|(_, standing)| standing == Standing::Applies)
    }
}

/// Match every suggestion with the first verdict or mute in force for it.
pub(crate) fn judge<'a>(
    found: &'a [Found],
    entries: &'a [(PathBuf, Entry)],
    env: Option<&str>,
) -> Vec<Judged<'a>> {
    let today = odx_aggregate::date(std::time::SystemTime::now());
    found
        .iter()
        .flat_map(|item| item.groups.iter().map(move |group| (item, group)))
        .map(|(item, group)| {
            let verdict = entries
                .iter()
                .map(|(_, entry)| entry)
                .find(|entry| {
                    entry.concerns(&group.key, &group.patch_ids, &item.addon, &item.target)
                        && entry.in_force(&today, env)
                })
                .map(|entry| {
                    let tree = entry
                        .recheck
                        .as_ref()
                        .and_then(|recheck| item.tree(&recheck.files));
                    (entry, entry.standing(tree.as_deref()))
                });
            Judged {
                found: item,
                group,
                verdict,
            }
        })
        .collect()
}

fn text(judged: &[Judged<'_>], applies: &[Option<bool>], show_suppressed: bool) -> String {
    let mut out = String::new();
    let (mut shown, mut blacklisted, mut suppressed) = (0, 0, 0);
    let mut heading = String::new();
    for (item, applies) in judged.iter().zip(applies) {
        if item.group.class == Class::Blacklisted {
            blacklisted += 1;
        } else if item.hidden() {
            suppressed += 1;
        }
        if item.hidden() && !show_suppressed {
            continue;
        }
        let title = format!("{} on {}", item.found.addon, item.found.target);
        if title != heading {
            let _ = writeln!(out, "{title}:");
            heading = title;
        }
        shown += 1;
        let group = item.group;
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
            item.found.source,
            group.class.name(),
            group.title
        );
        if let Some(reason) = &group.reason {
            let _ = write!(out, "; {reason}");
        }
        match applies {
            Some(true) => out.push_str("; applies cleanly"),
            Some(false) => out.push_str("; conflicts with the target"),
            None => {}
        }
        match item.verdict {
            Some((entry, Standing::Applies)) => {
                let _ = write!(
                    out,
                    "; suppressed by {} ({}): {}",
                    entry.id,
                    entry.kind.name(),
                    entry.reason
                );
            }
            Some((entry, Standing::Stale)) => {
                let _ = write!(
                    out,
                    "; previously declined ({}): {} (target code changed since {})",
                    entry.kind.name(),
                    entry.reason,
                    entry.date
                );
            }
            None => {}
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "{shown} suggestions ({blacklisted} blacklisted upstream, {suppressed} suppressed by verdicts and mutes)"
    );
    out
}

fn json(judged: &[Judged<'_>], applies: &[Option<bool>]) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "suggestions": judged.iter().zip(applies).map(|(item, applies)| {
            let group = item.group;
            serde_json::json!({
                "addon": item.found.addon,
                "target": item.found.target,
                "source": item.found.source,
                "repository": item.found.url,
                "key": group.key,
                "pr": group.pr,
                "title": group.title,
                "class": group.class.name(),
                "reason": group.reason,
                "patch_ids": group.patch_ids,
                "applies": applies,
                "verdict": item.verdict.map(|(entry, standing)| serde_json::json!({
                    "id": entry.id,
                    "kind": entry.kind.name(),
                    "reason": entry.reason,
                    "stale": standing == Standing::Stale,
                })),
                "commits": group.commits.iter().map(|c| serde_json::json!({
                    "sha": c.sha, "subject": c.subject, "author": c.author, "date": c.date,
                })).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_from_urls_and_paths() {
        assert_eq!(repository_name("https://github.com/OCA/pos.git"), "OCA/pos");
        assert_eq!(repository_name("git@github.com:OCA/pos"), "OCA/pos");
        assert_eq!(repository_name("/srv/git/OCA/pos/"), "OCA/pos");
        assert_eq!(repository_name(r"C:\Temp\work\up"), "work/up");
    }
}
