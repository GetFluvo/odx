//! `odx lint`: odoo-lint over the project's addons, in-process, with the
//! project's series and the aggregated, vendored and installed addons on
//! its addons path.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_config::Problem;
use odx_core::Exit;
use odx_lint::{Fixes, OutputFormat, Request};

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
}

fn scope(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Scope, Report> {
    if !in_project(global, ctx) {
        return Ok(Scope {
            root: ctx.dir.clone(),
            target: None,
            addons_path: Vec::new(),
            own: vec![ctx.dir.clone()],
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
    Ok(Scope {
        target: Some(config.odoo_version.clone()),
        root,
        addons_path,
        own,
    })
}

/// The files changed against `base`, committed or not, untracked ones
/// included, as absolute paths.
fn changed(ctx: &Context<'_>, base: Option<&str>) -> Result<Vec<PathBuf>, Report> {
    let top = output_of(&git(ctx).args(["rev-parse", "--show-toplevel"])).map_err(|e| {
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
    let top = PathBuf::from(top);
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

fn lint(args: &LintArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let scope = scope(global, ctx)?;
    let mut paths: Vec<PathBuf> = if args.paths.is_empty() {
        scope.own.clone()
    } else {
        args.paths.iter().map(|path| ctx.dir.join(path)).collect()
    };
    let mut report = Report::default();
    if args.only_changed {
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
    let fixes = match (args.fix, args.unsafe_fixes) {
        (false, _) => Fixes::None,
        (true, false) => Fixes::Safe,
        (true, true) => Fixes::Unsafe,
    };
    let outcome = odx_lint::lint(&Request {
        root: scope.root,
        paths,
        target: scope.target,
        addons_path: scope.addons_path,
        select: args.select.clone(),
        ignore: args.ignore.clone(),
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
    let violations: Vec<odx_lint::Violation> = outcome
        .violations
        .into_iter()
        .map(|violation| odx_lint::Violation {
            file_path: shown(&ctx.dir, &violation.file_path),
            ..violation
        })
        .collect();
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
        return Ok(report);
    }
    report.out = odx_lint::render(args.output_format, &violations);
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
    Ok(report)
}
