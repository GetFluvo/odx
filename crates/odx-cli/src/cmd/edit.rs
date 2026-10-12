//! `odx add`, `odx drop` and the writing half of `odx import --write`.

use std::path::{Path, PathBuf};

use odx_config::edit::{Added, PyprojectEdit, Target, parse_spec};
use odx_config::schema::Repository;
use odx_config::{ConfigError, Source, discover, find_pyproject, parse_pyproject};
use odx_core::Exit;

use super::Report;
use crate::Context;
use crate::cli::{AddArgs, DropArgs};

/// The `pyproject.toml` to edit: `--config`, the project found upwards, or
/// `pyproject.toml` in the working directory.
fn locate(ctx: &Context<'_>) -> Result<PathBuf, Report> {
    if let Some(config) = ctx.config {
        return Ok(ctx.dir.join(config));
    }
    match find_pyproject(ctx.fs, &ctx.dir) {
        Ok(project) => Ok(project.file),
        Err(ConfigError::NotFound { .. }) => {
            let file = ctx.dir.join("pyproject.toml");
            if ctx.fs.exists(&file) {
                Ok(file)
            } else {
                Err(Report::error(
                    Exit::Usage,
                    format!("{}: no such file", file.display()),
                ))
            }
        }
        Err(e @ ConfigError::Io { .. }) => Err(Report::error(Exit::Failure, e.to_string())),
        Err(e) => Err(Report::error(Exit::Usage, e.to_string())),
    }
}

fn open(ctx: &Context<'_>, file: &Path) -> Result<PyprojectEdit, Report> {
    let bytes = ctx
        .fs
        .read(file)
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
    PyprojectEdit::parse(&String::from_utf8_lossy(&bytes))
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", file.display())))
}

/// Validate the edited text and write it; the file is left untouched when
/// the result would be invalid.
fn save(ctx: &Context<'_>, file: &Path, doc: &PyprojectEdit) -> Result<(), Report> {
    let text = doc.to_string();
    parse_pyproject(&text, file).map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    ctx.fs
        .write(file, text.as_bytes())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))
}

/// Write imported repositories into the project's `pyproject.toml`.
pub(crate) fn write_repositories(
    ctx: &Context<'_>,
    repos: &[Repository],
    odoo_version: Option<&str>,
) -> Result<Report, Report> {
    let file = locate(ctx)?;
    let mut doc = open(ctx, &file)?;
    let series = odoo_version.or_else(|| repos.first().and_then(|r| r.base.as_deref()));
    let skipped = doc
        .add_repositories(repos, series)
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", file.display())))?;
    save(ctx, &file, &doc)?;
    let mut report = Report::default();
    for url in &skipped {
        report.warnings.push(odx_config::Problem {
            key: url.clone(),
            message: "already configured; skipped".to_owned(),
        });
    }
    report.out = format!(
        "wrote {} repositories to {}\n",
        repos.len() - skipped.len(),
        file.display()
    );
    Ok(report)
}

/// `odx add SPEC [--package DIST ...]`.
pub(crate) fn run_add(args: &AddArgs, ctx: &Context<'_>) -> Report {
    add(args, ctx).unwrap_or_else(|report| report)
}

fn add(args: &AddArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let spec = parse_spec(&args.spec).map_err(|e| Report::error(Exit::Usage, e))?;
    let file = locate(ctx)?;
    let mut doc = open(ctx, &file)?;
    let series = args.odoo_version.as_deref();
    let result = match &args.package {
        Some(dist) => doc.add_package(
            dist,
            &spec,
            args.subdirectory.as_deref(),
            args.group.as_deref(),
            series,
        ),
        None => doc.add_merge(&spec, series),
    };
    let added =
        result.map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", file.display())))?;
    let what = args.package.as_ref().map_or_else(
        || format!("{} {}", describe(&spec.target), spec.url),
        |dist| format!("package {dist}"),
    );
    let message = match added {
        Added::AlreadyPresent => format!("{what} is already configured\n"),
        Added::Merge | Added::Repository | Added::PackageEntry | Added::DirectReference => {
            save(ctx, &file, &doc)?;
            format!("added {what} to {}\n", file.display())
        }
    };
    Ok(Report {
        out: message,
        ..Report::default()
    })
}

fn describe(target: &Target) -> String {
    match target {
        Target::Pr(pr) => format!("pull request #{pr} of"),
        Target::Sha(sha) => format!("commit {sha} of"),
        Target::Branch(branch) => format!("branch {branch} of"),
    }
}

/// `odx drop SPEC [--package] [--reason TEXT]`.
pub(crate) fn run_drop(args: &DropArgs, ctx: &Context<'_>) -> Report {
    drop(args, ctx).unwrap_or_else(|report| report)
}

fn drop(args: &DropArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let file = locate(ctx)?;
    let mut doc = open(ctx, &file)?;
    let what = if args.package {
        let groups = group_names(ctx, &file);
        let groups: Vec<&str> = groups.iter().map(String::as_str).collect();
        doc.drop_package(&args.spec, &groups)
            .map_err(|e| Report::error(Exit::Failure, e.to_string()))?;
        format!("package {}", args.spec)
    } else {
        let spec = parse_spec(&args.spec).map_err(|e| Report::error(Exit::Usage, e))?;
        doc.drop_merge(&spec)
            .map_err(|e| Report::error(Exit::Failure, e.to_string()))?;
        format!("{} {}", describe(&spec.target), spec.url)
    };
    save(ctx, &file, &doc)?;
    let reason = args
        .reason
        .as_ref()
        .map(|reason| format!(" (reason: {reason})"))
        .unwrap_or_default();
    Ok(Report {
        out: format!("dropped {what} from {}{reason}\n", file.display()),
        ..Report::default()
    })
}

/// The dependency group names configured in `[tool.odx.groups]`.
fn group_names(ctx: &Context<'_>, file: &Path) -> Vec<String> {
    match discover(ctx.fs, &ctx.dir, Some(file)) {
        Ok(Source::Pyproject(project)) => project
            .config
            .groups
            .map(|groups| groups.dirs.into_keys().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
