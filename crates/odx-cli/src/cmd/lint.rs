//! `odx lint`: odoo-lint over the project's addons, in-process, with the
//! project's series and the aggregated, vendored and installed addons on
//! its addons path.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_analyze::manifest::parse_manifest;
use odx_analyze::stats::addon_dirs;
use odx_config::Problem;
use odx_core::Exit;
use odx_lint::project::{self, Addon, Rule};
use odx_lint::{Fixes, OutputFormat, Request, Violation};

use super::aggregate::prepare;
use super::propose::git;
use super::{Report, in_project, output_of};
use crate::Context;
use crate::cli::{GlobalArgs, LintArgs};

/// Run `odx lint`.
pub(crate) fn run_lint(args: &LintArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    lint(args, global, ctx).unwrap_or_else(|report| report)
}

/// The folders under `.venv/lib/python*/site-packages/odoo/addons`.
fn installed(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(".venv/lib")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("site-packages/odoo/addons"))
        .filter(|dir| dir.is_dir())
        .collect()
}

/// The project's root, series, addons path and own addon folders; the
/// current directory alone outside a project.
struct Scope {
    root: PathBuf,
    target: Option<String>,
    addons_path: Vec<PathBuf>,
    own: Vec<PathBuf>,
    /// `[tool.odx.lint] paths`, when set.
    paths: Option<Vec<PathBuf>>,
    /// `[tool.odx.lint] only_changed`.
    only_changed: bool,
    /// `[tool.odx] languages`, for odoo-lint's PO105.
    languages: Vec<String>,
    /// What odx's project rules need; none outside a project.
    facts: Option<Facts>,
}

/// The project's series, who provides which addon, and every addon a
/// dependency may name.
struct Facts {
    series: String,
    sources: BTreeMap<String, Vec<String>>,
    known: BTreeSet<String>,
    /// Whether odx knows Odoo's own addons of the series; without them
    /// every dependency would look unavailable.
    core_listed: bool,
}

fn scope(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Scope, Report> {
    if !in_project(global, ctx) {
        return Ok(Scope {
            root: ctx.dir.clone(),
            target: None,
            addons_path: Vec::new(),
            own: vec![ctx.dir.clone()],
            paths: None,
            only_changed: false,
            languages: Vec::new(),
            facts: None,
        });
    }
    let prepared = prepare(global, ctx)?;
    let config = &prepared.project.config;
    let root = prepared.project.root.clone();
    let own: Vec<PathBuf> = config
        .project_addon_dirs()
        .iter()
        .map(|dir| root.join(dir))
        .collect();
    let aggregated = plan_config(config, &root, None)
        .unwrap_or_default()
        .into_iter()
        .map(|repo| repo.target_dir);
    let vendored = config.group_dirs().into_values().map(|dir| root.join(dir));
    let addons_path = own
        .iter()
        .cloned()
        .chain(aggregated)
        .chain(vendored)
        .chain(installed(&root))
        .filter(|dir| dir.is_dir())
        .collect();
    let series = config.odoo_version.clone();
    let sources = super::check_deps::inventory(&prepared).sources;
    let core = odx_analyze::core::core_addons(&series);
    let core_listed = core.is_some();
    let known = core
        .unwrap_or_default()
        .into_iter()
        .map(str::to_owned)
        .chain(
            config
                .check_deps
                .as_ref()
                .and_then(|check| check.provided.clone())
                .unwrap_or_default(),
        )
        .chain(sources.keys().cloned())
        .collect();
    let lint = config.lint.clone().unwrap_or_default();
    Ok(Scope {
        target: Some(series.clone()),
        paths: lint
            .paths
            .map(|paths| paths.iter().map(|path| root.join(path)).collect()),
        only_changed: lint.only_changed.unwrap_or_default(),
        languages: config.languages.clone(),
        root,
        addons_path,
        own,
        facts: Some(Facts {
            series,
            sources,
            known,
            core_listed,
        }),
    })
}

/// The files changed against `base`, committed or not, untracked ones
/// included, as absolute paths.
fn changed(ctx: &Context<'_>, base: Option<&str>) -> Result<Vec<PathBuf>, Report> {
    // The top as a path from here, so it is spelled like the project's
    // paths (git resolves symlinks and short names).
    let up = output_of(&git(ctx).args(["rev-parse", "--show-cdup"])).map_err(|e| {
        Report::error(
            Exit::Usage,
            format!("--only-changed needs a git repository: {e}"),
        )
    })?;
    let base = base
        .map(str::to_owned)
        .or_else(|| {
            ["@{upstream}", "origin/HEAD"].into_iter().find_map(|name| {
                output_of(&git(ctx).args(["rev-parse", "--verify", "--quiet", name]))
                    .ok()
                    .map(|_| name.to_owned())
            })
        })
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                "the branch has no upstream and the clone no origin/HEAD: name what to compare with in --base"
                    .to_owned(),
            )
        })?;
    let committed = output_of(&git(ctx).args(["diff", "--name-only", &format!("{base}...HEAD")]))
        .map_err(|e| Report::error(Exit::Usage, format!("--base {base}: {e}")))?;
    let local = output_of(&git(ctx).args(["diff", "--name-only", "HEAD"])).unwrap_or_default();
    let untracked =
        output_of(&git(ctx).args(["ls-files", "--others", "--exclude-standard", "--full-name"]))
            .unwrap_or_default();
    let mut top = ctx.dir.clone();
    for _ in up.split('/').filter(|part| *part == "..") {
        top.pop();
    }
    Ok(committed
        .lines()
        .chain(local.lines())
        .chain(untracked.lines())
        .map(|file| top.join(file))
        .collect())
}

/// `path` relative to `dir` when it is below it, with `/` between its
/// parts as forges expect.
fn shown(dir: &Path, path: &str) -> String {
    Path::new(path).strip_prefix(dir).map_or_else(
        |_| path.to_owned(),
        |relative| relative.to_string_lossy().replace('\\', "/"),
    )
}

/// The project's own addons among `paths`: addon folders, or the addons in
/// them.
fn own_addons(paths: &[PathBuf], own: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = paths
        .iter()
        .flat_map(|path| {
            if path.join("__manifest__.py").is_file() {
                vec![path.clone()]
            } else {
                addon_dirs(path)
            }
        })
        .filter(|dir| own.iter().any(|root| dir.starts_with(root)))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The version of the manifest at `dir` as `tag` had it, when `dir`
/// changed since; none when it did not change or did not exist then.
fn released(ctx: &Context<'_>, tag: &str, dir: &Path) -> Option<String> {
    let dir = dir.to_string_lossy();
    let at = |args: &[&str]| output_of(&git(ctx).args(["-C", &dir]).args(args)).unwrap_or_default();
    if at(&["diff", "--name-only", tag, "--", "."]).is_empty() {
        return None;
    }
    parse_manifest(&at(&["show", &format!("{tag}:./__manifest__.py")]))
        .ok()
        .and_then(|manifest| manifest.version)
}

/// The names of the folders in `dir`.
fn folders(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// odx's project rules over the own addons among `paths`.
fn project_rules(
    ctx: &Context<'_>,
    facts: &Facts,
    paths: &[PathBuf],
    own: &[PathBuf],
    args: &LintArgs,
) -> Vec<Violation> {
    let on = |rule: Rule| project::enabled(rule, args.select.as_deref(), &args.ignore);
    let tag = output_of(&git(ctx).args(["describe", "--tags", "--abbrev=0"])).ok();
    let mut found = Vec::new();
    for dir in own_addons(paths, own) {
        let manifest = dir.join("__manifest__.py");
        let text = std::fs::read_to_string(&manifest).unwrap_or_default();
        let parsed = parse_manifest(&text).unwrap_or_default();
        let addon = Addon {
            name: dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir: dir.clone(),
            manifest,
            text,
            version: parsed.version,
            depends: parsed.depends,
        };
        if on(project::VERSION_SERIES) {
            found.extend(project::version_series(&addon, &facts.series));
        }
        if on(project::DEPENDS_UNAVAILABLE) && facts.core_listed {
            found.extend(project::depends_unavailable(&addon, &|dep| {
                facts.known.contains(dep)
            }));
        }
        if on(project::MIGRATION_VERSION) {
            let folders = folders(&dir.join("migrations"));
            found.extend(project::migration_folders(&addon, &facts.series, &folders));
        }
        if on(project::TWO_SOURCES) {
            let sources = facts.sources.get(&addon.name).cloned().unwrap_or_default();
            found.extend(project::two_sources(&addon, &sources));
        }
        if let Some(tag) = tag.as_deref().filter(|_| on(project::VERSION_NOT_BUMPED))
            && let Some(version) = released(ctx, tag, &dir)
        {
            found.extend(project::version_not_bumped(&addon, tag, &version));
        }
    }
    found
}

fn lint(args: &LintArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let scope = scope(global, ctx)?;
    let mut paths: Vec<PathBuf> = if args.paths.is_empty() {
        scope.paths.clone().unwrap_or_else(|| scope.own.clone())
    } else {
        args.paths.iter().map(|path| ctx.dir.join(path)).collect()
    };
    let mut report = Report::default();
    if args.only_changed || scope.only_changed {
        let addons: BTreeSet<PathBuf> = changed(ctx, args.base.as_deref())?
            .iter()
            .filter_map(|file| odx_lint::addon_of(&scope.root, file))
            .filter(|addon| paths.iter().any(|path| addon.starts_with(path)))
            .collect();
        if addons.is_empty() {
            return Ok(Report {
                out: "no addon changed\n".to_owned(),
                ..report
            });
        }
        paths = addons.into_iter().collect();
    }
    let rules = scope
        .facts
        .as_ref()
        .map(|facts| project_rules(ctx, facts, &paths, &scope.own, args))
        .unwrap_or_default();
    // Selectors naming odx's rules are odx's: odoo-lint would warn on them.
    let theirs = |selectors: &[String]| -> Vec<String> {
        selectors
            .iter()
            .filter(|selector| !project::is_odx(selector))
            .cloned()
            .collect()
    };
    let fixes = match (args.fix, args.unsafe_fixes) {
        (false, _) => Fixes::None,
        (true, false) => Fixes::Safe,
        (true, true) => Fixes::Unsafe,
    };
    let mut outcome = odx_lint::lint(&Request {
        root: scope.root,
        paths,
        target: scope.target,
        addons_path: scope.addons_path,
        languages: scope.languages,
        select: args.select.as_deref().map(theirs),
        ignore: theirs(&args.ignore),
        fixes,
        diff: args.diff,
    })
    .map_err(|e| Report::error(Exit::Usage, format!("odoo-lint: {e}")))?;
    report.warnings = outcome
        .warnings
        .iter()
        .map(|message| Problem {
            key: "odoo-lint".to_owned(),
            message: message.clone(),
        })
        .collect();
    if let Some(facts) = scope.facts.as_ref().filter(|facts| !facts.core_listed)
        && project::enabled(
            project::DEPENDS_UNAVAILABLE,
            args.select.as_deref(),
            &args.ignore,
        )
    {
        report.warnings.push(Problem {
            key: "odoo_version".to_owned(),
            message: format!(
                "no list of Odoo's own addons for {}: {} is skipped",
                facts.series,
                project::DEPENDS_UNAVAILABLE.code
            ),
        });
    }
    let mut violations: Vec<Violation> = std::mem::take(&mut outcome.violations)
        .into_iter()
        .chain(rules)
        .map(|violation| Violation {
            file_path: shown(&ctx.dir, &violation.file_path),
            ..violation
        })
        .collect();
    violations.sort();
    finish(args, &outcome, &violations, &mut report);
    Ok(report)
}

/// The output and exit status for what `outcome` found and did.
fn finish(
    args: &LintArgs,
    outcome: &odx_lint::Outcome,
    violations: &[Violation],
    report: &mut Report,
) {
    if args.diff {
        report.out = outcome.diffs.concat();
        let _ = writeln!(
            report.out,
            "{} fix(es) would change {} file(s)",
            outcome.fixed,
            outcome.diffs.len()
        );
        report.exit = if outcome.diffs.is_empty() || args.exit_zero {
            Exit::Success
        } else {
            Exit::Failure
        };
        return;
    }
    report.out = odx_lint::render(args.output_format, violations);
    if args.output_format == OutputFormat::Text {
        if args.fix {
            let _ = writeln!(
                report.out,
                "fixed {} finding(s) in {} file(s)",
                outcome.fixed,
                outcome.written.len()
            );
        }
        if violations.is_empty() {
            report.out.push_str("no findings\n");
        } else {
            let _ = writeln!(report.out, "{} finding(s)", violations.len());
        }
    }
    report.exit = if violations.is_empty() || args.exit_zero {
        Exit::Success
    } else {
        Exit::Failure
    };
}
