//! `odx promote`: the plan of copying exactly what an environment runs for
//! some addons, with their dependencies, into the next environment.

use std::fmt::Write as _;

use odx_analyze::manifest::{Value, parse_literal, parse_manifest};
use odx_config::environments::Model;
use odx_core::Exit;
use odx_env::promote::{Addon, Conflict, Item, Options, Plan, PlanError, Snapshot, plan};
use odx_env::search::{Candidate, search};
use odx_env::{Change, Unit};

use super::Report;
use super::aggregate::prepare;
use super::checkout::{Checkout, environment, read};
use super::env::model;
use super::promote_apply::{Applied, addon_paths, apply, missing_commits};
use super::propose::{opened_json, opened_text};
use crate::Context;
use crate::cli::{GlobalArgs, PromoteArgs};

/// `odx promote`.
pub(crate) fn run_promote(args: &PromoteArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    promote(args, global, ctx).unwrap_or_else(|report| report)
}

fn usage(message: String) -> Report {
    Report::error(Exit::Usage, message)
}

fn promote(args: &PromoteArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let root = &prepared.project.root;
    let config = &prepared.project.config;
    let model = model(config)?;
    let to = environment(&model, &args.to)?;
    let from_name = match &args.from {
        Some(name) => name.clone(),
        None => model
            .default_source(&to.name)
            .map(str::to_owned)
            .map_err(usage)?,
    };
    let from = environment(&model, &from_name)?;
    allowed(&model, &from.name, &to.name)?;
    let from = read(root, from, &args.remote)?;
    let to = read(root, to, &args.remote)?;
    let selection = match (&args.search, &args.feature) {
        (Some(query), _) if args.select.is_empty() => {
            return Ok(candidates(query, &from, global.json));
        }
        (Some(_), _) => args.select.clone(),
        (None, Some(feature)) => feature_modules(&from, feature)?,
        (None, None) => args.modules.clone(),
    };
    let defaults = config.promote.as_ref();
    let options = Options {
        allow_downgrade: args.allow_downgrade
            || defaults.and_then(|p| p.allow_downgrade).unwrap_or(false),
        allow_source_change: args.allow_source_change,
        with_odoo: !args.no_with_odoo && defaults.and_then(|p| p.with_odoo).unwrap_or(true),
    };
    let (ours, theirs) = (addons(&from), addons(&to));
    let planned = plan(
        &snapshot(&from, &ours),
        &snapshot(&to, &theirs),
        &selection,
        options,
    )
    .map_err(|error| match error {
        PlanError::SeriesMismatch => Report::error(
            Exit::Failure,
            format!(
                "{} runs {} and {} runs {}: a promotion across series is refused; `odx upgrade` prepares one",
                from.env.name, from.env.series, to.env.name, to.env.series
            ),
        ),
        PlanError::Missing(names) => usage(format!(
            "not on {}: {}",
            from.env.name,
            names.join(", ")
        )),
    })?;
    let commits = missing_commits(root, &from.from, &to.from, &addon_paths(&from, &planned));
    let applied = if (args.commit || args.open_pr)
        && !planned.refused()
        && !(planned.is_noop() && commits.is_empty())
    {
        Some(apply(ctx, root, &model, args, [&from, &to], &planned)?)
    } else {
        None
    };
    let done = Done {
        commits: &commits,
        with_code: args.with_code,
        applied: applied.as_ref(),
    };
    let out = if global.json {
        json(&from, &to, &planned, &done)
    } else {
        text(&from, &to, &planned, &done)
    };
    Ok(Report {
        exit: if planned.refused() {
            Exit::Failure
        } else {
            Exit::Success
        },
        out,
        ..Report::default()
    })
}

/// The side of `checkout` with its `addons`.
fn snapshot<'a>(checkout: &'a Checkout, addons: &'a [Addon]) -> Snapshot<'a> {
    Snapshot {
        env: &checkout.env.name,
        series: &checkout.env.series,
        addons,
        uv: checkout.uv.as_ref().map(|(_, uv)| uv),
        lock: checkout.lock.as_ref().map(|(_, lock)| lock),
    }
}

fn allowed(model: &Model, from: &str, to: &str) -> Result<(), Report> {
    if model.allows(from, to) {
        Ok(())
    } else {
        Err(usage(format!(
            "promoting {from} into {to} is not allowed (`promotion` in [tool.odx.environments])"
        )))
    }
}

/// The project addons of a checkout, as the plan sees them.
fn addons(checkout: &Checkout) -> Vec<Addon> {
    checkout
        .addons
        .iter()
        .map(|addon| {
            let manifest = parse_manifest(&addon.manifest).ok();
            Addon {
                name: addon.name.clone(),
                version: odx_analyze::sniff::manifest_version(&addon.manifest),
                tree: addon.tree.clone(),
                depends: manifest.map(|m| m.depends).unwrap_or_default(),
            }
        })
        .collect()
}

fn feature_modules(from: &Checkout, feature: &str) -> Result<Vec<String>, Report> {
    from.config
        .features
        .get(feature)
        .map(|feature| feature.modules.clone())
        .ok_or_else(|| {
            usage(format!(
                "no feature {feature:?} in [tool.odx.features] on {}",
                from.env.name
            ))
        })
}

/// The searchable texts of an addon's manifest: its name, `name`,
/// `summary`, `category`, `description`.
fn texts(name: &str, manifest: &str) -> Vec<String> {
    let parsed = parse_literal(manifest).ok();
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    vec![
        name.to_owned(),
        field("name"),
        field("summary"),
        field("category"),
        field("description"),
    ]
}

fn candidates(query: &str, from: &Checkout, json: bool) -> Report {
    let mut found: Vec<(Candidate, Vec<String>)> = from
        .config
        .features
        .iter()
        .map(|(name, feature)| {
            (
                Candidate {
                    feature: true,
                    name: name.clone(),
                    texts: vec![
                        name.clone(),
                        feature.description.clone().unwrap_or_default(),
                    ],
                },
                feature.modules.clone(),
            )
        })
        .collect();
    found.extend(from.addons.iter().map(|addon| {
        (
            Candidate {
                feature: false,
                name: addon.name.clone(),
                texts: texts(&addon.name, &addon.manifest),
            },
            vec![addon.name.clone()],
        )
    }));
    let all: Vec<Candidate> = found
        .iter()
        .map(|(candidate, _)| candidate.clone())
        .collect();
    let synonyms = from
        .config
        .promote
        .as_ref()
        .map(|promote| promote.search_synonyms.clone())
        .unwrap_or_default();
    let scores: std::collections::BTreeMap<usize, u32> = search(query, &all, &synonyms)
        .iter()
        .map(|hit| (hit.index, hit.score))
        .collect();
    let mut hits: Vec<(&Candidate, &Vec<String>, u32)> = found
        .iter()
        .enumerate()
        .filter_map(|(index, (candidate, modules))| {
            scores.get(&index).map(|score| (candidate, modules, *score))
        })
        .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.2));
    let out = if json {
        let listed: Vec<serde_json::Value> = hits
            .iter()
            .map(|(candidate, modules, score)| {
                serde_json::json!({
                    "kind": if candidate.feature { "feature" } else { "addon" },
                    "name": candidate.name,
                    "score": score,
                    "modules": modules,
                })
            })
            .collect();
        format!(
            "{:#}\n",
            serde_json::json!({ "schema": 1, "query": query, "candidates": listed })
        )
    } else if hits.is_empty() {
        format!("nothing on {} matches {query:?}\n", from.env.name)
    } else {
        let mut out = format!("candidates on {} for {query:?}:\n", from.env.name);
        for (candidate, modules, score) in &hits {
            let _ = write!(
                out,
                "  {:7}  {:4}  {}",
                if candidate.feature {
                    "feature"
                } else {
                    "addon"
                },
                score,
                candidate.name
            );
            if candidate.feature {
                let _ = write!(out, " ({})", modules.join(", "));
            }
            out.push('\n');
        }
        out.push_str("plan one with --select <name>[,<name>...]\n");
        out
    };
    Report {
        out,
        ..Report::default()
    }
}

fn describe(unit: Option<&Unit>) -> String {
    unit.map_or_else(
        || "-".to_owned(),
        |unit| {
            let pin: String = unit.pin.chars().take(14).collect();
            match &unit.version {
                Some(version) if *version != unit.pin => format!("{version} {pin}"),
                _ => pin,
            }
        },
    )
}

pub(crate) const fn status(change: Change) -> &'static str {
    match change {
        Change::Identical => "identical",
        Change::Ahead => "newer",
        Change::Behind => "older",
        Change::Changed => "changed",
        Change::SourceChanged => "other source",
        Change::OnlyInA | Change::OnlyInB => "new",
    }
}

fn verdict(item: &Item, to: &str) -> String {
    match (item.conflict, item.allowed) {
        (None, _) => String::new(),
        (Some(Conflict::VersionConflict), _) => {
            format!("  refused: version conflict (sync {to} into the source first)")
        }
        (Some(conflict), true) => format!("  allowed: {}", conflict.name()),
        (Some(conflict), false) => format!(
            "  refused: {} (--allow-{})",
            conflict.name(),
            match conflict {
                Conflict::Downgrade => "downgrade",
                _ => "source-change",
            }
        ),
    }
}

/// What came of the plan besides itself.
struct Done<'a> {
    /// The source's commits on the closure's project addons the target lacks.
    commits: &'a [String],
    /// Whether they were cherry-picked.
    with_code: bool,
    /// What writing the plan did.
    applied: Option<&'a Applied>,
}

fn text(from: &Checkout, to: &Checkout, plan: &Plan, done: &Done<'_>) -> String {
    let mut out = format!(
        "promote {} ({}) into {} ({}), series {}\nselection: {}\n",
        from.env.name,
        from.from,
        to.env.name,
        to.from,
        from.env.series,
        plan.selection.join(", ")
    );
    let width = plan
        .items
        .iter()
        .map(|item| item.addon.chars().count())
        .max()
        .unwrap_or(0);
    for item in &plan.items {
        let _ = writeln!(
            out,
            "  {:width$}  {:12}  {} | {}{}",
            item.addon,
            status(item.change),
            describe(Some(&item.from)),
            describe(item.to.as_ref()),
            verdict(item, &to.env.name)
        );
    }
    if !plan.external.is_empty() {
        let _ = writeln!(
            out,
            "not copied (Odoo's own addons, or of aggregated repositories): {}",
            plan.external.join(", ")
        );
    }
    if let Some((ours, theirs)) = &plan.core {
        let _ = writeln!(
            out,
            "core drift: odoo {} | {} (copied with the pins; --no-with-odoo leaves it)",
            describe(Some(ours)),
            describe(theirs.as_ref())
        );
    }
    if !done.commits.is_empty() {
        let _ = writeln!(
            out,
            "code on {} not on {}{}:",
            from.env.name,
            to.env.name,
            if done.with_code {
                ""
            } else {
                " (--with-code cherry-picks it)"
            }
        );
        for commit in done.commits {
            let (sha, subject) = commit.split_once(' ').unwrap_or((commit, ""));
            let short: String = sha.chars().take(7).collect();
            let _ = writeln!(out, "  {short} {subject}");
        }
    }
    for warning in &plan.warnings {
        let _ = writeln!(out, "warning: {warning}");
    }
    let copied = plan
        .items
        .iter()
        .filter(|item| item.change != Change::Identical)
        .count();
    if let Some(applied) = done.applied {
        if !applied.included.is_empty() {
            let _ = writeln!(
                out,
                "!!! branch-specific content included: {} !!!",
                applied.included.join(", ")
            );
        }
        let short: String = applied.commit.chars().take(7).collect();
        let _ = write!(out, "committed {short} on {}", applied.branch);
        if applied.picked > 0 {
            let _ = write!(out, " ({} commit(s) cherry-picked)", applied.picked);
        }
        out.push('\n');
        if let Some(pushed) = &applied.pushed {
            out.push_str(&opened_text(pushed));
        }
        return out;
    }
    let _ = if plan.refused() {
        writeln!(out, "refused: resolve the conflicts above; nothing written")
    } else if plan.is_noop() && done.commits.is_empty() {
        writeln!(out, "nothing to promote: {} has all of it", to.env.name)
    } else {
        writeln!(
            out,
            "{copied} to copy into {}; plan only (--commit or --open-pr with --yes writes it)",
            to.env.name
        )
    };
    out
}

fn json(from: &Checkout, to: &Checkout, plan: &Plan, done: &Done<'_>) -> String {
    let side = |checkout: &Checkout| {
        serde_json::json!({
            "env": checkout.env.name,
            "branch": checkout.env.branch,
            "series": checkout.env.series,
            "from": checkout.from,
        })
    };
    let unit = |unit: Option<&Unit>| {
        unit.map(|unit| {
            serde_json::json!({
                "kind": unit.kind.name(),
                "name": unit.name,
                "version": unit.version,
                "source": unit.source,
                "pin": unit.pin,
            })
        })
    };
    let items: Vec<serde_json::Value> = plan
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "addon": item.addon,
                "change": item.change.name(),
                "conflict": item.conflict.map(Conflict::name),
                "allowed": item.allowed,
                "from": unit(Some(&item.from)),
                "to": unit(item.to.as_ref()),
            })
        })
        .collect();
    let value = serde_json::json!({
        "schema": 1,
        "from": side(from),
        "to": side(to),
        "selection": plan.selection,
        "items": items,
        "external": plan.external,
        "core": plan.core.as_ref().map(|(ours, theirs)| serde_json::json!({
            "from": unit(Some(ours)),
            "to": unit(theirs.as_ref()),
        })),
        "warnings": plan.warnings,
        "refused": plan.refused(),
        "noop": plan.is_noop() && done.commits.is_empty(),
        "code": done.commits.iter().map(|commit| {
            let (sha, subject) = commit.split_once(' ').unwrap_or((commit, ""));
            serde_json::json!({ "sha": sha, "subject": subject })
        }).collect::<Vec<_>>(),
        "applied": done.applied.map(|applied| serde_json::json!({
            "branch": applied.branch,
            "commit": applied.commit,
            "picked": applied.picked,
            "included": applied.included,
            "pull_request": applied.pushed.as_ref().map(opened_json),
        })),
    });
    format!("{value:#}\n")
}
