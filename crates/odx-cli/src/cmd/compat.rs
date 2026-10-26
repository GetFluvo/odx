//! `odx import` and `odx export`.

use std::fmt::Write as _;
use std::path::Path;

use odx_config::compat::{export, import, parse_addons_with_warnings, parse_repos};
use odx_config::{ConfigError, Source, discover};
use odx_core::{Exit, FileSystem};

use super::Report;
use crate::Context;
use crate::cli::{ExportArgs, ImportArgs};

fn read_text(fs: &dyn FileSystem, path: &Path) -> Result<String, Report> {
    fs.read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", path.display())))
}

/// `odx import [REPOS] [ADDONS]`: print `[tool.odx]` tables for a repos.yaml.
pub(crate) fn run_import(args: &ImportArgs, ctx: &Context<'_>) -> Report {
    import_report(args, ctx).unwrap_or_else(|report| report)
}

fn import_report(args: &ImportArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let repos_path = ctx.dir.join(&args.repos);
    if !ctx.fs.exists(&repos_path) {
        return Err(Report::error(
            Exit::Usage,
            format!(
                "{}: no such file\nhint: pass the git-aggregator file: odx import path/to/repos.yaml",
                repos_path.display()
            ),
        ));
    }
    let repos = parse_repos(&read_text(ctx.fs, &repos_path)?)
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", repos_path.display())))?;
    let mut report = Report::default();
    let addons_path = args.addons.as_ref().map_or_else(
        || repos_path.with_file_name("addons.yaml"),
        |path| ctx.dir.join(path),
    );
    let addons = if args.addons.is_some() || ctx.fs.exists(&addons_path) {
        let (addons, warnings) = parse_addons_with_warnings(&read_text(ctx.fs, &addons_path)?)
            .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", addons_path.display())))?;
        report.warnings.extend(warnings);
        Some(addons)
    } else {
        None
    };
    let result = import(&repos, addons.as_ref());
    report.warnings.extend(result.warnings.iter().cloned());
    if args.write {
        let written = super::edit::write_repositories(
            ctx,
            &result.repositories,
            args.odoo_version.as_deref(),
        )?;
        report.warnings.extend(written.warnings);
        report.out = written.out;
    } else {
        report.out = result.to_toml();
    }
    Ok(report)
}

/// `odx export [REPOS] [ADDONS]`: write git-aggregator and doodba files.
pub(crate) fn run_export(args: &ExportArgs, ctx: &Context<'_>) -> Report {
    export_report(args, ctx).unwrap_or_else(|report| report)
}

fn export_report(args: &ExportArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let project = match discover(ctx.fs, &ctx.dir, ctx.config) {
        Ok(Source::Pyproject(project)) => project,
        Ok(Source::ReposYaml { file, .. }) => {
            return Err(Report::error(
                Exit::Usage,
                format!(
                    "{}: export needs a pyproject.toml with [tool.odx]",
                    file.display()
                ),
            ));
        }
        Err(e @ ConfigError::Io { .. }) => return Err(Report::error(Exit::Failure, e.to_string())),
        Err(e) => return Err(Report::error(Exit::Usage, e.to_string())),
    };
    let files = export(&project.config);
    let mut report = Report {
        warnings: project.warnings,
        ..Report::default()
    };
    report.warnings.extend(files.warnings);
    if args.stdout {
        report.out = files.repos_yaml;
        if let Some(addons) = &files.addons_yaml {
            report.out.push_str("---\n");
            report.out.push_str(addons);
        }
        return Ok(report);
    }
    let mut written = vec![(project.root.join(&args.repos), files.repos_yaml)];
    if let Some(addons) = files.addons_yaml {
        written.push((project.root.join(&args.addons), addons));
    }
    for (path, text) in written {
        if let Err(e) = ctx.fs.write(&path, text.as_bytes()) {
            report.exit = Exit::Failure;
            report.error = Some(format!("{}: {e}", path.display()));
            return Ok(report);
        }
        let _ = writeln!(report.out, "wrote {}", path.display());
    }
    Ok(report)
}
