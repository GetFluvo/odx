//! `odx dev`: the development workspace (addons path, `odoo.conf`,
//! editable installs) and package develop mode.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_analyze::stats::addon_dirs;
use odx_config::deps::normalize_name;
use odx_config::edit::PyprojectEdit;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_dev::{DevelopState, enter, leave, set_addons_path};

use super::aggregate::{Prepared, prepare, uv_program};
use super::edit::save;
use super::{Report, run_checked};
use crate::Context;
use crate::cli::{DevArgs, GlobalArgs};

const STATE: &str = ".odx/develop.toml";

/// `odx dev`.
pub(crate) fn run_dev(args: &DevArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    dev(args, global, ctx).unwrap_or_else(|report| report)
}

fn dev(args: &DevArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    match (&args.package, args.restore) {
        (_, true) => restore(ctx, &prepared, args.package.as_deref()),
        (Some(package), false) => develop(ctx, &prepared, package, args.branch.as_deref()),
        (None, false) => workspace(ctx, &prepared, args),
    }
}

fn failed(message: impl std::fmt::Display) -> Report {
    Report::error(Exit::Failure, message.to_string())
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

fn read_state(ctx: &Context<'_>, root: &Path) -> Result<DevelopState, Report> {
    let path = root.join(STATE);
    ctx.fs.read(&path).map_or_else(
        |_| Ok(DevelopState::default()),
        |bytes| {
            DevelopState::parse(&String::from_utf8_lossy(&bytes))
                .map_err(|e| Report::error(Exit::Usage, format!("{STATE}: {e}")))
        },
    )
}

fn write_state(root: &Path, state: &DevelopState) -> Result<(), Report> {
    let path = root.join(STATE);
    if state.packages.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, state.render()))
        .map_err(|e| failed(format!("{STATE}: {e}")))
}

/// The directories that hold addons, in the order Odoo should search them:
/// the project's own, packages in develop mode, aggregated repositories,
/// then the dependency groups.
fn addons_path(prepared: &Prepared, state: &DevelopState) -> Vec<PathBuf> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut dirs = Vec::new();
    for dir in config.project_addon_dirs() {
        let dir = root.join(dir);
        let addons = addon_dirs(&dir);
        if addons.first() == Some(&dir) {
            dirs.extend(dir.parent().map(Path::to_path_buf));
        } else if !addons.is_empty() {
            dirs.push(dir);
        }
    }
    dirs.extend(
        state
            .packages
            .values()
            .filter_map(|saved| root.join(&saved.path).parent().map(Path::to_path_buf)),
    );
    // The configuration was validated when it was loaded.
    let planned = plan_config(config, root, None).unwrap_or_default();
    dirs.extend(planned.into_iter().map(|repo| repo.target_dir));
    dirs.extend(config.group_dirs().into_values().map(|dir| root.join(dir)));
    let mut seen = Vec::new();
    for dir in dirs.into_iter().filter(|dir| dir.is_dir()) {
        if !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen
}

/// Workspace mode: addons path file, `odoo.conf`, editable installs.
fn workspace(ctx: &Context<'_>, prepared: &Prepared, args: &DevArgs) -> Result<Report, Report> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let dev = config.dev.clone().unwrap_or_default();
    let state = read_state(ctx, root)?;
    let path = addons_path(prepared, &state)
        .iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(",");
    let mut report = Report::default();
    if args.print_addons_path {
        report.out = format!("{path}\n");
        return Ok(report);
    }
    let verb = if args.dry_run { "would write" } else { "wrote" };
    let _ = writeln!(report.out, "addons path: {path}");
    let file = root.join(
        dev.addons_path_file
            .unwrap_or_else(|| PathBuf::from(".odx/addons-path")),
    );
    if !args.dry_run {
        file.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file, format!("{path}\n")))
            .map_err(|e| failed(format!("{}: {e}", file.display())))?;
    }
    let _ = writeln!(report.out, "{verb} {}", relative(&file, root));
    if let Some(conf) = dev.odoo_conf {
        let conf = root.join(conf);
        let text = std::fs::read_to_string(&conf).unwrap_or_default();
        if !args.dry_run {
            std::fs::write(&conf, set_addons_path(&text, &path))
                .map_err(|e| failed(format!("{}: {e}", conf.display())))?;
        }
        let _ = writeln!(
            report.out,
            "{verb} addons_path in {}",
            relative(&conf, root)
        );
    }
    if args.no_editable || dev.editable == Some(false) {
        return Ok(report);
    }
    let editable = editable_addons(prepared);
    if editable.is_empty() {
        return Ok(report);
    }
    let venv = root.join(
        args.venv
            .clone()
            .or(dev.venv)
            .unwrap_or_else(|| PathBuf::from(".venv")),
    );
    if !venv.is_dir() {
        report.warnings.push(odx_config::Problem {
            key: relative(&venv, root),
            message: "no virtual environment; editable installs skipped (run `uv venv`)".to_owned(),
        });
        return Ok(report);
    }
    if !args.dry_run {
        let uv = uv_program(ctx.env);
        let invocation = editable.iter().fold(
            Invocation::new(&uv)
                .args(["pip", "install", "--python"])
                .arg(venv.as_os_str())
                .current_dir(root),
            |invocation, dir| invocation.arg("-e").arg(dir.as_os_str()),
        );
        run_checked(&invocation, &format!("{uv} pip install")).map_err(failed)?;
    }
    let _ = writeln!(
        report.out,
        "{} {} addons in editable mode into {}",
        if args.dry_run {
            "would install"
        } else {
            "installed"
        },
        editable.len(),
        relative(&venv, root)
    );
    Ok(report)
}

/// The addons with their own `pyproject.toml` (PEP 660 editable installs):
/// the project's and the selected ones of every aggregated repository.
fn editable_addons(prepared: &Prepared) -> Vec<PathBuf> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut addons: Vec<PathBuf> = config
        .project_addon_dirs()
        .iter()
        .flat_map(|dir| addon_dirs(&root.join(dir)))
        .collect();
    let planned = plan_config(config, root, None).unwrap_or_default();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let patterns = configured.addons.clone().unwrap_or_default();
        addons.extend(addon_dirs(&repo.target_dir).into_iter().filter(|addon| {
            let name = addon
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| git_aggregate_rs::fnmatch(&name, pattern))
        }));
    }
    addons.retain(|addon| addon.join("pyproject.toml").is_file());
    addons.sort();
    addons.dedup();
    addons
}

/// Whether `git args` succeeds in `dir`.
fn git(dir: &Path, args: &[&str]) -> bool {
    OsRunner
        .run(
            &Invocation::new("git")
                .args(args.iter().copied())
                .current_dir(dir),
        )
        .is_ok_and(|out| out.success())
}

/// Package mode: clone the locked commit into the develop directory and
/// point the package's source at it.
fn develop(
    ctx: &Context<'_>,
    prepared: &Prepared,
    package: &str,
    branch: Option<&str>,
) -> Result<Report, Report> {
    let root = &prepared.project.root;
    let config = &prepared.project.config;
    let mut state = read_state(ctx, root)?;
    let dist = normalize_name(package);
    let lock = prepared.lock.clone().unwrap_or_default();
    let Some(locked) = lock
        .packages
        .iter()
        .find(|(name, _)| normalize_name(name) == dist)
        .map(|(_, entry)| entry.clone())
    else {
        return Err(failed(format!(
            "{package} is not a locked package; add it to a group or [tool.odx.packages] and run `odx lock`"
        )));
    };
    let dev_dir = config
        .dev
        .as_ref()
        .and_then(|dev| dev.dev_dir.clone())
        .unwrap_or_else(|| PathBuf::from(".src"));
    let clone = root.join(&dev_dir).join(&dist);
    let mut report = Report::default();
    if !clone.join(".git").exists() {
        let cache = prepared.cache.repo_dir(&locked.url);
        let parent = clone.parent().unwrap_or(root);
        std::fs::create_dir_all(parent)
            .map_err(|e| failed(format!("{}: {e}", parent.display())))?;
        let cloned = git(
            parent,
            &["clone", "--quiet", &cache.display().to_string(), &dist],
        ) && git(&clone, &["remote", "set-url", "origin", &locked.url])
            && git(&clone, &["remote", "rename", "origin", "upstream"]);
        if !cloned {
            return Err(failed(format!(
                "{dist}: cannot clone {} from the cache; run `odx lock`",
                locked.url
            )));
        }
        if let Some(mirror) = config.mirror.as_ref() {
            git(&clone, &["remote", "add", "mirror", &mirror.url]);
        }
        let _ = writeln!(report.out, "cloned {dist} into {}", relative(&clone, root));
    }
    let switched = match branch {
        Some(branch)
            if git(
                &clone,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            ) =>
        {
            git(&clone, &["switch", "--quiet", branch])
        }
        Some(branch) => git(&clone, &["switch", "--quiet", "-c", branch, &locked.sha]),
        None if state.packages.contains_key(&dist) => true,
        None => git(&clone, &["switch", "--quiet", "--detach", &locked.sha]),
    };
    if !switched {
        return Err(failed(format!(
            "{dist}: cannot switch {} to {}",
            relative(&clone, root),
            branch.unwrap_or(&locked.sha)
        )));
    }
    if let Some(branch) = branch {
        let _ = writeln!(report.out, "{dist} is on branch {branch}");
    }
    if let Some(saved) = state.packages.get(&dist) {
        let _ = writeln!(report.out, "{dist} already uses {} (editable)", saved.path);
        return Ok(report);
    }
    let path = format!(
        "{}/{dist}{}",
        dev_dir.display().to_string().replace('\\', "/"),
        locked
            .subdirectory
            .as_ref()
            .map(|dir| format!("/{dir}"))
            .unwrap_or_default()
    );
    let mut edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let saved = enter(edit.document_mut(), &dist, &path);
    // The state first: a pyproject.toml that changed must be restorable.
    state.packages.insert(dist.clone(), saved);
    write_state(root, &state)?;
    save(ctx, &prepared.project.file, &edit)?;
    let _ = writeln!(
        report.out,
        "{dist} now uses {path} (editable); run `uv sync`, and `odx dev --restore {dist}` when done"
    );
    Ok(report)
}

/// `odx dev --restore [<package>]`: put the locked sources back.
fn restore(
    ctx: &Context<'_>,
    prepared: &Prepared,
    package: Option<&str>,
) -> Result<Report, Report> {
    let root = &prepared.project.root;
    let mut state = read_state(ctx, root)?;
    let wanted: Vec<String> = match package {
        Some(package) => {
            let dist = normalize_name(package);
            if !state.packages.contains_key(&dist) {
                return Err(failed(format!("{package} is not in develop mode")));
            }
            vec![dist]
        }
        None => state.packages.keys().cloned().collect(),
    };
    let mut report = Report::default();
    if wanted.is_empty() {
        report.out.push_str("no package is in develop mode\n");
        return Ok(report);
    }
    let mut edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let removed: Vec<(String, odx_dev::Saved)> = wanted
        .iter()
        .filter_map(|dist| state.packages.remove_entry(dist))
        .collect();
    for (dist, saved) in &removed {
        leave(edit.document_mut(), dist, saved);
        let _ = writeln!(
            report.out,
            "restored {dist}; its clone stays in {}",
            saved.path
        );
    }
    save(ctx, &prepared.project.file, &edit)?;
    write_state(root, &state)?;
    report
        .out
        .push_str("run `uv sync` to install the locked versions again\n");
    Ok(report)
}
