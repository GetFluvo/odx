//! `odx upgrade --target <series>`: for every addon of the project, whether
//! it exists on the target series, where, and how much work the project's
//! own addons need.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use odx_aggregate::plan_config;
use odx_analyze::stats::addon_dirs;
use odx_config::Problem;
use odx_config::deps::normalize_name;
use odx_config::schema::{Index, IndexSource};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner, repo_key};
use odx_forge::{CachedHttp, ForgeClient, ForgeKind, Http, RepoUrl, ReqwestHttp};
use odx_git::{Forge, Merger};
use odx_index::sources::openupgrade;
use odx_index::{AddonIndex, Build, Listing, Renames, SeriesIndex, build, series_key};

use super::Report;
use super::aggregate::{Prepared, prepare};
use crate::Context;
use crate::cli::{GlobalArgs, UpgradeArgs};

/// Where an addon of the project comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Own,
    Aggregated,
    Package,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Own => "own",
            Self::Aggregated => "aggregated",
            Self::Package => "package",
        }
    }
}

/// One addon of the project.
pub(crate) struct Addon {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    /// Its directory, for own and aggregated addons.
    pub(crate) dir: Option<PathBuf>,
    /// Its repository on the current series, when known without the index.
    pub(crate) url: Option<String>,
}

/// The answer for one addon.
#[derive(Debug, Default)]
struct Row {
    name: String,
    kind: &'static str,
    status: &'static str,
    url: Option<String>,
    moved_from: Option<String>,
    renamed_to: Option<String>,
    pull_request: Option<u64>,
    effort: Option<Effort>,
}

/// odoo-lint's counts for one module.
#[derive(Debug, Default, Clone, Copy, serde::Deserialize, serde::Serialize)]
struct Effort {
    changes: usize,
    automatic: usize,
    review: usize,
    manual: usize,
}

/// `odx upgrade`.
pub(crate) fn run_upgrade(args: &UpgradeArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    upgrade(args, global, ctx).unwrap_or_else(|report| report)
}

/// The project's own addons, the selected addons of its aggregated
/// repositories, and the addon packages in `uv.lock`.
pub(crate) fn addons(ctx: &Context<'_>, prepared: &Prepared) -> Vec<Addon> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let name_of = |dir: &std::path::Path| {
        dir.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mut found: Vec<Addon> = config
        .project_addon_dirs()
        .iter()
        .flat_map(|dir| addon_dirs(&root.join(dir)))
        .map(|dir| Addon {
            name: name_of(&dir),
            kind: Kind::Own,
            dir: Some(dir),
            url: None,
        })
        .collect();
    let planned = plan_config(config, root, None).unwrap_or_default();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let patterns = configured.addons.clone().unwrap_or_default();
        for dir in addon_dirs(&repo.target_dir) {
            let name = name_of(&dir);
            let selected = patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| git_aggregate_rs::fnmatch(&name, pattern));
            if selected {
                found.push(Addon {
                    name,
                    kind: Kind::Aggregated,
                    dir: Some(dir),
                    url: Some(repo.url.clone()),
                });
            }
        }
    }
    let uv_lock =
        ctx.fs.read(&root.join("uv.lock")).ok().and_then(|bytes| {
            odx_config::uvlock::UvLock::parse(&String::from_utf8_lossy(&bytes)).ok()
        });
    for package in uv_lock.iter().flat_map(|lock| &lock.packages) {
        let dist = normalize_name(&package.name);
        let addon = dist
            .strip_prefix("odoo-addon-")
            .or_else(|| dist.split_once("-addon-").map(|(_, addon)| addon))
            .map(|addon| addon.replace('-', "_"));
        if let Some(addon) = addon.filter(|addon| !found.iter().any(|a| a.name == *addon)) {
            found.push(Addon {
                name: addon,
                kind: Kind::Package,
                dir: None,
                url: None,
            });
        }
    }
    found
}

/// The series from `current` (exclusive) to `target` (inclusive), by
/// major version.
pub(crate) fn steps(current: &str, target: &str) -> Vec<String> {
    let (from, to) = (series_key(current).0, series_key(target).0);
    (from + 1..=to).map(|major| format!("{major}.0")).collect()
}

/// `addon`'s name on `target`: renames and merges applied in order.
pub(crate) fn name_on(renames: &Renames, addon: &str, series: &[String]) -> String {
    series.iter().fold(addon.to_owned(), |name, step| {
        renames
            .renamed
            .get(step)
            .and_then(|table| table.get(&name))
            .or_else(|| renames.merged.get(step).and_then(|table| table.get(&name)))
            .cloned()
            .unwrap_or(name)
    })
}

/// odoo-lint's upgrade counts per module for `dirs` (`ODX_ODL` names the
/// `odl` executable).
fn lint(
    ctx: &Context<'_>,
    dirs: &[PathBuf],
    target: &str,
) -> Result<BTreeMap<String, Effort>, String> {
    #[derive(serde::Deserialize)]
    struct Module {
        name: String,
        #[serde(flatten)]
        effort: Effort,
    }
    #[derive(serde::Deserialize)]
    struct Answer {
        modules: Vec<Module>,
    }
    let odl = ctx
        .env
        .var("ODX_ODL")
        .as_deref()
        .unwrap_or("odl")
        .to_owned();
    let invocation = Invocation::new(&odl)
        .args([
            "upgrade-check",
            "--target",
            target,
            "--output-format",
            "json",
        ])
        .args(dirs.iter().map(|dir| dir.as_os_str().to_owned()));
    let output = OsRunner
        .run(&invocation)
        .map_err(|e| format!("{odl}: {e}; install odoo-lint for readiness counts"))?;
    let answer: Answer = serde_json::from_slice(&output.stdout).map_err(|e| {
        format!(
            "{odl} upgrade-check: {e}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    Ok(answer
        .modules
        .into_iter()
        .map(|module| (module.name, module.effort))
        .collect())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The index of `series`: the cached one while fresh, then rebuilt for
/// `wanted` and cached again.
fn series_index(
    prepared: &Prepared,
    spec: &Build<'_>,
    series: &str,
    wanted: &[String],
    refresh: bool,
    warnings: &mut Vec<Problem>,
) -> SeriesIndex {
    let config = &prepared.project.config;
    let ttl = config
        .index
        .as_ref()
        .and_then(|index| index.ttl)
        .map_or(Duration::from_hours(7 * 24), odx_config::Span::as_duration);
    let cache = prepared.cache.root();
    let previous = SeriesIndex::load(cache, series)
        .filter(|index| !refresh && now().saturating_sub(index.fetched_at) <= ttl.as_secs());
    let (index, problems) = build(spec, series, wanted, previous, now());
    warnings.extend(problems.into_iter().map(|message| Problem {
        key: "index".to_owned(),
        message,
    }));
    // A cache that cannot be written only costs lookups next time.
    let _ = index.save(cache);
    index
}

/// Configured renames, then OpenUpgrade's for every step.
pub(crate) fn renames(
    ctx: &Context<'_>,
    prepared: &Prepared,
    online: Option<&dyn Http>,
    series: &[String],
    warnings: &mut Vec<Problem>,
) -> Renames {
    let config = &prepared.project.config;
    let mut renames = Renames {
        renamed: config
            .index
            .as_ref()
            .map(|index| index.renames.clone())
            .unwrap_or_default(),
        ..Renames::default()
    };
    let Some(http) = online else {
        return renames;
    };
    let base = ctx
        .env
        .var("ODX_OPENUPGRADE_URL")
        .as_deref()
        .unwrap_or("https://raw.githubusercontent.com/OCA/OpenUpgrade")
        .to_owned();
    for step in series {
        match openupgrade(http, &base, step) {
            Ok((renamed_here, merged_here)) => {
                renames
                    .renamed
                    .entry(step.clone())
                    .or_default()
                    .extend(renamed_here);
                renames.merged.insert(step.clone(), merged_here);
            }
            Err(message) => warnings.push(Problem {
                key: "openupgrade".to_owned(),
                message,
            }),
        }
    }
    renames
}

/// The organizations of `[tool.odx.index] orgs` whose listings the index
/// reads, as (a URL on their host, owner): names are on GitHub, URLs name
/// their host; none when `sources` leaves out `forge`.
fn organizations(index: Option<&Index>) -> Vec<(RepoUrl, String)> {
    let index = index.filter(|index| {
        index
            .sources
            .as_ref()
            .is_none_or(|sources| sources.contains(&IndexSource::Forge))
    });
    let mut orgs: Vec<(RepoUrl, String)> = Vec::new();
    for spelling in index
        .and_then(|index| index.orgs.as_ref())
        .into_iter()
        .flatten()
    {
        let spelling = spelling.trim_end_matches('/');
        let url = if spelling.contains("://") {
            format!("{spelling}/-")
        } else {
            format!("https://github.com/{spelling}/-")
        };
        let Some(site) = RepoUrl::parse(&url) else {
            continue;
        };
        let owner = site.path.trim_end_matches("/-").to_owned();
        if !orgs
            .iter()
            .any(|(known, name)| known.host == site.host && *name == owner)
        {
            orgs.push((site, owner));
        }
    }
    orgs
}

/// The addons of `orgs` on `series`, with the listings that failed.
fn list(
    ctx: &Context<'_>,
    http: &dyn Http,
    orgs: &[(RepoUrl, String)],
    series: &str,
) -> (Vec<(String, String)>, Vec<String>) {
    let sleep = std::thread::sleep;
    let client = ForgeClient::new(http, ctx.env, &OsRunner, &sleep);
    let mut found = Vec::new();
    let mut failures = Vec::new();
    for (site, owner) in orgs {
        match client
            .kind(site)
            .and_then(|kind| client.organization_addons(site, kind, owner, series))
        {
            Ok(listed) => found.extend(listed),
            Err(e) => failures.push(format!("listing {}/{owner} on {series}: {e}", site.host)),
        }
    }
    (found, failures)
}

/// The addon index of the series in `wanted`, each with the addons to
/// look up there.
pub(crate) fn addon_index(
    ctx: &Context<'_>,
    prepared: &Prepared,
    online: Option<&dyn Http>,
    renames: Renames,
    wanted: &[(String, Vec<String>)],
    refresh: bool,
    warnings: &mut Vec<Problem>,
) -> AddonIndex {
    let config = &prepared.project.config;
    let planned = plan_config(config, &prepared.project.root, None).unwrap_or_default();
    let cached: Vec<(String, PathBuf, Forge)> = planned
        .iter()
        .map(|repo| {
            (
                repo.url.clone(),
                prepared.cache.repo_dir(&repo.url),
                repo.forge,
            )
        })
        .collect();
    let pypi = ctx
        .env
        .var("ODX_PYPI_URL")
        .as_deref()
        .unwrap_or("https://pypi.org/pypi")
        .to_owned();
    let merger = Merger::new(Arc::new(OsRunner));
    let overrides = config
        .index
        .as_ref()
        .map(|index| index.overrides.clone())
        .unwrap_or_default();
    let orgs = organizations(config.index.as_ref());
    let listing = online
        .filter(|_| !orgs.is_empty())
        .map(|http| |series: &str| list(ctx, http, &orgs, series));
    let spec = Build {
        overrides: &overrides,
        cached: &cached,
        merger: &merger,
        package_index: online.map(|http| (http, pypi.as_str())),
        listing: listing.as_ref().map(|listing| listing as Listing<'_>),
    };
    let mut index = AddonIndex {
        renames,
        ..AddonIndex::default()
    };
    for (series, addons) in wanted {
        let built = series_index(prepared, &spec, series, addons, refresh, warnings);
        index.series.insert(series.clone(), built);
    }
    index
}

/// What one `odx upgrade` run works with.
struct Run<'a> {
    ctx: &'a Context<'a>,
    prepared: &'a Prepared,
    current: String,
    target: String,
    /// The series after `current` up to `target`.
    series: Vec<String>,
    /// The network, unless offline.
    online: Option<&'a dyn Http>,
}

impl Run<'_> {
    /// Configured renames, then OpenUpgrade's for every step.
    fn renames(&self, warnings: &mut Vec<Problem>) -> Renames {
        renames(self.ctx, self.prepared, self.online, &self.series, warnings)
    }

    /// The index of the current and the target series for `addons`.
    fn index(
        &self,
        renames: Renames,
        addons: &[Addon],
        refresh: bool,
        warnings: &mut Vec<Problem>,
    ) -> AddonIndex {
        let foreign = addons.iter().filter(|addon| addon.kind != Kind::Own);
        let wanted_now: Vec<String> = foreign.clone().map(|addon| addon.name.clone()).collect();
        let wanted_then: Vec<String> = foreign
            .map(|addon| name_on(&renames, &addon.name, &self.series))
            .collect();
        let wanted = [
            (self.current.clone(), wanted_now),
            (self.target.clone(), wanted_then),
        ];
        addon_index(
            self.ctx,
            self.prepared,
            self.online,
            renames,
            &wanted,
            refresh,
            warnings,
        )
    }

    /// Where an addon of another source stands on the target series.
    fn row(
        &self,
        addon: &Addon,
        index: &AddonIndex,
        kinds: &mut BTreeMap<String, Option<ForgeKind>>,
    ) -> Row {
        let mut row = Row {
            name: addon.name.clone(),
            kind: addon.kind.name(),
            ..Row::default()
        };
        let new_name = name_on(&index.renames, &addon.name, &self.series);
        let here = addon.url.clone().or_else(|| {
            index
                .locate(&addon.name, &self.current)
                .map(|location| location.url.clone())
        });
        if new_name != addon.name {
            row.renamed_to = Some(new_name.clone());
        }
        if let Some(location) = index.locate(&new_name, &self.target) {
            row.status = if location.origin == odx_index::Origin::PackageIndex {
                "released"
            } else {
                "available"
            };
            row.url = Some(location.url.clone());
            row.moved_from = here.filter(|url| repo_key(url) != repo_key(&location.url));
            return row;
        }
        // An open migration pull request in the repository it lives in.
        let sleep = std::thread::sleep;
        let pull_request = here
            .as_deref()
            .and_then(RepoUrl::parse)
            .zip(self.online)
            .and_then(|(repo, http)| {
                let client = ForgeClient::new(http, self.ctx.env, &OsRunner, &sleep);
                let kind = kinds
                    .entry(repo.origin())
                    .or_insert_with(|| client.kind(&repo).ok())
                    .as_ref()
                    .copied()?;
                let open = client
                    .open_pull_requests(&repo, kind, &self.target)
                    .unwrap_or_default();
                open.into_iter()
                    .find(|pr| pr.title.contains("[MIG]") && pr.title.contains(&new_name))
                    .map(|pr| pr.number)
            });
        row.status = if pull_request.is_some() {
            "migration PR"
        } else {
            "missing"
        };
        row.pull_request = pull_request;
        row.url = here;
        row
    }
}

/// Where every addon of the project stands on a target series.
struct Assessment {
    current: String,
    target: String,
    addons: Vec<Addon>,
    rows: Vec<Row>,
}

fn upgrade(args: &UpgradeArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let mut report = Report::default();
    let Assessment {
        current,
        target,
        mut rows,
        ..
    } = assess(
        args.target.as_deref(),
        args.no_lint,
        global,
        ctx,
        &mut report.warnings,
    )?;
    if args.only_missing {
        rows.retain(|row| matches!(row.status, "missing" | "migration PR"));
    }
    report.out = if global.json {
        format!("{}\n", json(&current, &target, &rows))
    } else {
        text(&current, &target, &rows)
    };
    Ok(report)
}

/// `odx migrate plan`: what a move to `target` takes, addon by addon, the
/// ones to migrate dependencies first.
/// The target series: `target`, else `[tool.odx.upgrade]
/// target_odoo_version`.
pub(crate) fn target_series(target: Option<&str>, prepared: &Prepared) -> Result<String, Report> {
    target
        .map(str::to_owned)
        .or_else(|| {
            prepared
                .project
                .config
                .upgrade
                .as_ref()
                .and_then(|u| u.target_odoo_version.clone())
        })
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                "no target series: pass --target or set [tool.odx.upgrade] target_odoo_version"
                    .to_owned(),
            )
        })
}

pub(crate) fn migrate_plan(
    target: Option<&str>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let mut report = Report::default();
    let assessment = assess(target, true, global, ctx, &mut report.warnings)?;
    let depends = |name: &str| -> Vec<String> {
        assessment
            .addons
            .iter()
            .find(|addon| addon.name == name)
            .and_then(|addon| addon.dir.as_ref())
            .and_then(|dir| ctx.fs.read(&dir.join("__manifest__.py")).ok())
            .and_then(|bytes| {
                odx_analyze::manifest::parse_manifest(&String::from_utf8_lossy(&bytes)).ok()
            })
            .map(|manifest| manifest.depends)
            .unwrap_or_default()
    };
    let rows = &assessment.rows;
    let missing: Vec<&Row> = rows.iter().filter(|row| row.status == "missing").collect();
    let names: Vec<String> = missing.iter().map(|row| row.name.clone()).collect();
    let order = dependencies_first(&names, &depends);
    let to = &assessment.target;
    let ready: Vec<&Row> = rows
        .iter()
        .filter(|row| matches!(row.status, "released" | "available"))
        .collect();
    let prs: Vec<&Row> = rows
        .iter()
        .filter(|row| row.status == "migration PR")
        .collect();
    let own: Vec<&Row> = rows.iter().filter(|row| row.status == "own").collect();
    let url_of = |name: &str| {
        missing
            .iter()
            .find(|row| row.name == name)
            .and_then(|row| row.url.clone())
    };
    report.out = if global.json {
        let value = serde_json::json!({
            "current": assessment.current,
            "target": to,
            "available": ready.iter().map(|row| &row.name).collect::<Vec<_>>(),
            "pull_requests": prs.iter().map(|row| serde_json::json!({
                "addon": row.name, "number": row.pull_request, "url": row.url,
            })).collect::<Vec<_>>(),
            "to_migrate": order.iter().map(|name| serde_json::json!({
                "addon": name, "url": url_of(name),
            })).collect::<Vec<_>>(),
            "own": own.iter().map(|row| &row.name).collect::<Vec<_>>(),
        });
        format!("{value}\n")
    } else {
        let mut out = format!("migration plan {} -> {to}\n", assessment.current);
        let names = |rows: &[&Row]| {
            rows.iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "available on {to} ({}): {}",
            ready.len(),
            names(&ready)
        );
        for row in &prs {
            let _ = writeln!(
                out,
                "open migration pull request: {} #{} in {}; review, test and contribute there",
                row.name,
                row.pull_request.unwrap_or_default(),
                row.url.as_deref().unwrap_or_default()
            );
        }
        let _ = writeln!(out, "to migrate, dependencies first ({}):", order.len());
        for name in &order {
            match url_of(name) {
                Some(url) => {
                    let _ = writeln!(out, "  {name} (in {url})");
                }
                None => {
                    let _ = writeln!(out, "  {name}");
                }
            }
        }
        let _ = writeln!(
            out,
            "own addons, migrated in place ({}): {}",
            own.len(),
            names(&own)
        );
        let _ = writeln!(
            out,
            "next: in a clone of each addon's repository, `odx migrate addon <addon> --to {to}`"
        );
        out
    };
    Ok(report)
}

/// `names` ordered so that each comes after those of them it depends on;
/// otherwise in the given order. A cycle is broken at its first member.
pub(crate) fn dependencies_first(
    names: &[String],
    depends: &dyn Fn(&str) -> Vec<String>,
) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut left: Vec<&String> = names.iter().collect();
    while !left.is_empty() {
        // What `name` still waits for among `names`.
        let waits = |name: &str| -> Vec<String> {
            depends(name)
                .into_iter()
                .filter(|dep| dep != name && names.contains(dep) && !order.contains(dep))
                .collect()
        };
        let ready = left.iter().position(|name| waits(name).is_empty());
        let next = ready.unwrap_or_else(|| {
            left.iter()
                .position(|name| reaches(name, name, &waits, &mut Vec::new()))
                .unwrap_or(0)
        });
        order.push(left.remove(next).clone());
    }
    order
}

/// Whether `to` is reachable from `from` through what they wait for.
fn reaches(
    from: &str,
    to: &str,
    waits: &dyn Fn(&str) -> Vec<String>,
    seen: &mut Vec<String>,
) -> bool {
    waits(from).into_iter().any(|next| {
        if next == to {
            return true;
        }
        if seen.contains(&next) {
            return false;
        }
        seen.push(next.clone());
        reaches(&next, to, waits, seen)
    })
}

/// The project's addons on `target` (default `[tool.odx.upgrade]
/// target_odoo_version`), with odoo-lint's counts for its own unless
/// `no_lint`.
fn assess(
    target: Option<&str>,
    no_lint: bool,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    warnings: &mut Vec<Problem>,
) -> Result<Assessment, Report> {
    let prepared = prepare(global, ctx)?;
    let config = &prepared.project.config;
    let target = target_series(target, &prepared)?;
    let addons = addons(ctx, &prepared);
    let http =
        ReqwestHttp::new().map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let current = config.odoo_version.clone();
    let run = Run {
        ctx,
        prepared: &prepared,
        series: steps(&current, &target),
        current,
        target,
        online: http
            .as_ref()
            .ok()
            .filter(|_| !global.offline)
            .map(|http| http as &dyn Http),
    };
    let renames = run.renames(warnings);
    let index = run.index(renames, &addons, global.refresh_index, warnings);
    // Own addons: odoo-lint's readiness.
    let own: Vec<PathBuf> = addons
        .iter()
        .filter(|addon| addon.kind == Kind::Own)
        .filter_map(|addon| addon.dir.clone())
        .collect();
    let efforts = if no_lint || own.is_empty() {
        BTreeMap::new()
    } else {
        lint(ctx, &own, &run.target).unwrap_or_else(|message| {
            warnings.push(Problem {
                key: "odoo-lint".to_owned(),
                message,
            });
            BTreeMap::new()
        })
    };
    let mut kinds = BTreeMap::new();
    let rows: Vec<Row> = addons
        .iter()
        .map(|addon| match addon.kind {
            Kind::Own => Row {
                name: addon.name.clone(),
                kind: addon.kind.name(),
                status: "own",
                effort: efforts.get(&addon.name).copied(),
                ..Row::default()
            },
            Kind::Aggregated | Kind::Package => run.row(addon, &index, &mut kinds),
        })
        .collect();
    let (current, target) = (run.current, run.target);
    Ok(Assessment {
        current,
        target,
        addons,
        rows,
    })
}

fn text(current: &str, target: &str, rows: &[Row]) -> String {
    let mut out = format!("upgrade {current} -> {target}\n");
    for row in rows {
        let _ = write!(out, "{}: {}", row.name, row.status);
        if let Some(new_name) = &row.renamed_to {
            let _ = write!(out, " as {new_name}");
        }
        if let Some(number) = row.pull_request {
            let _ = write!(out, " #{number}");
        }
        if let Some(url) = &row.url {
            let _ = write!(out, " in {url}");
        }
        if let Some(from) = &row.moved_from {
            let _ = write!(out, ", moved from {from}");
        }
        if let Some(effort) = row.effort {
            let _ = write!(
                out,
                ", {} changes ({} automatic, {} review, {} manual)",
                effort.changes, effort.automatic, effort.review, effort.manual
            );
        }
        out.push('\n');
    }
    let count = |status: &str| rows.iter().filter(|row| row.status == status).count();
    let _ = writeln!(
        out,
        "{} addons: {} released, {} available, {} with a migration PR, {} missing, {} own",
        rows.len(),
        count("released"),
        count("available"),
        count("migration PR"),
        count("missing"),
        count("own")
    );
    out
}

fn json(current: &str, target: &str, rows: &[Row]) -> serde_json::Value {
    serde_json::json!({
        "schema": 1,
        "current": current,
        "target": target,
        "addons": rows.iter().map(|row| serde_json::json!({
            "name": row.name,
            "kind": row.kind,
            "status": row.status,
            "url": row.url,
            "moved_from": row.moved_from,
            "renamed_to": row.renamed_to,
            "pull_request": row.pull_request,
            "effort": row.effort,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(orgs: &[&str], sources: Option<Vec<IndexSource>>) -> Vec<String> {
        let index = Index {
            ttl: None,
            orgs: Some(orgs.iter().map(|org| (*org).to_owned()).collect()),
            sources,
            overrides: BTreeMap::new(),
            renames: BTreeMap::new(),
        };
        organizations(Some(&index))
            .iter()
            .map(|(site, owner)| format!("{} {owner}", site.origin()))
            .collect()
    }

    #[test]
    fn dependencies_come_first_and_cycles_break_in_order() {
        let names: Vec<String> = ["c", "a", "b", "d"].map(str::to_owned).to_vec();
        let depends = |name: &str| -> Vec<String> {
            match name {
                "c" => vec!["a".to_owned(), "base".to_owned()],
                "a" => vec!["b".to_owned(), "a".to_owned()],
                "b" => vec!["a".to_owned()],
                _ => Vec::new(),
            }
        };
        assert_eq!(dependencies_first(&names, &depends), ["d", "a", "c", "b"]);
    }

    #[test]
    fn organizations_from_names_and_urls() {
        let orgs = [
            "OCA",
            "https://gitlab.com/g/s/",
            "https://gitlab.com/g/s",
            "http://x",
            "https://github.com/OCA",
        ];
        assert_eq!(
            listed(&orgs, None),
            ["https://github.com OCA", "https://gitlab.com g/s"]
        );
        assert_eq!(listed(&orgs[..1], Some(vec![IndexSource::Forge])).len(), 1);
        assert_eq!(listed(&orgs, Some(vec![IndexSource::Cache])).len(), 0);
        assert_eq!(organizations(None).len(), 0);
    }
}
