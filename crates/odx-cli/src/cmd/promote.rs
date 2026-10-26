//! `odx promote`: the plan of copying exactly what an environment runs for
//! some addons, with their dependencies, into the next environment.

use std::fmt::Write as _;

use odx_analyze::manifest::{Value, parse_literal, parse_manifest};
use odx_config::environments::Model;
use odx_core::Exit;
use odx_env::promote::{Addon, Conflict, Item, Options, Plan, PlanError, Snapshot, plan};
use odx_env::search::{Candidate, search};
use odx_env::{Change, Unit};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::prepare;
use super::checkout::{Checkout, environment, read};
use super::env::model;
use super::promote_apply::{Applied, addon_paths, apply, missing_commits};
use super::promote_forge::{merged, touched};
use super::promote_rollback::rollback;
use super::propose::{OpenedJson, opened_text};
use crate::Context;
use crate::cli::{GlobalArgs, PromoteArgs};

/// `odx promote`.
pub(crate) fn run_promote(args: &PromoteArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    promote(args, global, ctx).unwrap_or_else(|report| report)
}

fn usage(message: String) -> Report {
    Report::error(Exit::Usage, message)
}

/// `args` with `[tool.odx.promote] with_code` as the default of
/// `--with-code`.
fn with_defaults(args: &PromoteArgs, config: &odx_config::OdxConfig) -> PromoteArgs {
    let configured = config.promote.as_ref().and_then(|p| p.with_code);
    PromoteArgs {
        with_code: !args.no_with_code && (args.with_code || configured.unwrap_or(false)),
        ..args.clone()
    }
}

fn promote(args: &PromoteArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let root = &prepared.project.root;
    let config = &prepared.project.config;
    let args = &with_defaults(args, config);
    let model = model(config)?;
    let to = environment(&model, &args.to)?;
    if args.rollback.is_some() {
        return rollback(ctx, root, &model, args, to, global);
    }
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
        (None, None) if args.from_pr.is_some() => from_pr(
            ctx,
            root,
            &from,
            &args.remote,
            args.from_pr.unwrap_or_default(),
        )?,
        (None, None) => args.modules.clone(),
    };
    let defaults = config.promote.as_ref();
    let options = Options {
        allow_downgrade: args.allow_downgrade
            || defaults.and_then(|p| p.allow_downgrade).unwrap_or(false),
        allow_source_change: args.allow_source_change,
        with_odoo: !args.no_with_odoo && defaults.and_then(|p| p.with_odoo).unwrap_or(true),
    };
    let ((ours, unreadable), (theirs, _)) = (addons(&from), addons(&to));
    let mut planned = plan(
        &snapshot(&from, &ours),
        &snapshot(&to, &theirs),
        &selection,
        options,
    )
    .map_err(|error| refused(&error, &from, &to))?;
    planned.warnings.extend(unreadable.iter().map(|name| {
        format!(
            "the manifest of {name} on {} does not parse: its dependencies are not promoted with it",
            from.env.name
        )
    }));
    let commits = missing_commits(root, &from.from, &to.from, &addon_paths(&from, &planned));
    let applied = if (args.commit || args.open_pr)
        && !planned.refused()
        && !(planned.is_noop() && commits.is_empty())
    {
        Some(apply(
            ctx,
            root,
            &prepared.cache,
            &model,
            args,
            [&from, &to],
            &planned,
        )?)
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

fn refused(error: &PlanError, from: &Checkout, to: &Checkout) -> Report {
    match error {
        PlanError::SeriesMismatch => Report::error(
            Exit::Failure,
            format!(
                "{} runs {} and {} runs {}: a promotion across series is refused; `odx upgrade` prepares one",
                from.env.name, from.env.series, to.env.name, to.env.series
            ),
        ),
        PlanError::Missing(names) => {
            usage(format!("not on {}: {}", from.env.name, names.join(", ")))
        }
    }
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

/// The project addons of a checkout, as the plan sees them, and those
/// whose manifest does not parse (their dependencies are unknown).
fn addons(checkout: &Checkout) -> (Vec<Addon>, Vec<String>) {
    let mut unreadable = Vec::new();
    let addons = checkout
        .addons
        .iter()
        .map(|addon| {
            let manifest = parse_manifest(&addon.manifest);
            if manifest.is_err() {
                unreadable.push(addon.name.clone());
            }
            Addon {
                name: addon.name.clone(),
                version: odx_analyze::sniff::manifest_version(&addon.manifest),
                tree: addon.tree.clone(),
                depends: manifest.map(|m| m.depends).unwrap_or_default(),
            }
        })
        .collect();
    (addons, unreadable)
}

/// The project addons pull request `number`, merged into the source,
/// changed.
fn from_pr(
    ctx: &Context<'_>,
    root: &std::path::Path,
    from: &Checkout,
    remote: &str,
    number: u64,
) -> Result<Vec<String>, Report> {
    let found = merged(ctx, root, &from.env, remote, number)?;
    let files = touched(root, &found.commits);
    let addons: Vec<String> = from
        .addons
        .iter()
        .filter(|addon| {
            files
                .iter()
                .any(|file| file.starts_with(&format!("{}/", addon.path)))
        })
        .map(|addon| addon.name.clone())
        .collect();
    if addons.is_empty() {
        return Err(usage(format!(
            "#{number} changed no project addon of {}",
            from.env.name
        )));
    }
    Ok(addons)
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
        super::to_json(&SearchJson {
            schema: 1,
            query,
            candidates: hits
                .iter()
                .map(|(candidate, modules, score)| CandidateJson {
                    kind: if candidate.feature {
                        CandidateKind::Feature
                    } else {
                        CandidateKind::Addon
                    },
                    name: &candidate.name,
                    score: *score,
                    modules,
                })
                .collect(),
        })
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

/// What `odx --json promote --search` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx promote --search")]
pub(crate) struct SearchJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// What was searched for.
    query: &'a str,
    /// What matches, best first.
    candidates: Vec<CandidateJson<'a>>,
}

/// What a candidate is.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum CandidateKind {
    /// A feature: a group of addons.
    Feature,
    /// One addon.
    Addon,
}

/// Something `--select` can promote.
#[derive(Debug, Serialize, JsonSchema)]
struct CandidateJson<'a> {
    /// A feature or an addon.
    kind: CandidateKind,
    /// Its name, for `--select`.
    name: &'a str,
    /// How well it matches; higher is better.
    score: u32,
    /// The addons it promotes.
    modules: &'a [String],
}

/// What `odx --json promote` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx promote")]
pub(crate) struct PromoteJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The source environment.
    from: SideJson<'a>,
    /// The target environment.
    to: SideJson<'a>,
    /// The addons asked for.
    selection: &'a [String],
    /// Every addon of the closure, and how it differs.
    items: Vec<ItemJson<'a>>,
    /// Addons not copied: Odoo's own, or of aggregated repositories.
    external: &'a [String],
    /// The Odoo core on both sides; null when it is not copied.
    core: Option<CoreJson<'a>>,
    /// What the plan warns about.
    warnings: &'a [String],
    /// Whether a conflict refuses the plan.
    refused: bool,
    /// Whether the target has all of it.
    noop: bool,
    /// The source's commits on the closure's project addons the target
    /// lacks.
    code: Vec<CommitJson<'a>>,
    /// What writing the plan did; null for a plan only.
    applied: Option<AppliedJson<'a>>,
}

/// An environment, as read.
#[derive(Debug, Serialize, JsonSchema)]
struct SideJson<'a> {
    /// The environment.
    env: &'a str,
    /// Its branch.
    branch: &'a str,
    /// Its Odoo series.
    series: &'a str,
    /// The git ref it was read from.
    from: &'a str,
}

impl<'a> SideJson<'a> {
    fn of(checkout: &'a Checkout) -> Self {
        Self {
            env: &checkout.env.name,
            branch: &checkout.env.branch,
            series: &checkout.env.series,
            from: &checkout.from,
        }
    }
}

/// One thing an environment runs, and its exact pin.
#[derive(Debug, Serialize, JsonSchema)]
struct UnitJson<'a> {
    /// `odoo`, `addon`, `package` or `repository`.
    kind: &'static str,
    /// Addon, package or repository name.
    name: &'a str,
    /// Its version, when it has one.
    version: Option<&'a str>,
    /// Where it comes from: `project`, `index`, `git <url>`, a URL.
    source: &'a str,
    /// What identifies its content: a wheel hash, a commit, a tree.
    pin: &'a str,
}

impl<'a> UnitJson<'a> {
    fn of(unit: &'a Unit) -> Self {
        Self {
            kind: unit.kind.name(),
            name: &unit.name,
            version: unit.version.as_deref(),
            source: &unit.source,
            pin: &unit.pin,
        }
    }
}

/// An addon of the closure.
#[derive(Debug, Serialize, JsonSchema)]
struct ItemJson<'a> {
    /// The addon.
    addon: &'a str,
    /// How the source differs from the target: `identical`, `ahead`,
    /// `behind`, `changed`, `source-changed`, `only-in-a` or `only-in-b`.
    change: &'static str,
    /// `downgrade`, `source change` or `version conflict`; null when none.
    conflict: Option<&'static str>,
    /// Whether a flag allows the conflict.
    allowed: bool,
    /// On the source.
    from: UnitJson<'a>,
    /// On the target; null when it lacks it.
    to: Option<UnitJson<'a>>,
}

/// The Odoo core on both sides.
#[derive(Debug, Serialize, JsonSchema)]
struct CoreJson<'a> {
    /// On the source.
    from: UnitJson<'a>,
    /// On the target; null when it lacks it.
    to: Option<UnitJson<'a>>,
}

/// A commit.
#[derive(Debug, Serialize, JsonSchema)]
struct CommitJson<'a> {
    /// Its hash.
    sha: &'a str,
    /// Its subject.
    subject: &'a str,
}

/// What writing the plan did.
#[derive(Debug, Serialize, JsonSchema)]
struct AppliedJson<'a> {
    /// The branch written.
    branch: &'a str,
    /// Its head.
    commit: &'a str,
    /// Commits cherry-picked onto it.
    picked: usize,
    /// Branch-specific paths included on request.
    included: &'a [String],
    /// The pull request opened for it; null without `--open-pr`.
    pull_request: Option<OpenedJson<'a>>,
}

fn json(from: &Checkout, to: &Checkout, plan: &Plan, done: &Done<'_>) -> String {
    let value = PromoteJson {
        schema: 1,
        from: SideJson::of(from),
        to: SideJson::of(to),
        selection: &plan.selection,
        items: plan
            .items
            .iter()
            .map(|item| ItemJson {
                addon: &item.addon,
                change: item.change.name(),
                conflict: item.conflict.map(Conflict::name),
                allowed: item.allowed,
                from: UnitJson::of(&item.from),
                to: item.to.as_ref().map(UnitJson::of),
            })
            .collect(),
        external: &plan.external,
        core: plan.core.as_ref().map(|(ours, theirs)| CoreJson {
            from: UnitJson::of(ours),
            to: theirs.as_ref().map(UnitJson::of),
        }),
        warnings: &plan.warnings,
        refused: plan.refused(),
        noop: plan.is_noop() && done.commits.is_empty(),
        code: done
            .commits
            .iter()
            .map(|commit| {
                let (sha, subject) = commit.split_once(' ').unwrap_or((commit, ""));
                CommitJson { sha, subject }
            })
            .collect(),
        applied: done.applied.map(|applied| AppliedJson {
            branch: &applied.branch,
            commit: &applied.commit,
            picked: applied.picked,
            included: &applied.included,
            pull_request: applied.pushed.as_ref().map(OpenedJson::of),
        }),
    };
    super::to_json(&value)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// `odx promote`'s arguments on their own.
    #[derive(Parser)]
    struct Promote {
        #[command(flatten)]
        args: PromoteArgs,
    }

    fn parse(flags: &[&str]) -> Result<PromoteArgs, clap::Error> {
        let mut line = vec!["promote", "sale_x", "--to", "prod"];
        line.extend_from_slice(flags);
        Promote::try_parse_from(line).map(|promote| promote.args)
    }

    fn args(flags: &[&str]) -> PromoteArgs {
        parse(flags).unwrap()
    }

    fn config(with_code: Option<bool>) -> odx_config::OdxConfig {
        let text = with_code.map_or_else(String::new, |on| {
            format!("[tool.odx.promote]\nwith_code = {on}\n")
        });
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{text}");
        odx_config::parse_pyproject(&text, std::path::Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config
    }

    #[test]
    fn with_code_defaults_to_the_configuration() {
        let with_code = |flags: &[&str], on| with_defaults(&args(flags), &config(on)).with_code;
        assert!(!with_code(&[], None));
        assert!(with_code(&["--with-code"], None));
        assert!(with_code(&[], Some(true)));
        assert!(!with_code(&["--no-with-code"], Some(true)));
        assert!(!with_code(&[], Some(false)));
        assert!(parse(&["--with-code", "--no-with-code"]).is_err());
    }
}
