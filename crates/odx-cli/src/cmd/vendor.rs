//! `odx vendor`: the addon directories of every dependency group, the
//! vendor manifest, and the platform requirements file (`--requirements`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use odx_aggregate::plan_config;
use odx_analyze::manifest::{parse_manifest, python_requirement};
use odx_analyze::stats::addon_dirs;
use odx_config::Problem;
use odx_config::deps::normalize_name;
use odx_config::schema::PlatformProfile;
use odx_config::uvlock::{UvLock, UvPackage, UvRequirement};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_forge::ReqwestHttp;
use odx_git::Merger;
use odx_platform::export::filter_export;
use odx_platform::provided::ProvidedSet;
use odx_platform::requirements::{
    Conflict, Declared, PlatformRequirements, build, edited, is_odoo, render, with_header,
};
use odx_platform::{profile, profile_name, provided_set, requirements_path};
use odx_vendor::VendorError;
use odx_vendor::archive::download_error;
use odx_vendor::plan::plan;
use odx_vendor::record::VendorRecord;
use odx_vendor::{Options, Vendored};

use super::Report;
use super::aggregate::{Prepared, prepare, uv_program};
use super::hooks::{Hook, Ran, run_hook};
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
    let prepared = prepare(global, ctx)?;
    let config = &prepared.project.config;
    // Requirements alone are fine for a project that vendors nothing.
    let install = profile(config) != PlatformProfile::GenericWheels
        && !(args.requirements.is_some() && config.group_dirs().is_empty());
    if !install && args.requirements.is_none() {
        return Err(Report::error(
            Exit::Usage,
            "the generic-wheels profile ships wheels, not addon directories: \
             `odx vendor --requirements` writes the platform requirements, `odx build` the wheels"
                .to_owned(),
        ));
    }
    let mut report = Report::default();
    let mut json = serde_json::Map::new();
    json.insert("schema".to_owned(), serde_json::json!(1));
    if install {
        install_addons(ctx, &prepared, &mut report, &mut json)?;
    }
    if let Some(target) = &args.requirements {
        write_requirements(
            ctx,
            &prepared,
            target.as_deref(),
            args.strict,
            &mut report,
            &mut json,
        )?;
    }
    match run_hook(ctx, &prepared, Hook::Prepare, args.allow_shell) {
        Ran::Nothing => {}
        Ran::Succeeded => report.out.push_str("ran the custom prepare hook\n"),
        Ran::NotAllowed(message) => report.warnings.push(Problem {
            key: "platform.custom.prepare".to_owned(),
            message,
        }),
        Ran::Failed(message) => {
            report.exit = Exit::Failure;
            report.error = Some(message);
        }
    }
    if global.json {
        report.out = format!("{}\n", serde_json::Value::Object(json));
    }
    Ok(report)
}

/// Replace every group directory with its locked addons and write the
/// vendor manifest.
fn install_addons(
    ctx: &Context<'_>,
    prepared: &Prepared,
    report: &mut Report,
    json: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Report> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let group_dirs: BTreeMap<String, PathBuf> = config
        .group_dirs()
        .into_iter()
        .map(|(group, dir)| (group, root.join(dir)))
        .collect();
    if group_dirs.is_empty() {
        return Err(Report::error(
            Exit::Usage,
            "no [tool.odx.groups] directories to vendor into".to_owned(),
        ));
    }
    let uv_lock = read_uv_lock(ctx, root)?.ok_or_else(|| {
        Report::error(
            Exit::Failure,
            format!(
                "{} is missing; run `uv lock`",
                root.join("uv.lock").display()
            ),
        )
    })?;
    let lock = prepared.lock.clone().unwrap_or_default();
    let git_source = |dist: &str| {
        lock.packages
            .iter()
            .find(|(name, _)| normalize_name(name) == normalize_name(dist))
            .map(|(_, entry)| {
                (
                    entry.url.clone(),
                    entry.sha.clone(),
                    entry.subdirectory.clone(),
                )
            })
    };
    let groups: Vec<String> = group_dirs.keys().cloned().collect();
    let failed = |e: VendorError| Report::error(Exit::Failure, e.to_string());
    let items = plan(&uv_lock, &groups, &git_source).map_err(failed)?;
    let record_path = root.join(vendor_manifest(config));
    let previous = match ctx.fs.read(&record_path) {
        Ok(bytes) => VendorRecord::parse(&String::from_utf8_lossy(&bytes))
            .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", record_path.display())))?,
        Err(_) => VendorRecord::default(),
    };
    let vendor_config = config.vendor.as_ref();
    let patterns = vendor_config
        .and_then(|vendor| vendor.exclude.clone())
        .unwrap_or_else(|| vec!["static/description/index.html".to_owned()]);
    let exclude = |path: &str| {
        patterns
            .iter()
            .any(|pattern| git_aggregate_rs::fnmatch(path, pattern))
    };
    let wheel_cache = prepared.cache.root().join("wheels");
    let options = Options {
        root,
        group_dirs: &group_dirs,
        wheel_cache: &wheel_cache,
        previous: &previous,
        bump: vendor_config
            .and_then(|vendor| vendor.bump_on_content_change)
            .unwrap_or(true),
    };
    let merger = Merger::new(Arc::new(OsRunner));
    let archive = |url: &str, sha: &str, subdirectory: Option<&str>| {
        merger
            .archive(&prepared.cache.repo_dir(url), sha, subdirectory)
            .map_err(|e| format!("commit {sha} of {url} is not in the cache ({e}); run `odx lock`"))
    };
    let vendored = ReqwestHttp::new()
        .map_err(download_error("the package index"))
        .and_then(|http| odx_vendor::vendor(&items, &options, &http, &archive, &exclude))
        .map_err(failed)?;
    record_path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&record_path, vendored.record.render()))
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", record_path.display())))?;
    describe_vendored(&vendored, &group_dirs, root, report, json);
    Ok(())
}

/// Report what `odx vendor` wrote, as text and JSON.
fn describe_vendored(
    vendored: &Vendored,
    group_dirs: &BTreeMap<String, PathBuf>,
    root: &Path,
    report: &mut Report,
    json: &mut serde_json::Map<String, serde_json::Value>,
) {
    let mut vendored_json = Vec::new();
    for (group, dir) in group_dirs {
        let addons: Vec<&str> = vendored
            .record
            .addons
            .iter()
            .filter(|addon| &addon.group == group)
            .map(|addon| addon.name.as_str())
            .collect();
        let _ = writeln!(
            report.out,
            "vendored {group}: {} addons into {}",
            addons.len(),
            relative(dir, root)
        );
        vendored_json.push(
            serde_json::json!({ "group": group, "dir": relative(dir, root), "addons": addons }),
        );
    }
    for bumped in &vendored.bumped {
        let _ = writeln!(
            report.out,
            "bumped {} {} -> {}: content changed without a version change",
            bumped.addon, bumped.from, bumped.to
        );
    }
    report.warnings.extend(vendored.not_bumped.iter().map(|addon| Problem {
        key: addon.clone(),
        message: "content changed without a version change, and the version has no numeric last segment to bump".to_owned(),
    }));
    json.insert(
        "vendored".to_owned(),
        serde_json::Value::Array(vendored_json),
    );
    json.insert(
        "bumped".to_owned(),
        vendored
            .bumped
            .iter()
            .map(|bumped| serde_json::json!({ "addon": bumped.addon, "from": bumped.from, "to": bumped.to }))
            .collect(),
    );
}

/// The vendor manifest path: `[tool.odx.vendor] manifest`, else
/// `[tool.odx.layout] vendor_manifest`, else `.odx/vendor.lock`.
pub(crate) fn vendor_manifest(config: &odx_config::OdxConfig) -> PathBuf {
    config
        .vendor
        .as_ref()
        .and_then(|vendor| vendor.manifest.clone())
        .or_else(|| {
            config
                .layout
                .as_ref()
                .and_then(|layout| layout.vendor_manifest.clone())
        })
        .unwrap_or_else(|| PathBuf::from(".odx/vendor.lock"))
}

/// Write the platform requirements file and report it.
fn write_requirements(
    ctx: &Context<'_>,
    prepared: &Prepared,
    target: Option<&Path>,
    strict: bool,
    report: &mut Report,
    json: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), Report> {
    let root = &prepared.project.root;
    let file = root.join(target.map_or_else(
        || requirements_path(&prepared.project.config),
        Path::to_path_buf,
    ));
    let generated = generate(ctx, prepared)?;
    report.warnings.extend(generated.warnings.iter().cloned());
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
    json.insert(
        "requirements".to_owned(),
        serde_json::json!({
            "file": relative(&file, root),
            "lines": found.lines,
            "omitted": found.omitted,
            "conflicts": found.conflicts,
            "unresolved": found.unresolved,
        }),
    );
    let _ = writeln!(
        report.out,
        "wrote {}: {} requirements, {} provided by the platform left out",
        relative(&file, root),
        found.lines.len(),
        found.omitted.len()
    );
    if strict && !found.conflicts.is_empty() {
        report.exit = Exit::Failure;
        report.error = Some(format!(
            "{} requirements conflict with the platform (--strict)",
            found.conflicts.len()
        ));
    }
    Ok(())
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
    let uv_lock = read_uv_lock(ctx, root)?;
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

/// `uv.lock` of the project, if there is one.
fn read_uv_lock(ctx: &Context<'_>, root: &Path) -> Result<Option<UvLock>, Report> {
    let path = root.join("uv.lock");
    ctx.fs.read(&path).map_or(Ok(None), |bytes| {
        UvLock::parse(&String::from_utf8_lossy(&bytes))
            .map(Some)
            .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", path.display())))
    })
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
