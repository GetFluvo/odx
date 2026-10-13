//! `odx verify`: whether a checkout is exactly what the lockfiles describe,
//! without network access.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use odx_aggregate::{locked_entry, locked_package, plan_config};
use odx_config::deps::normalize_name;
use odx_config::edit::PyprojectEdit;
use odx_config::lock::Lockfile;
use odx_config::packages::package_specs;
use odx_config::schema::PlatformProfile;
use odx_config::uvlock::{UvLock, UvPackage, UvWheel};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_forge::ReqwestHttp;
use odx_git::Merger;
use odx_platform::installed::{
    Distribution, modified_files, scan, site_packages, substituted, wheel_record, wheel_tags,
};
use odx_platform::{profile, requirements_path};
use odx_vendor::archive::{download_error, fetch};
use odx_vendor::check;
use odx_vendor::record::VendorRecord;
use toml_edit::{DocumentMut, Item};

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::hooks::{Hook, Ran, run_hook};
use super::vendor::{describe_conflict, generate, provided_lines, vendor_manifest};
use crate::Context;
use crate::cli::{GlobalArgs, VerifyArgs};

/// `odx verify`.
pub(crate) fn run_verify(args: &VerifyArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    verify(args, global, ctx).unwrap_or_else(|report| report)
}

fn verify(args: &VerifyArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let lock = prepared.lock.clone().unwrap_or_default();
    let mut problems = Vec::new();
    let submodules = submodules(&prepared.project.root, &mut problems);
    let checked = repositories(&prepared, &lock, &submodules, &mut problems);
    let edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    packages(ctx, &prepared, edit.document(), &lock, &mut problems);
    stray_addons(&prepared, &mut problems);
    if args.requirements {
        requirements(ctx, &prepared, &mut problems)?;
    }
    if let Some(dir) = &args.installed {
        installed(ctx, &prepared, dir, &lock, &mut problems);
    }
    match run_hook(ctx, &prepared, Hook::Verify, args.allow_shell) {
        Ran::Nothing | Ran::Succeeded => {}
        Ran::NotAllowed(message) | Ran::Failed(message) => problems.push(message),
    }
    if args.vendored || args.deployed.is_some() {
        vendored(
            ctx,
            &prepared,
            args.deployed.as_deref(),
            &lock,
            &mut problems,
        )?;
    }
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
fn repositories(
    prepared: &Prepared,
    lock: &Lockfile,
    submodules: &BTreeMap<String, String>,
    problems: &mut Vec<String>,
) -> usize {
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
            .to_string()
            .replace('\\', "/");
        let Some(entry) = locked_entry(repo, &lock.repositories) else {
            problems.push(format!(
                "{}: not locked as configured; run `odx lock`",
                repo.name
            ));
            continue;
        };
        checked += 1;
        if let Some(gitlink) = submodules.get(&dir) {
            // A submodule (Odoo.sh): its pinned commit must be the result.
            let tree = merger.tree(&repo.target_dir, gitlink).unwrap_or_default();
            if tree != entry.result_tree {
                problems.push(format!(
                    "{}: submodule {dir} pins {}, which is not the locked result; push the result and update the submodule",
                    repo.name,
                    gitlink.get(..12).unwrap_or(gitlink)
                ));
            }
        } else if !repo.target_dir.exists() {
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

/// The submodules of the project (path → pinned commit); a submodule in
/// `.gitmodules` without a committed commit is reported as unpinned.
fn submodules(root: &Path, problems: &mut Vec<String>) -> BTreeMap<String, String> {
    let git = |args: &[&str]| {
        OsRunner
            .run(
                &Invocation::new("git")
                    .args(args.iter().copied())
                    .current_dir(root),
            )
            .ok()
            .filter(odx_core::Output::success)
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            .unwrap_or_default()
    };
    let mut pinned = BTreeMap::new();
    let listing = git(&[
        "config",
        "-f",
        ".gitmodules",
        "--get-regexp",
        "^submodule\\..*\\.path$",
    ]);
    for path in listing
        .lines()
        .filter_map(|line| line.split_once(' ').map(|(_, path)| path.trim()))
    {
        let entry = git(&["ls-tree", "HEAD", "--", path]);
        match entry.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["160000", "commit", sha, _] => {
                pinned.insert(path.to_owned(), (*sha).to_owned());
            }
            _ => problems.push(format!(
                "{path}: submodule in .gitmodules without a committed commit; Odoo.sh deploys only pinned submodules"
            )),
        }
    }
    pinned
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
    // Packages switched to a local clone by `odx dev <package>`.
    let developing: BTreeSet<String> = ctx
        .fs
        .read(&prepared.project.root.join(".odx/develop.toml"))
        .ok()
        .and_then(|bytes| odx_dev::DevelopState::parse(&String::from_utf8_lossy(&bytes)).ok())
        .map(|state| state.packages.into_keys().collect())
        .unwrap_or_default();
    problems.extend(developing.iter().map(|dist| {
        format!("{dist}: in develop mode (`odx dev`); run `odx dev --restore {dist}` before locking or deploying")
    }));
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
            .filter(|dist| {
                !names.contains(&normalize_name(dist))
                    && !developing.contains(&normalize_name(dist))
            })
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

/// The platform requirements file is what `odx vendor --requirements`
/// writes, lists no provided package and has no unresolved conflict.
fn requirements(
    ctx: &Context<'_>,
    prepared: &Prepared,
    problems: &mut Vec<String>,
) -> Result<(), Report> {
    let generated = generate(ctx, prepared)?;
    let relative = requirements_path(&prepared.project.config);
    let file = prepared.project.root.join(&relative);
    let relative = relative.display().to_string().replace('\\', "/");
    problems.extend(
        generated
            .requirements
            .conflicts
            .iter()
            .map(|conflict| format!("{}: {}", conflict.package, describe_conflict(conflict))),
    );
    let Ok(text) = ctx
        .fs
        .read(&file)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    else {
        problems.push(format!(
            "{relative} is missing; run `odx vendor --requirements`"
        ));
        return Ok(());
    };
    problems.extend(
        provided_lines(&text, &generated.provided)
            .into_iter()
            .map(|package| format!("{relative}: lists {package}, which the platform provides")),
    );
    if text != generated.text {
        problems.push(format!(
            "{relative} is not what `odx vendor --requirements` writes; run it"
        ));
    }
    if profile(&prepared.project.config) == PlatformProfile::OdooSh {
        problems.extend(
            text.lines()
                .filter(|line| {
                    let line = line.split('#').next().unwrap_or_default();
                    line.contains(" @ ")
                        || line.contains("git+")
                        || line.trim_start().starts_with('-')
                })
                .map(|line| {
                    format!(
                        "{relative}: `{}` is not a public PyPI package; Odoo.sh cannot install it",
                        line.trim()
                    )
                }),
        );
    }
    Ok(())
}

/// The vendored addon directories (of the project, or of the deployment
/// checkout `deployed`) are exactly what `odx vendor` wrote, and in the
/// project the vendor manifest still matches the lockfiles.
fn vendored(
    ctx: &Context<'_>,
    prepared: &Prepared,
    deployed: Option<&Path>,
    lock: &Lockfile,
    problems: &mut Vec<String>,
) -> Result<(), Report> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let relative = vendor_manifest(config);
    let Ok(bytes) = ctx.fs.read(&root.join(&relative)) else {
        problems.push(format!(
            "{} is missing; run `odx vendor`",
            relative.display().to_string().replace('\\', "/")
        ));
        return Ok(());
    };
    let record = VendorRecord::parse(&String::from_utf8_lossy(&bytes))
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", relative.display())))?;
    let base = deployed.map_or_else(|| root.clone(), |dir| ctx.dir.join(dir));
    let group_dirs = config
        .group_dirs()
        .into_iter()
        .map(|(group, dir)| (group, base.join(dir)))
        .collect();
    problems.extend(check(&record, &group_dirs, &base));
    if deployed.is_some() {
        return Ok(());
    }
    let uv_lock = ctx
        .fs
        .read(&root.join("uv.lock"))
        .ok()
        .and_then(|bytes| UvLock::parse(&String::from_utf8_lossy(&bytes)).ok())
        .unwrap_or_default();
    for addon in &record.addons {
        let current = addon.source.strip_prefix("git ").map_or_else(
            || {
                uv_lock
                    .packages_named(&addon.package)
                    .iter()
                    .any(|package| {
                        package
                            .wheels
                            .iter()
                            .any(|candidate| candidate.hash == addon.wheel)
                    })
            },
            |sha| {
                lock.packages
                    .iter()
                    .find(|(dist, _)| normalize_name(dist) == normalize_name(&addon.package))
                    .is_none_or(|(_, entry)| entry.sha == sha)
            },
        );
        if !current {
            problems.push(format!(
                "{}: vendored from {} {}, which the lockfiles no longer pin; run `odx vendor`",
                addon.name, addon.package, addon.source
            ));
        }
    }
    Ok(())
}

/// Every distribution installed in `dir` that `uv.lock` knows has the
/// locked version, the locked commit or the files of the locked wheel, and
/// no file changed since it was installed.
fn installed(
    ctx: &Context<'_>,
    prepared: &Prepared,
    dir: &Path,
    lock: &Lockfile,
    problems: &mut Vec<String>,
) {
    let target = ctx.dir.join(dir);
    let Some(site) = site_packages(&target) else {
        problems.push(format!("{}: no site-packages found", target.display()));
        return;
    };
    let uv_lock_path = prepared.project.root.join("uv.lock");
    let Some(uv_lock) = ctx
        .fs
        .read(&uv_lock_path)
        .ok()
        .and_then(|bytes| UvLock::parse(&String::from_utf8_lossy(&bytes)).ok())
    else {
        problems.push(format!(
            "{} is missing or unreadable",
            uv_lock_path.display()
        ));
        return;
    };
    let cache = prepared.cache.root().join("wheels");
    for dist in scan(&site) {
        let built = lock
            .wheels
            .iter()
            .find(|(name, _)| normalize_name(name) == dist.name);
        if let Some((_, wheel)) = built.filter(|(_, wheel)| wheel.version != dist.version) {
            problems.push(format!(
                "{} {} is installed, odx build made {}",
                dist.name, dist.version, wheel.version
            ));
        }
        let candidates = uv_lock.packages_named(&dist.name);
        if candidates.is_empty() {
            continue;
        }
        let Some(package) = candidates
            .iter()
            .find(|package| package.version.as_deref() == Some(dist.version.as_str()))
        else {
            let locked: Vec<&str> = candidates
                .iter()
                .filter_map(|package| package.version.as_deref())
                .collect();
            problems.push(format!(
                "{} {} is installed, uv.lock pins {}",
                dist.name,
                dist.version,
                locked.join(" or ")
            ));
            continue;
        };
        problems.extend(
            modified_files(&site, &dist)
                .into_iter()
                .map(|file| format!("{}: {file} changed after installation", dist.name)),
        );
        if let Some(git) = &package.source.git {
            let locked = git.rsplit_once('#').map(|(_, sha)| sha).unwrap_or_default();
            if dist.commit.as_deref() != Some(locked) {
                problems.push(format!(
                    "{}: installed from commit {}, uv.lock pins {locked}",
                    dist.name,
                    dist.commit.as_deref().unwrap_or("(none)")
                ));
            }
            continue;
        }
        if package.wheels.is_empty() {
            // A local directory or file: nothing locked to compare with.
            continue;
        }
        problems.extend(compare_with_wheel(package, &dist, &cache));
    }
}

/// Whether `dist` holds the files of the locked wheel of `package` for its
/// tags; a problem when it does not or the wheel cannot be had.
fn compare_with_wheel(package: &UvPackage, dist: &Distribution, cache: &Path) -> Option<String> {
    let location = |wheel: &&UvWheel| {
        wheel
            .url
            .as_deref()
            .or(wheel.path.as_deref())
            .unwrap_or_default()
            .to_owned()
    };
    let Some(wheel) = package.wheels.iter().find(|wheel| {
        wheel_tags(&location(wheel))
            .iter()
            .any(|tag| dist.tags.contains(tag))
    }) else {
        return Some(format!(
            "{} {}: no wheel in uv.lock matches what is installed",
            dist.name, dist.version
        ));
    };
    let url = location(&wheel);
    let hash = wheel.hash.clone().unwrap_or_default();
    let record = ReqwestHttp::new()
        .map_err(download_error(&url))
        .and_then(|http| fetch(&http, cache, &url, &hash))
        .map_err(|e| e.to_string())
        .and_then(|bytes| wheel_record(&bytes));
    match record {
        Ok(record) => {
            let different = substituted(&record, &dist.record);
            different.first().map(|first| {
                format!(
                    "{} {}: not the locked wheel ({} files differ, such as {first})",
                    dist.name,
                    dist.version,
                    different.len()
                )
            })
        }
        Err(e) => Some(format!(
            "{}: cannot compare with the locked wheel: {e}",
            dist.name
        )),
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
