//! `odx stats`: lines of code of the project's addons, per language or per
//! addon.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_analyze::stats::{Row, addon_dirs, count, rows};

use super::Report;
use super::aggregate::prepare;
use crate::Context;
use crate::cli::{GlobalArgs, StatsArgs};

/// `odx stats`.
pub(crate) fn run_stats(args: &StatsArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    let prepared = match prepare(global, ctx) {
        Ok(prepared) => prepared,
        Err(report) => return report,
    };
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut dirs = config.project_addon_dirs();
    if args.include_vendor {
        dirs.extend(config.group_dirs().into_values());
        // The configuration was validated when it was loaded.
        let planned = plan_config(config, root, None).unwrap_or_default();
        dirs.extend(planned.into_iter().map(|repo| repo.target_dir));
    }
    let mut addons: Vec<PathBuf> = dirs
        .iter()
        .flat_map(|dir| addon_dirs(&root.join(dir)))
        .collect();
    addons.sort();
    addons.dedup();
    let mut report = Report::default();
    if addons.is_empty() {
        report.warnings.push(odx_config::Problem {
            key: "stats".to_owned(),
            message: format!("no addon directories found under {}", root.display()),
        });
    }
    report.out = if args.per_addon {
        per_addon(&addons, root, global.json)
    } else {
        per_language(&addons, root, global.json)
    };
    report
}

fn per_language(addons: &[PathBuf], root: &Path, json: bool) -> String {
    let languages = count(addons, root);
    let rows = rows(&languages);
    let total = Row::sum("Total", &rows);
    if json {
        return format!(
            "{}\n",
            serde_json::json!({ "schema": 1, "languages": languages, "total": total })
        );
    }
    table("Language", &rows, &total)
}

fn per_addon(addons: &[PathBuf], root: &Path, json: bool) -> String {
    let counted: Vec<(String, _)> = addons
        .iter()
        .map(|addon| {
            let name = addon
                .strip_prefix(root)
                .unwrap_or(addon)
                .display()
                .to_string()
                .replace('\\', "/");
            (name, count(std::slice::from_ref(addon), root))
        })
        .collect();
    let rows: Vec<Row> = counted
        .iter()
        .map(|(name, languages)| Row::sum(name, &rows(languages)))
        .collect();
    let total = Row::sum("Total", &rows);
    if json {
        let addons: serde_json::Map<String, serde_json::Value> = counted
            .iter()
            .map(|(name, languages)| (name.clone(), serde_json::json!(languages)))
            .collect();
        return format!(
            "{}\n",
            serde_json::json!({ "schema": 1, "addons": addons, "total": total })
        );
    }
    table("Addon", &rows, &total)
}

fn table(heading: &str, rows: &[Row], total: &Row) -> String {
    let width = rows
        .iter()
        .map(|row| row.name.len())
        .chain([heading.len(), total.name.len()])
        .max()
        .unwrap_or_default();
    let mut text = format!(
        "{heading:<width$} {:>7} {:>9} {:>9} {:>9} {:>9}\n",
        "Files", "Lines", "Code", "Comments", "Blanks"
    );
    for row in rows.iter().chain([total]) {
        let _ = writeln!(
            text,
            "{:<width$} {:>7} {:>9} {:>9} {:>9} {:>9}",
            row.name, row.files, row.lines, row.code, row.comments, row.blanks
        );
    }
    text
}
