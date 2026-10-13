//! `odx vendor`: the platform requirements file (`--requirements`).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use odx_aggregate::plan_config;
use odx_analyze::manifest::{parse_manifest, python_requirement};
use odx_analyze::stats::addon_dirs;
use odx_config::Problem;
use odx_config::deps::normalize_name;
use odx_config::schema::PlatformProfile;
use odx_config::uvlock::{UvLock, UvPackage, UvRequirement};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_platform::export::filter_export;
use odx_platform::provided::ProvidedSet;
use odx_platform::requirements::{
    Conflict, Declared, PlatformRequirements, build, edited, is_odoo, render, with_header,
};
use odx_platform::{profile, profile_name, provided_set, requirements_path};

use super::Report;
use super::aggregate::{Prepared, prepare, uv_program};
use crate::Context;
use crate::cli::{GlobalArgs, VendorArgs};

/// The requirements file odx would write, and what it found on the way.
pub(crate) struct Generated {
    /// The file text.
    pub(crate) text: String,
    /// Lines, omitted provided packages, conflicts, unresolved packages.
    pub(crate) requirements: PlatformRequirements,
    /// Manifests that could not be read.
    pub(crate) warnings: Vec<Problem>,
    /// What the platform provides.
    pub(crate) provided: ProvidedSet,
}

/// `odx vendor`.
pub(crate) fn run_vendor(args: &VendorArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    vendor(args, global, ctx).unwrap_or_else(|report| report)
}

fn vendor(args: &VendorArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let Some(target) = &args.requirements else {
        return Err(Report::error(
            Exit::Usage,
            "odx vendor: installing addon directories is not implemented yet (roadmap phase 5); \
             `odx vendor --requirements` writes the platform requirements file"
                .to_owned(),
        ));
    };
    let prepared = prepare(global, ctx)?;
    let root = &prepared.project.root;
    let file = root.join(
        target
            .clone()
            .unwrap_or_else(|| requirements_path(&prepared.project.config)),
    );
    let generated = generate(ctx, &prepared)?;
    let mut report = Report {
        warnings: generated.warnings.clone(),
        ..Report::default()
    };
    let previous = ctx
        .fs
        .read(&file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    if previous
        .as_deref()
        .is_some_and(|text| text != generated.text && edited(text))
    {
        report.warnings.push(Problem {
            key: relative(&file, root),
            message: "edited by hand; the edits are overwritten".to_owned(),
        });
    }
    if previous.as_deref() != Some(generated.text.as_str()) {
        file.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
        ctx.fs
            .write(&file, generated.text.as_bytes())
            .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
    }
    let found = &generated.requirements;
    report.warnings.extend(problems(found));
    if global.json {
        report.out = format!(
            "{}\n",
            serde_json::json!({
                "schema": 1,
                "file": relative(&file, root),
                "lines": found.lines,
                "omitted": found.omitted,
                "conflicts": found.conflicts,
                "unresolved": found.unresolved,
            })
        );
    } else {
        report.out = format!(
            "wrote {}: {} requirements, {} provided by the platform left out\n",
            relative(&file, root),
            found.lines.len(),
            found.omitted.len()
        );
    }
    if args.strict && !found.conflicts.is_empty() {
        report.exit = Exit::Failure;
        report.error = Some(format!(
            "{} requirements conflict with the platform (--strict)",
            found.conflicts.len()
        ));
    }
    Ok(report)
}

/// Warnings for conflicts and packages `uv.lock` does not pin.
pub(crate) fn problems(found: &PlatformRequirements) -> Vec<Problem> {
    found
        .conflicts
        .iter()
        .map(|conflict| Problem {
            key: conflict.package.clone(),
            message: describe_conflict(conflict),
        })
        .chain(found.unresolved.iter().map(|package| Problem {
            key: package.clone(),
            message: "no single version in uv.lock; written unpinned (run `uv lock`)".to_owned(),
        }))
        .collect()
}

/// `pos_x requires >=2.11, the platform provides 2.10.3`.
pub(crate) fn describe_conflict(conflict: &Conflict) -> String {
    format!(
        "{} requires {}, the platform provides {}",
        conflict.by,
        conflict.specifier,
        conflict.provided.join(" or ")
    )
}

/// The requirements file for the project's platform profile:
/// `generic-wheels` exports `uv.lock` (with hashes) without the provided
/// packages; the other profiles list the external Python packages the
/// shipped addons need, pinned to `uv.lock`.
pub(crate) fn generate(ctx: &Context<'_>, prepared: &Prepared) -> Result<Generated, Report> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let provided = provided_set(config, root, |path| {
        ctx.fs
            .read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    })
    .map_err(|e| Report::error(Exit::Usage, e))?;
    let uv_lock_path = root.join("uv.lock");
    let uv_lock = match ctx.fs.read(&uv_lock_path) {
        Ok(bytes) => Some(
            UvLock::parse(&String::from_utf8_lossy(&bytes)).map_err(|e| {
                Report::error(Exit::Usage, format!("{}: {e}", uv_lock_path.display()))
            })?,
        ),
        Err(_) => None,
    };
    let mut warnings = Vec::new();
    let declared = declared(prepared, uv_lock.as_ref(), &mut warnings);
    let mut requirements = build(
        &declared,
        &provided,
        &uv_lock.as_ref().map(UvLock::versions).unwrap_or_default(),
    );
    let profile = profile(config);
    let header = format!(
        "profile {}, provided set {}",
        profile_name(profile),
        provided.label
    );
    let text = if profile == PlatformProfile::GenericWheels {
        let Some(lock) = &uv_lock else {
            return Err(Report::error(
                Exit::Failure,
                format!(
                    "{} is missing; the generic-wheels profile exports it",
                    uv_lock_path.display()
                ),
            ));
        };
        let export = uv_export(ctx, root, lock, &config.group_dirs().into_keys().collect())?;
        let (body, omitted) = filter_export(&export, &provided);
        requirements.omitted = omitted;
        requirements.unresolved.clear();
        requirements.lines.clear();
        with_header(&body, &header)
    } else {
        render(&requirements, &header)
    };
    Ok(Generated {
        text,
        requirements,
        warnings,
        provided,
    })
}

/// `uv export --frozen --no-dev --no-emit-project` with the project's
/// shipped extras and dependency groups (`ODX_UV` names the executable).
fn uv_export(
    ctx: &Context<'_>,
    root: &Path,
    lock: &UvLock,
    groups: &BTreeSet<String>,
) -> Result<String, Report> {
    let uv = uv_program(ctx.env);
    let mut invocation = Invocation::new(&uv)
        .args(["export", "--frozen", "--no-dev", "--no-emit-project"])
        .current_dir(root);
    let none = UvPackage::default();
    let project = lock.root().unwrap_or(&none);
    for group in groups {
        if project.optional_dependencies.contains_key(group) {
            invocation = invocation.arg("--extra").arg(group);
        } else if project.dev_dependencies.contains_key(group) {
            invocation = invocation.arg("--group").arg(group);
        }
    }
    let output = OsRunner.run(&invocation);
    match output {
        Ok(out) if out.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => Err(Report::error(
            Exit::Failure,
            format!(
                "{uv} export failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )),
        Err(e) => Err(Report::error(
            Exit::Failure,
            format!("{uv} export failed: {e}"),
        )),
    }
}

/// Every Python requirement of what the project ships: the non-addon
/// requirements of `[project]` and the shipped groups, the dependencies of
/// every addon package they reach in `uv.lock`, and the
/// `external_dependencies` of the project's and the aggregated addons.
fn declared(
    prepared: &Prepared,
    uv_lock: Option<&UvLock>,
    warnings: &mut Vec<Problem>,
) -> Vec<Declared> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let groups: BTreeSet<String> = config.group_dirs().into_keys().collect();
    let mut declared = Vec::new();
    if let Some((lock, project)) = uv_lock.and_then(|lock| lock.root().map(|root| (lock, root))) {
        let shipped = |requirement: &&UvRequirement| {
            requirement
                .extra()
                .is_none_or(|extra| groups.contains(extra))
        };
        let direct = project
            .metadata
            .requires_dist
            .iter()
            .filter(shipped)
            .map(|requirement| {
                let by = requirement.extra().map_or_else(
                    || "pyproject.toml".to_owned(),
                    |extra| format!("group {extra}"),
                );
                (requirement, by)
            })
            .chain(
                project
                    .metadata
                    .requires_dev
                    .iter()
                    .filter(|(group, _)| groups.contains(*group))
                    .flat_map(|(group, requirements)| {
                        requirements
                            .iter()
                            .map(move |requirement| (requirement, format!("group {group}")))
                    }),
            );
        let mut addons = Vec::new();
        for (requirement, by) in direct {
            if is_odoo(&requirement.name) {
                addons.push(requirement.name.clone());
            } else {
                declared.push(Declared {
                    name: requirement.name.clone(),
                    specifier: requirement.specifier.clone().unwrap_or_default(),
                    by,
                });
            }
        }
        let mut seen = BTreeSet::new();
        while let Some(addon) = addons.pop() {
            if !seen.insert(normalize_name(&addon)) {
                continue;
            }
            for package in lock.packages_named(&addon) {
                for dependency in &package.dependencies {
                    if is_odoo(&dependency.name) {
                        addons.push(dependency.name.clone());
                    } else {
                        declared.push(Declared {
                            name: dependency.name.clone(),
                            specifier: String::new(),
                            by: package.name.clone(),
                        });
                    }
                }
            }
        }
    }
    for addon in shipped_addon_dirs(prepared) {
        let manifest = addon.join("__manifest__.py");
        let text = std::fs::read_to_string(&manifest).unwrap_or_default();
        let name = addon
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match parse_manifest(&text) {
            Ok(parsed) if parsed.installable => {
                declared.extend(parsed.python.iter().map(|entry| {
                    let (dist, specifier) = python_requirement(entry);
                    Declared {
                        name: dist,
                        specifier,
                        by: name.clone(),
                    }
                }));
            }
            Ok(_) => {}
            Err(e) => warnings.push(Problem {
                key: relative(&manifest, root),
                message: format!("cannot read the manifest: {e}"),
            }),
        }
    }
    declared
}

/// The project's addons and the selected addons of every aggregated
/// repository.
fn shipped_addon_dirs(prepared: &Prepared) -> Vec<PathBuf> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut dirs: Vec<PathBuf> = config
        .project_addon_dirs()
        .iter()
        .flat_map(|dir| addon_dirs(&root.join(dir)))
        .collect();
    // The configuration was validated when it was loaded.
    let planned = plan_config(config, root, None).unwrap_or_default();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let patterns = configured.addons.clone().unwrap_or_default();
        dirs.extend(addon_dirs(&repo.target_dir).into_iter().filter(|addon| {
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
    dirs.sort();
    dirs.dedup();
    dirs
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// The packages `text` (a requirements file) lists that the platform
/// provides.
pub(crate) fn provided_lines(text: &str, provided: &ProvidedSet) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with(['#', '-']) || line.is_empty() {
            continue;
        }
        let end = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
            .unwrap_or(line.len());
        let name = normalize_name(line.split_at(end).0);
        if provided.contains(&name) {
            found.push(name);
        }
    }
    found
}
