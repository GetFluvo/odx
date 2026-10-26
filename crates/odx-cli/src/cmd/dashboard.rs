//! `odx dashboard`: pinned pull requests, overrides to drop, new upstream
//! pull requests, forward-port and backport suggestions, index updates and
//! what is suppressed, per environment series; `odx dashboard act` takes an
//! entry's action.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use odx_aggregate::plan_config;
use odx_dashboard::{Action, Dashboard, Entry, Section, SectionKind, markdown, table};
use odx_forge::{CachedHttp, ForgeClient, PrState, RepoUrl, ReqwestHttp};
use odx_index::series_key;
use odx_port::analysis::Class;
use odx_verdicts::Standing;

use odx_config::environments::Model;
use odx_env::promote::addon_of_dist;
use odx_env::{Change, Kind as UnitKind, Unit, diff};

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::checkout::{self, environment};
use super::env_diff::units;
use super::status::{Row, rows};
use super::suggest::{Judged, collect, judge, repository_name};
use super::upgrade::{Kind, addons};
use crate::Context;
use crate::cli::{
    AddArgs, DashboardAct, DashboardActArgs, DashboardArgs, DashboardFormat, DropArgs, GlobalArgs,
    LockArgs, PromoteArgs, SuggestArgs, VerdictAddArgs,
};
use odx_config::Problem;
use odx_core::Exit;
use odx_core::system::OsRunner;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Where the last dashboard of the project is kept.
fn snapshot_file(prepared: &Prepared) -> std::path::PathBuf {
    prepared
        .cache
        .root()
        .join("snapshots")
        .join(super::verdict::project_id(prepared))
        .join("dashboard.json")
}

/// The kept dashboard, when it is younger than `[tool.odx.cache]
/// ttl.pr_state` (default 30 minutes).
fn snapshot(prepared: &Prepared) -> Option<Vec<Dashboard>> {
    let ttl = prepared
        .project
        .config
        .cache
        .as_ref()
        .and_then(|cache| cache.ttl.as_ref())
        .and_then(|ttl| ttl.pr_state)
        .map_or(30 * 60, |span| span.as_duration().as_secs());
    let kept: Vec<Dashboard> = std::fs::read(snapshot_file(prepared))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    let oldest = kept.iter().map(|d| d.generated_at).min().unwrap_or(0);
    (now().saturating_sub(oldest) <= ttl).then_some(kept)
}

/// Build the dashboards of `prepared` and keep them for the next
/// `odx dashboard` (what `odx sync` runs for every workspace).
pub(crate) fn refresh(
    ctx: &Context<'_>,
    prepared: &Prepared,
    global: &GlobalArgs,
) -> Result<usize, Report> {
    let args = DashboardArgs {
        format: DashboardFormat::Table,
        env: None,
        section: Vec::new(),
        checkboxes: false,
        act: None,
    };
    let (dashboards, _) = build(&args, global, ctx, prepared, &mut Vec::new())?;
    keep(prepared, &dashboards);
    Ok(dashboards
        .iter()
        .flat_map(|d| &d.sections)
        .map(|s| s.entries.len())
        .sum())
}

/// Keep `dashboards`; a cache that cannot be written only costs a rebuild.
fn keep(prepared: &Prepared, dashboards: &[Dashboard]) {
    let file = snapshot_file(prepared);
    let _ = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&file, serde_json::to_vec(dashboards).unwrap_or_default()));
}

/// `odx dashboard` and `odx dashboard act`.
pub(crate) fn run_dashboard(
    args: &DashboardArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    dashboard(args, global, ctx).unwrap_or_else(|report| report)
}

/// What `odx add` or `odx drop` take for an entry.
#[derive(Debug, Clone, Default)]
pub(crate) struct Edit {
    /// The spec, or the distribution of a package.
    spec: String,
    /// Whether `spec` is a distribution.
    package: bool,
    /// For an addon added as a package from its repository: the
    /// distribution and the subdirectory.
    add_as: Option<(String, String)>,
}

/// The [`Edit`] of each entry that has one, by id.
pub(crate) type Specs = BTreeMap<String, Edit>;

/// The id of a status row's entry.
fn row_id(row: &Row) -> String {
    format!("pr:{}#{}", repository_name(&row.url), row.pr)
}

/// The pinned and droppable sections, from `odx status`.
fn pinned(status: &[Row], specs: &mut Specs) -> [Section; 2] {
    for row in status {
        let (spec, package) = row.dist.clone().map_or_else(
            || (format!("{}#{}", row.url, row.pr), false),
            |dist| (dist, true),
        );
        specs.insert(
            row_id(row),
            Edit {
                spec,
                package,
                add_as: None,
            },
        );
    }
    let (merged, open): (Vec<&Row>, Vec<&Row>) = status.iter().partition(|row| {
        row.upstream
            .as_ref()
            .is_some_and(|pr| pr.state == PrState::Merged)
    });
    let entry = |row: &Row| {
        let state = row
            .upstream
            .as_ref()
            .map_or("unknown", |pr| pr.state.name());
        Entry {
            id: row_id(row),
            addon: None,
            subject: row.upstream.as_ref().map_or_else(
                || row.name.clone(),
                |pr| format!("{} - {}", row.name, pr.title),
            ),
            state: format!("{state}, head {}", row.head),
            actions: vec![Action::Drop],
            notes: std::iter::once(row.advice)
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    };
    let fetched_at = now();
    [
        Section {
            kind: SectionKind::Pinned,
            fetched_at,
            entries: open.into_iter().map(entry).collect(),
        },
        Section {
            kind: SectionKind::Droppable,
            fetched_at,
            entries: merged.into_iter().map(entry).collect(),
        },
    ]
}

/// The suggestion sections of `target`: forward ports (from lower
/// series), backports (from higher ones), and what is suppressed.
fn suggestions(judged: &[Judged<'_>], target: &str) -> [Section; 3] {
    let mut forward = Vec::new();
    let mut backward = Vec::new();
    let mut suppressed = Vec::new();
    for item in judged.iter().filter(|item| item.found.target == target) {
        let group = item.group;
        if !matches!(group.class, Class::Portable | Class::Blacklisted) {
            continue;
        }
        let lower = series_key(&item.found.source) < series_key(target);
        let id = format!(
            "{}:{}->{target}",
            if lower { "fwd" } else { "bwd" },
            group.key
        );
        let mut notes = Vec::new();
        if let Some(reason) = &group.reason {
            notes.push(format!("blacklisted upstream: {reason}"));
        }
        match item.verdict {
            Some((entry, Standing::Applies)) => {
                notes.push(format!(
                    "{} ({}): {}",
                    entry.id,
                    entry.kind.name(),
                    entry.reason
                ));
            }
            Some((entry, Standing::Stale)) => notes.push(format!(
                "previously declined ({}): {} (target code changed since {})",
                entry.kind.name(),
                entry.reason,
                entry.date
            )),
            None => {}
        }
        let hidden = item.hidden();
        let entry = Entry {
            id,
            addon: Some(item.found.addon.clone()),
            subject: format!("{}: {}", item.found.addon, group.title),
            state: format!("from {}", item.found.source),
            actions: if hidden {
                Vec::new()
            } else {
                vec![Action::Port, Action::Mute]
            },
            notes,
        };
        if hidden {
            suppressed.push(entry);
        } else if lower {
            forward.push(entry);
        } else {
            backward.push(entry);
        }
    }
    let fetched_at = now();
    [
        (SectionKind::ForwardPort, forward),
        (SectionKind::Backport, backward),
        (SectionKind::Suppressed, suppressed),
    ]
    .map(|(kind, entries)| Section {
        kind,
        fetched_at,
        entries,
    })
}

/// Open pull requests on the project's series in the repositories it
/// aggregates whose title names an addon the project uses from them and
/// that the project does not merge yet.
fn new_upstream(
    ctx: &Context<'_>,
    prepared: &Prepared,
    global: &GlobalArgs,
    specs: &mut Specs,
    warnings: &mut Vec<Problem>,
) -> Section {
    let config = &prepared.project.config;
    let planned = plan_config(config, &prepared.project.root, None).unwrap_or_default();
    let used = addons(ctx, prepared);
    let http =
        ReqwestHttp::new().map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let sleep = std::thread::sleep;
    let mut entries = Vec::new();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let mine: Vec<&str> = used
            .iter()
            .filter(|addon| {
                addon.kind == Kind::Aggregated && addon.url.as_deref() == Some(&repo.url)
            })
            .map(|addon| addon.name.as_str())
            .collect();
        let merged: BTreeSet<u32> = configured
            .merges
            .iter()
            .filter_map(|merge| merge.pr)
            .collect();
        // A forge that does not answer leaves the section without this
        // repository, and says so.
        let open = RepoUrl::parse(&repo.url)
            .filter(|_| !global.offline)
            .zip(http.as_ref().ok())
            .map(|(url, http)| {
                let client = ForgeClient::new(http, ctx.env, &OsRunner, &sleep);
                client
                    .kind(&url)
                    .and_then(|kind| client.open_pull_requests(&url, kind, &config.odoo_version))
                    .unwrap_or_else(|e| {
                        warnings.push(Problem {
                            key: format!("new upstream {}", repo.name),
                            message: e.to_string(),
                        });
                        Vec::new()
                    })
            })
            .unwrap_or_default();
        for pr in open {
            let named = mine.iter().find(|addon| pr.title.contains(**addon));
            let new = u32::try_from(pr.number).is_ok_and(|n| !merged.contains(&n));
            if let Some(addon) = named.filter(|_| new) {
                let id = format!("new:{}#{}", repository_name(&repo.url), pr.number);
                specs.insert(
                    id.clone(),
                    Edit {
                        spec: format!("{}#{}", repo.url, pr.number),
                        package: false,
                        add_as: None,
                    },
                );
                entries.push(Entry {
                    id,
                    addon: Some((*addon).to_owned()),
                    subject: pr.title.clone(),
                    state: "open".to_owned(),
                    actions: vec![Action::Include],
                    notes: pr.labels.clone(),
                });
            }
        }
    }
    Section {
        kind: SectionKind::NewUpstream,
        fetched_at: now(),
        entries,
    }
}

/// Index releases the auto-update policy would take, or leaves for review;
/// what keeps it from looking is a warning.
fn auto_update(ctx: &Context<'_>, prepared: &Prepared, warnings: &mut Vec<Problem>) -> Section {
    let (updated, review) = super::autoupdate::candidates(ctx, prepared)
        .unwrap_or_else(|report| {
            warnings.push(Problem {
                key: "auto-update".to_owned(),
                message: report.error.unwrap_or_default(),
            });
            None
        })
        .unwrap_or_default();
    let entries = updated
        .iter()
        .map(|update| (update, true))
        .chain(review.iter().map(|update| (update, false)))
        .map(|(update, safe)| Entry {
            id: format!("pkg:{}@{}", update.package, update.to),
            addon: None,
            subject: format!("{} {} -> {}", update.package, update.from, update.to),
            state: if safe {
                "safe".to_owned()
            } else {
                "review".to_owned()
            },
            actions: if safe {
                vec![Action::Follow]
            } else {
                Vec::new()
            },
            notes: std::iter::once(update.reason.clone())
                .filter(|r| !r.is_empty())
                .collect(),
        })
        .collect();
    Section {
        kind: SectionKind::AutoUpdate,
        fetched_at: now(),
        entries,
    }
}

/// The glue section: the glue addons `odx glue` finds, to add or to
/// ignore.
fn glue(prepared: &Prepared, specs: &mut Specs) -> Section {
    let series = &prepared.project.config.odoo_version;
    let found = super::glue::glue(prepared);
    let entries = found
        .candidates
        .iter()
        .map(|candidate| {
            let offered = &candidate.offered;
            let id = format!("glue:{}", offered.addon);
            let (spec, dist, path) = super::glue::add_spec(candidate, series);
            specs.insert(
                id.clone(),
                Edit {
                    spec,
                    package: false,
                    add_as: Some((dist, path)),
                },
            );
            Entry {
                id,
                addon: Some(offered.addon.clone()),
                subject: format!("{} from {}", offered.addon, repository_name(&offered.url)),
                state: format!("with {}", offered.triggers.join(", ")),
                actions: vec![Action::Include, Action::Mute],
                notes: (!candidate.missing.is_empty())
                    .then(|| format!("also needs {}", candidate.missing.join(", ")))
                    .into_iter()
                    .collect(),
            }
        })
        .collect();
    Section {
        kind: SectionKind::Glue,
        fetched_at: now(),
        entries,
    }
}

/// The pairs of environments the drift section compares:
/// `[tool.odx.dashboard] environment_pairs`, else every promotion edge
/// between environments that are not disposable.
fn pairs(config: &odx_config::OdxConfig, model: &Model) -> Vec<[String; 2]> {
    config
        .dashboard
        .as_ref()
        .and_then(|dashboard| dashboard.environment_pairs.clone())
        .unwrap_or_else(|| {
            model
                .promotion
                .iter()
                .filter(|pair| {
                    pair.iter()
                        .all(|name| model.get(name).is_some_and(|env| !env.disposable))
                })
                .cloned()
                .collect()
        })
}

/// A unit's version and pin, for people.
fn pin_of(unit: Option<&Unit>) -> String {
    unit.map_or_else(
        || "-".to_owned(),
        |unit| {
            let pin: String = unit.pin.chars().take(12).collect();
            match &unit.version {
                Some(version) if *version != unit.pin => format!("{version} {pin}"),
                _ => pin,
            }
        },
    )
}

/// What differs between the environments of each pair, through git; a
/// pair that cannot be read is a warning.
fn drift(prepared: &Prepared, warnings: &mut Vec<Problem>) -> Option<Section> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    // The configuration was validated when it was loaded.
    let model = Model::resolve(config).ok().flatten()?;
    let mut entries = Vec::new();
    let pairs = pairs(config, &model);
    for (index, [a, b]) in pairs.iter().enumerate() {
        // With its reverse pair shown too, each difference is listed once:
        // what the second side has more of goes with the reverse pair, a
        // change with whichever pair comes first.
        let reverse = pairs
            .iter()
            .position(|pair| *pair == [b.clone(), a.clone()]);
        let listed = |change: Change| match (change, reverse) {
            (Change::Behind | Change::OnlyInB, Some(_)) => false,
            (Change::Changed | Change::SourceChanged, Some(at)) => index < at,
            _ => change != Change::Identical,
        };
        let read = |name: &str| {
            environment(&model, name).and_then(|env| checkout::read(root, env, "origin"))
        };
        let (ours, theirs) = match read(a).and_then(|ours| read(b).map(|theirs| (ours, theirs))) {
            Ok(both) => both,
            Err(report) => {
                warnings.push(Problem {
                    key: format!("drift {a}->{b}"),
                    message: report.error.unwrap_or_default(),
                });
                continue;
            }
        };
        let allowed = model.allows(a, b);
        for entry in diff(&units(&ours), &units(&theirs)) {
            if !listed(entry.change) {
                continue;
            }
            let addon = match entry.kind {
                UnitKind::Addon => Some(entry.name.clone()),
                UnitKind::Package => addon_of_dist(&entry.name),
                UnitKind::Odoo | UnitKind::Repository => None,
            };
            let promotable = allowed
                && addon.is_some()
                && matches!(
                    entry.change,
                    Change::Ahead | Change::OnlyInA | Change::Changed
                );
            let state = match entry.change {
                Change::Ahead => format!("newer on {a}"),
                Change::Behind => format!("older on {a}"),
                Change::SourceChanged => "from another source".to_owned(),
                Change::OnlyInA => format!("only on {a}"),
                Change::OnlyInB => format!("only on {b}"),
                Change::Changed | Change::Identical => "changed".to_owned(),
            };
            entries.push(Entry {
                id: format!("drift:{a}->{b}:{}/{}", entry.kind.name(), entry.name),
                subject: match entry.kind {
                    UnitKind::Repository => format!("repository {}", entry.name),
                    _ => entry.name.clone(),
                },
                addon,
                state,
                actions: if promotable {
                    vec![Action::Promote]
                } else {
                    Vec::new()
                },
                notes: vec![format!(
                    "{a}: {} | {b}: {}",
                    pin_of(entry.a.as_ref()),
                    pin_of(entry.b.as_ref())
                )],
            });
        }
    }
    Some(Section {
        kind: SectionKind::Drift,
        fetched_at: now(),
        entries,
    })
}

/// The dashboards of every target series (`--env` narrows them), with the
/// specs of their pull request entries.
pub(crate) fn build(
    args: &DashboardArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
    warnings: &mut Vec<Problem>,
) -> Result<(Vec<Dashboard>, Specs), Report> {
    let mut specs = Specs::new();
    let config = &prepared.project.config;
    let suggest = SuggestArgs {
        env: args.env.clone(),
        series: None,
        from: Vec::new(),
        addon: None,
        show_muted: true,
        check_apply: false,
    };
    let found = collect(&suggest, global, ctx, prepared, warnings)?;
    let entries = super::verdict::entries(ctx, prepared, global, warnings);
    let judged = judge(&found, &entries, args.env.as_deref());
    // `collect` checked the environments already.
    let mut targets: BTreeSet<String> = super::suggest::targets(&suggest, prepared)
        .unwrap_or_default()
        .into_iter()
        .collect();
    targets.insert(config.odoo_version.clone());
    let planned = plan_config(config, &prepared.project.root, None).unwrap_or_default();
    let status = rows(prepared, &planned, global, ctx);
    let mut dashboards = Vec::new();
    for target in targets {
        let mut sections = Vec::new();
        let own = target == config.odoo_version;
        if own {
            sections.extend(pinned(&status, &mut specs));
            sections.push(new_upstream(ctx, prepared, global, &mut specs, warnings));
        }
        sections.extend(suggestions(&judged, &target));
        if own {
            sections.push(auto_update(ctx, prepared, warnings));
            sections.push(glue(prepared, &mut specs));
            sections.extend(drift(prepared, warnings));
        }
        sections.sort_by_key(|section| {
            SectionKind::ALL
                .iter()
                .position(|kind| *kind == section.kind)
        });
        dashboards.push(Dashboard {
            schema: 1,
            series: target,
            generated_at: now(),
            sections,
        });
    }
    Ok((dashboards, specs))
}

fn dashboard(
    args: &DashboardArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    if let Some(name) = args
        .section
        .iter()
        .find(|name| SectionKind::parse(name).is_none())
    {
        let known: Vec<&str> = SectionKind::ALL.iter().map(|kind| kind.name()).collect();
        return Err(Report::error(
            Exit::Usage,
            format!("no section {name:?}; sections are {}", known.join(", ")),
        ));
    }
    #[cfg(not(feature = "tui"))]
    if args.format == DashboardFormat::Tui && args.act.is_none() {
        return Err(Report::error(
            Exit::Usage,
            "this odx was built without the interactive view (the `tui` feature)".to_owned(),
        ));
    }
    let prepared = prepare(global, ctx)?;
    #[cfg(feature = "tui")]
    if args.format == DashboardFormat::Tui && args.act.is_none() {
        return interactive(args, global, ctx, &prepared);
    }
    let mut warnings = Vec::new();
    // A recent dashboard (odx sync keeps one) answers at once; actions and
    // --fresh rebuild.
    let kept = snapshot(&prepared).filter(|_| args.act.is_none() && !global.fresh);
    let (dashboards, specs, note) = if let Some(dashboards) = kept {
        let age =
            now().saturating_sub(dashboards.iter().map(|d| d.generated_at).min().unwrap_or(0));
        let note = format!("data as of {} min ago; --fresh rebuilds it\n", age / 60);
        (dashboards, Specs::new(), note)
    } else {
        let (dashboards, specs) = build(args, global, ctx, &prepared, &mut warnings)?;
        keep(&prepared, &dashboards);
        (dashboards, specs, String::new())
    };
    if let Some(DashboardAct::Act(act)) = &args.act {
        return run_act(act, &dashboards, &specs, global, ctx, &prepared);
    }
    // --section, else `[tool.odx.dashboard] sections`, else every section.
    let configured: Vec<String> = prepared
        .project
        .config
        .dashboard
        .as_ref()
        .and_then(|dashboard| dashboard.sections.as_ref())
        .map(|sections| sections.iter().map(|s| s.name().to_owned()).collect())
        .unwrap_or_default();
    let wanted = if args.section.is_empty() {
        &configured
    } else {
        &args.section
    };
    let dashboards: Vec<Dashboard> = dashboards
        .into_iter()
        .map(|mut dashboard| {
            dashboard.sections.retain(|section| {
                wanted.is_empty() || wanted.iter().any(|name| name == section.kind.name())
            });
            dashboard
        })
        .collect();
    let out = if global.json {
        super::to_json(&dashboards)
    } else {
        dashboards
            .iter()
            .map(|dashboard| match args.format {
                DashboardFormat::Markdown => markdown(dashboard, args.checkboxes),
                DashboardFormat::Table | DashboardFormat::Tui => table(dashboard),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    Ok(Report {
        out: if global.json {
            out
        } else {
            format!("{out}{note}")
        },
        warnings,
        ..Report::default()
    })
}

/// `odx dashboard act <id> --port`: the suggestion found again, then
/// ported in a clone under the cache.
fn act_port(
    act: &DashboardActArgs,
    dashboard: &Dashboard,
    entry: &Entry,
    reference: &str,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
) -> Result<Report, Report> {
    let source = entry.state.strip_prefix("from ").unwrap_or_default();
    let suggest = SuggestArgs {
        env: None,
        series: Some(dashboard.series.clone()),
        from: vec![source.to_owned()],
        addon: entry.addon.clone(),
        show_muted: true,
        check_apply: false,
    };
    let mut warnings = Vec::new();
    // The dashboard was just built from the same suggestions.
    super::verdict::suggestion(&suggest, reference, global, ctx, prepared, &mut warnings).map(
        |(found, group)| {
            super::port::port_suggestion(ctx, global, prepared, &found, &group, act.open_pr)
        },
    )
}

/// `odx dashboard act <id> --promote`: the promotion of the entry's addon
/// along its pair, planned (or, with `--open-pr`, written and proposed).
fn act_promote(
    act: &DashboardActArgs,
    entry: &Entry,
    reference: &str,
    rest: &str,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    // `drift:<from>-><to>:<kind>/<name>`, as `drift` writes it.
    let to = rest
        .split_once("->")
        .and_then(|(_, after)| after.split_once(':'))
        .map(|(to, _)| to.to_owned())
        .unwrap_or_default();
    let promote = PromoteArgs {
        modules: entry.addon.iter().cloned().collect(),
        from_pr: None,
        rollback: None,
        feature: None,
        search: None,
        select: Vec::new(),
        from: Some(reference.to_owned()),
        to,
        remote: "origin".to_owned(),
        allow_downgrade: false,
        allow_source_change: false,
        no_with_odoo: false,
        // A project addon's drift is its code.
        with_code: true,
        no_with_code: false,
        allow_transitive: false,
        include_branch_specific: Vec::new(),
        commit: false,
        open_pr: act.open_pr,
        yes: act.open_pr,
    };
    super::promote::run_promote(&promote, global, ctx)
}

/// The action the flags of `odx dashboard act` name.
const fn chosen(act: &DashboardActArgs) -> Action {
    if act.include {
        Action::Include
    } else if act.port {
        Action::Port
    } else if act.mute {
        Action::Mute
    } else if act.drop {
        Action::Drop
    } else if act.promote {
        Action::Promote
    } else {
        Action::Follow
    }
}

/// `odx dashboard act <id>`: one action, as the command behind it.
pub(crate) fn run_act(
    act: &DashboardActArgs,
    dashboards: &[Dashboard],
    specs: &Specs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
) -> Result<Report, Report> {
    let action = chosen(act);
    if act.open_pr && !matches!(action, Action::Port | Action::Promote) {
        return Err(Report::error(
            Exit::Usage,
            "--open-pr goes with --port or --promote".to_owned(),
        ));
    }
    let (dashboard, entry) = dashboards
        .iter()
        .find_map(|dashboard| dashboard.entry(&act.id).map(|entry| (dashboard, entry)))
        .ok_or_else(|| Report::error(Exit::Usage, format!("no dashboard entry {}", act.id)))?;
    if !entry.actions.contains(&action) {
        return Err(Report::error(
            Exit::Usage,
            format!(
                "{} cannot be {}d here; it offers: {}",
                act.id,
                action.name(),
                names(&entry.actions)
            ),
        ));
    }
    let rest = act.id.split_once(':').map_or("", |(_, rest)| rest);
    let reference = rest.split_once("->").map_or(rest, |(change, _)| change);
    let Edit {
        spec,
        package,
        add_as,
    } = specs.get(&act.id).cloned().unwrap_or_default();
    match action {
        Action::Port => act_port(act, dashboard, entry, reference, global, ctx, prepared),
        Action::Mute if act.id.starts_with("glue:") => {
            super::glue::ignore(prepared, rest, act.reason.as_deref())
        }
        Action::Mute => {
            let reason = act
                .reason
                .clone()
                .ok_or_else(|| Report::error(Exit::Usage, "--mute needs --reason".to_owned()))?;
            let source = entry
                .state
                .strip_prefix("from ")
                .unwrap_or_default()
                .to_owned();
            let add = VerdictAddArgs {
                change: reference.to_owned(),
                addon: entry.addon.clone().unwrap_or_default(),
                target: Some(dashboard.series.clone()),
                from: vec![source],
                kind: act.kind,
                reason,
                scope: None,
                environments: Vec::new(),
                until: act.until.clone(),
                no_recheck: false,
            };
            Ok(super::verdict::run_verdict(
                &crate::cli::VerdictCommand::Add(add),
                global,
                ctx,
            ))
        }
        Action::Drop => {
            let drop = DropArgs {
                spec,
                package,
                reason: act.reason.clone(),
            };
            Ok(super::edit::run_drop(&drop, ctx))
        }
        Action::Include => {
            let (package, subdirectory) = add_as.unzip();
            let add = AddArgs {
                spec,
                package,
                subdirectory,
                group: None,
                odoo_version: None,
            };
            Ok(super::edit::run_add(&add, ctx))
        }
        Action::Promote => Ok(act_promote(act, entry, reference, rest, global, ctx)),
        Action::Follow => {
            let lock = LockArgs {
                repo: None,
                migrate: false,
                check: false,
                update: false,
                follow: Vec::new(),
                bundle: None,
                no_uv: false,
                no_push: false,
                auto_update: true,
            };
            Ok(super::aggregate::run_lock(&lock, global, ctx))
        }
    }
}

fn names(actions: &[Action]) -> String {
    let names: Vec<&str> = actions.iter().map(|action| action.name()).collect();
    if names.is_empty() {
        "nothing".to_owned()
    } else {
        names.join(", ")
    }
}

/// `odx dashboard --format tui`: the view in the terminal until `q`.
// A terminal session cannot run under test. The view's state, keys and
// drawing are tested in odx-dashboard, its actions as `odx dashboard act`.
#[cfg(feature = "tui")]
#[cfg_attr(coverage_nightly, coverage(off))]
fn interactive(
    args: &DashboardArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
) -> Result<Report, Report> {
    use odx_dashboard::tui::{App, Command, key_of};
    use ratatui::crossterm::event::{self, Event, KeyEventKind};

    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() {
        return Err(Report::error(
            Exit::Usage,
            "the interactive view needs a terminal; use --format table or markdown".to_owned(),
        ));
    }
    let mut warnings = Vec::new();
    let (mut dashboards, mut specs) = build(args, global, ctx, prepared, &mut warnings)?;
    keep(prepared, &dashboards);
    let mut app = App::new(&dashboards);
    let mut terminal = ratatui::try_init()
        .map_err(|e| Report::error(Exit::Failure, format!("cannot open the terminal: {e}")))?;
    let fresh = GlobalArgs {
        fresh: true,
        ..global.clone()
    };
    let outcome = loop {
        if let Err(e) = terminal.draw(|frame| app.render(frame)) {
            break Err(e.to_string());
        }
        let key = match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => key_of(key),
            Ok(_) => None,
            Err(e) => break Err(e.to_string()),
        };
        let Some(command) = key.and_then(|key| app.key(key)) else {
            continue;
        };
        if let Command::Act { id, action, reason } = &command {
            let act = DashboardActArgs {
                id: id.clone(),
                include: *action == Action::Include,
                port: *action == Action::Port,
                open_pr: false,
                mute: *action == Action::Mute,
                drop: *action == Action::Drop,
                follow: *action == Action::Follow,
                promote: *action == Action::Promote,
                reason: reason.clone(),
                kind: crate::cli::VerdictKind::NotWanted,
                until: None,
            };
            let report =
                run_act(&act, &dashboards, &specs, &fresh, ctx, prepared).unwrap_or_else(|r| r);
            app.status = report
                .error
                .or_else(|| report.out.lines().next().map(str::to_owned))
                .unwrap_or_else(|| format!("{} {id}: done", action.name()));
        }
        match command {
            Command::Quit => break Ok(()),
            Command::Refresh | Command::Act { .. } => {
                if let Ok((built, built_specs)) = build(args, &fresh, ctx, prepared, &mut warnings)
                {
                    keep(prepared, &built);
                    app.load(&built);
                    dashboards = built;
                    specs = built_specs;
                }
            }
        }
    };
    ratatui::restore();
    outcome
        .map(|()| Report {
            warnings,
            ..Report::default()
        })
        .map_err(|e| Report::error(Exit::Failure, e))
}
