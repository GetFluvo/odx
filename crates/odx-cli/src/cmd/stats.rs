//! `odx stats`: lines of code of the project's addons, per language or per
//! addon.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_analyze::stats::{Row, addon_dirs, count, rows};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::prepare;
use crate::Context;
use crate::cli::{GlobalArgs, StatsArgs};

/// What `odx --json stats` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx stats")]
pub(crate) struct StatsJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// Per language, as tokei counts it.
    #[schemars(with = "BTreeMap<String, LanguageSchema>")]
    languages: serde_json::Value,
    /// The sum of every language.
    #[schemars(with = "RowSchema")]
    total: &'a Row,
}

/// What `odx --json stats --per-addon` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx stats --per-addon")]
pub(crate) struct StatsPerAddonJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// Per addon directory relative to the project root, per language.
    #[schemars(with = "BTreeMap<String, BTreeMap<String, LanguageSchema>>")]
    addons: BTreeMap<&'a str, serde_json::Value>,
    /// The sum of every addon.
    #[schemars(with = "RowSchema")]
    total: &'a Row,
}

/// The counts of tokei as JSON.
fn tokei_json(languages: &impl Serialize) -> serde_json::Value {
    // Maps with string keys of numbers and lists: serializing cannot fail.
    serde_json::to_value(languages).unwrap_or_default()
}

// The types below describe what tokei and `Row` serialize; they are never
// built.

/// Totals of one language, one addon or everything.
#[derive(JsonSchema)]
#[expect(
    dead_code,
    reason = "only describes the JSON of a type of another crate"
)]
struct RowSchema {
    /// Language or addon name, or `Total`.
    name: String,
    /// Files counted.
    files: usize,
    /// All lines.
    lines: usize,
    /// Lines of code.
    code: usize,
    /// Comment lines.
    comments: usize,
    /// Blank lines.
    blanks: usize,
}

/// One language, as tokei counts it.
#[derive(JsonSchema)]
#[expect(
    dead_code,
    reason = "only describes the JSON of a type of another crate"
)]
struct LanguageSchema {
    /// Blank lines.
    blanks: usize,
    /// Lines of code.
    code: usize,
    /// Comment lines.
    comments: usize,
    /// Per file.
    reports: Vec<ReportSchema>,
    /// Per language embedded in these files (such as code blocks in
    /// Markdown), per file.
    children: BTreeMap<String, Vec<ReportSchema>>,
    /// Whether some file could not be parsed.
    inaccurate: bool,
}

/// One file, as tokei counts it.
#[derive(JsonSchema)]
#[expect(
    dead_code,
    reason = "only describes the JSON of a type of another crate"
)]
struct ReportSchema {
    /// Its lines.
    stats: CodeStatsSchema,
    /// Its path, relative to the project root.
    name: String,
}

/// Lines of a file or of a blob of code in it.
#[derive(JsonSchema)]
#[expect(
    dead_code,
    reason = "only describes the JSON of a type of another crate"
)]
struct CodeStatsSchema {
    /// Blank lines.
    blanks: usize,
    /// Lines of code.
    code: usize,
    /// Comment lines.
    comments: usize,
    /// Code of other languages embedded in it, per language.
    blobs: BTreeMap<String, Self>,
}

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
        return super::to_json(&StatsJson {
            schema: 1,
            languages: tokei_json(&languages),
            total: &total,
        });
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
        return super::to_json(&StatsPerAddonJson {
            schema: 1,
            addons: counted
                .iter()
                .map(|(name, languages)| (name.as_str(), tokei_json(languages)))
                .collect(),
            total: &total,
        });
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
