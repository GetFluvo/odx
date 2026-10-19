//! `odx env`: the project's environments, with their preset applied.

use std::fmt::Write as _;

use odx_config::environments::{Environment, Model};
use odx_core::{Exit, Invocation};

use super::aggregate::prepare;
use super::{Report, output_of};
use crate::Context;
use crate::cli::GlobalArgs;

/// `odx env list`.
pub(crate) fn run_list(global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    list(global, ctx).unwrap_or_else(|report| report)
}

/// The environment model, or why there is none.
pub(crate) fn model(config: &odx_config::OdxConfig) -> Result<Model, Report> {
    // The configuration was validated when it was loaded.
    Model::resolve(config).ok().flatten().ok_or_else(|| {
        Report::error(
            Exit::Usage,
            "no environments configured: add [tool.odx.environments] to pyproject.toml".to_owned(),
        )
    })
}

fn list(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let model = model(&prepared.project.config)?;
    let branch = output_of(
        &Invocation::new("git")
            .arg("-C")
            .arg(&prepared.project.root)
            .args(["branch", "--show-current"]),
    )
    .ok()
    .filter(|branch| !branch.is_empty());
    let current = branch
        .as_deref()
        .and_then(|branch| model.of_branch(branch, None))
        .map(|env| env.name.clone());
    let out = if global.json {
        json(&model, current.as_deref())
    } else {
        text(&model, branch.as_deref(), current.as_deref())
    };
    Ok(Report {
        out,
        ..Report::default()
    })
}

fn json(model: &Model, current: Option<&str>) -> String {
    let environments: Vec<serde_json::Value> = model
        .environments
        .values()
        .map(|env| {
            serde_json::json!({
                "name": env.name,
                "branch": env.branch,
                "role": env.role.map(odx_config::schema::Role::name),
                "series": env.series,
                "repo": env.repo,
                "disposable": env.disposable,
            })
        })
        .collect();
    let value = serde_json::json!({
        "schema": 1,
        "preset": model.preset.name(),
        "environments": environments,
        "promotion": model.promotion,
        "branch_specific": model.branch_specific,
        "current": current,
    });
    format!("{value:#}\n")
}

fn text(model: &Model, branch: Option<&str>, current: Option<&str>) -> String {
    let mut out = format!("preset {}\n", model.preset.name());
    let width =
        |pick: fn(&Environment) -> usize| model.environments.values().map(pick).max().unwrap_or(0);
    let name = width(|env| env.name.len());
    let role = width(|env| env.role.map_or(1, |role| role.name().len()));
    let series = width(|env| env.series.len());
    let branch_width = width(|env| env.branch.len());
    for env in model.environments.values() {
        let mark = if current == Some(env.name.as_str()) {
            '*'
        } else {
            ' '
        };
        let mut line = format!(
            "{mark} {:name$}  {:role$}  {:series$}  {:branch_width$}",
            env.name,
            env.role.map_or("-", |role| role.name()),
            env.series,
            env.branch,
        );
        if let Some(repo) = &env.repo {
            let _ = write!(line, "  {repo}");
        }
        if env.disposable {
            line.push_str("  (disposable)");
        }
        let _ = writeln!(out, "{}", line.trim_end());
    }
    let edges: Vec<String> = model
        .promotion
        .iter()
        .map(|[from, to]| format!("{from} → {to}"))
        .collect();
    let _ = writeln!(
        out,
        "promotion: {}",
        if edges.is_empty() {
            "none".to_owned()
        } else {
            edges.join(", ")
        }
    );
    if !model.branch_specific.is_empty() {
        let _ = writeln!(out, "branch-specific: {}", model.branch_specific.join(", "));
    }
    if let Some(branch) = branch
        && current.is_none()
    {
        let _ = writeln!(out, "the current branch {branch} is no environment's");
    }
    out
}
