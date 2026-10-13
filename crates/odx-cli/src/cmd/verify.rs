//! `odx verify`: whether a checkout is exactly what the lockfiles describe,
//! without network access.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use odx_aggregate::{locked_entry, locked_package, plan_config};
use odx_config::deps::normalize_name;
use odx_config::edit::PyprojectEdit;
use odx_config::lock::Lockfile;
use odx_config::packages::package_specs;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_git::Merger;
use toml_edit::{DocumentMut, Item};

use super::Report;
use super::aggregate::{Prepared, prepare};
use crate::Context;
use crate::cli::{GlobalArgs, VerifyArgs};

/// `odx verify`.
pub(crate) fn run_verify(args: &VerifyArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    verify(args, global, ctx).unwrap_or_else(|report| report)
}

fn verify(args: &VerifyArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    if args.vendored || args.installed || args.requirements {
        return Err(Report::error(
            Exit::Usage,
            "odx verify --vendored/--installed/--requirements: not implemented yet (roadmap phase 5)"
                .to_owned(),
        ));
    }
    let prepared = prepare(global, ctx)?;
    let lock = prepared.lock.clone().unwrap_or_default();
    let mut problems = Vec::new();
    let checked = repositories(&prepared, &lock, &mut problems);
    let edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    packages(ctx, &prepared, edit.document(), &lock, &mut problems);
    stray_addons(&prepared, &mut problems);
    if prepared.lock.is_none() && !problems.is_empty() {
        problems.insert(
            0,
            format!(
                "{} is missing; run `odx lock`",
                prepared.lock_path.display()
            ),
        );
    }
    let mut report = if global.json {
        Report {
            out: format!(
                "{}\n",
                serde_json::json!({ "schema": 1, "ok": problems.is_empty(), "problems": problems })
            ),
            ..Report::default()
        }
    } else if problems.is_empty() {
        Report {
            out: format!(
                "verified: odx.lock matches the configuration and {checked} working trees\n"
            ),
            ..Report::default()
        }
    } else {
        Report {
            error: Some(format!("verification failed:\n  {}", problems.join("\n  "))),
            ..Report::default()
        }
    };
    if !problems.is_empty() {
        report.exit = Exit::Failure;
    }
    Ok(report)
}

/// Every configured repository is locked as configured, nothing else is
/// locked, and every working tree is the locked result; returns how many
/// working trees were checked.
fn repositories(prepared: &Prepared, lock: &Lockfile, problems: &mut Vec<String>) -> usize {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    // The configuration was validated when it was loaded.
    let planned = plan_config(config, root, None).unwrap_or_default();
    let merger = Merger::new(Arc::new(OsRunner));
    let mut checked = 0;
    for repo in &planned {
        let dir = repo
            .target_dir
            .strip_prefix(root)
            .unwrap_or(&repo.target_dir)
            .display()
            .to_string();
        let Some(entry) = locked_entry(repo, &lock.repositories) else {
            problems.push(format!(
                "{}: not locked as configured; run `odx lock`",
                repo.name
            ));
            continue;
        };
        checked += 1;
        if !repo.target_dir.exists() {
            problems.push(format!(
                "{}: {dir} is missing; run `odx aggregate --locked`",
                repo.name
            ));
        } else if !merger.is_current(
            &repo.target_dir,
            &prepared.cache.repo_dir(&repo.url),
            &entry.result_tree,
        ) {
            problems.push(format!(
                "{}: {dir} is not the locked result (local changes, or built from other commits); run `odx aggregate --locked`",
                repo.name
            ));
        }
    }
    let names: BTreeSet<&str> = planned.iter().map(|repo| repo.name.as_str()).collect();
    problems.extend(
        lock.repositories
            .iter()
            .filter(|entry| !names.contains(entry.name.as_str()))
            .map(|entry| {
                format!(
                    "{}: locked but no longer configured; run `odx lock`",
                    entry.name
                )
            }),
    );
    checked
}

/// Every git package is locked as configured, nothing else is locked,
/// `uv.lock` has the locked commits, and no package is in develop mode.
fn packages(
    ctx: &Context<'_>,
    prepared: &Prepared,
    doc: &DocumentMut,
    lock: &Lockfile,
    problems: &mut Vec<String>,
) {
    let (specs, _) = package_specs(doc, &prepared.project.config, &lock.packages);
    for spec in specs
        .iter()
        .filter(|spec| locked_package(spec, &lock.packages).is_none())
    {
        problems.push(format!(
            "{}: package not locked as configured; run `odx lock`",
            spec.dist
        ));
    }
    let names: BTreeSet<String> = specs
        .iter()
        .map(|spec| normalize_name(&spec.dist))
        .collect();
    problems.extend(
        lock.packages
            .keys()
            .filter(|dist| !names.contains(&normalize_name(dist)))
            .map(|dist| format!("{dist}: package locked but no longer configured; run `odx lock`")),
    );
    problems.extend(
        editable_sources(doc)
            .into_iter()
            .filter(|dist| names.contains(&normalize_name(dist)))
            .map(|dist| format!("{dist}: in develop mode (editable path source); restore it before locking or deploying")),
    );
    if lock.packages.is_empty() {
        return;
    }
    let uv_lock = prepared.project.root.join("uv.lock");
    let Ok(text) = ctx
        .fs
        .read(&uv_lock)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    else {
        problems.push(format!("{} is missing; run `odx lock`", uv_lock.display()));
        return;
    };
    let resolved = uv_git_commits(&text);
    for (dist, entry) in &lock.packages {
        let found = resolved
            .iter()
            .find(|(name, _)| normalize_name(name) == normalize_name(dist))
            .map(|(_, commit)| commit.as_str());
        match found {
            Some(commit) if commit == entry.sha => {}
            Some(_) => problems.push(format!(
                "{dist}: uv.lock has another commit; run `odx lock`"
            )),
            None => problems.push(format!("{dist}: not in uv.lock from git; run `odx lock`")),
        }
    }
}

/// `[tool.uv.sources]` entries that point at an editable local path.
fn editable_sources(doc: &DocumentMut) -> Vec<String> {
    let sources = doc
        .get("tool")
        .and_then(|tool| tool.get("uv"))
        .and_then(|uv| uv.get("sources"))
        .and_then(Item::as_table_like);
    sources.map_or_else(Vec::new, |sources| {
        sources
            .iter()
            .filter(|(_, source)| {
                source.get("path").is_some()
                    && source.get("editable").and_then(Item::as_bool) == Some(true)
            })
            .map(|(name, _)| name.to_owned())
            .collect()
    })
}

/// The commit of every git package in `uv.lock` (`source = { git =
/// "<url>?…#<commit>" }`).
fn uv_git_commits(text: &str) -> Vec<(String, String)> {
    let doc: DocumentMut = text.parse().unwrap_or_default();
    doc.get("package")
        .and_then(Item::as_array_of_tables)
        .map_or_else(Vec::new, |packages| {
            packages
                .iter()
                .filter_map(|package| {
                    let name = package.get("name")?.as_str()?;
                    let git = package.get("source")?.get("git")?.as_str()?;
                    let (_, commit) = git.rsplit_once('#')?;
                    Some((name.to_owned(), commit.to_owned()))
                })
                .collect()
        })
}

/// No addon directory under `[tool.odx.groups] project_addons` is unknown
/// to git: the build would ship it although nobody committed it.
fn stray_addons(prepared: &Prepared, problems: &mut Vec<String>) {
    let root = &prepared.project.root;
    let dirs = prepared
        .project
        .config
        .groups
        .as_ref()
        .and_then(|groups| groups.project_addons.clone())
        .unwrap_or_default();
    if dirs.is_empty() {
        return;
    }
    let listing = OsRunner.run(
        &Invocation::new("git")
            .args(["ls-files", "-z"])
            .current_dir(root),
    );
    let Some(tracked) = listing.ok().filter(odx_core::Output::success).map(|out| {
        String::from_utf8_lossy(&out.stdout)
            .split('\0')
            .map(str::to_owned)
            .collect::<BTreeSet<String>>()
    }) else {
        problems.push(format!(
            "{}: not a git checkout; cannot check for uncommitted addons",
            root.display()
        ));
        return;
    };
    for dir in dirs {
        let entries = std::fs::read_dir(root.join(&dir))
            .map_or_else(|_| Vec::new(), |e| e.flatten().collect());
        let mut addons: Vec<String> = entries
            .iter()
            .map(std::fs::DirEntry::path)
            .filter(|path| path.join("__manifest__.py").is_file())
            .filter_map(|path| relative(&path, root))
            .filter(|path| !tracked.contains(&format!("{path}/__manifest__.py")))
            .collect();
        addons.sort();
        for addon in addons {
            problems.push(format!(
                "{addon}: addon directory not committed; commit or remove it"
            ));
        }
    }
}

fn relative(path: &Path, root: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|p| p.display().to_string().replace('\\', "/"))
}
