//! `odx build`: wheels of the aggregated, unmerged and project addons, from
//! the locked sources, with content-derived `+odx.<tree>` versions.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use odx_config::deps::normalize_name;
use odx_config::edit::PyprojectEdit;
use odx_config::lock::{LockedWheel, Lockfile};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_git::Merger;
use odx_platform::wheel::retag;
use odx_vendor::archive::{extract, subdirectory_entry};
use odx_vendor::content::hex;
use sha2::{Digest, Sha256};

use super::Report;
use super::aggregate::{Prepared, prepare, uv_program, write_lock};
use crate::Context;
use crate::cli::{BuildArgs, GlobalArgs};

/// The build backend of addons that do not declare one, as OCA does.
const WHOOL: &str = "[build-system]\nrequires = [\"whool\"]\nbuild-backend = \"whool.buildapi\"\n";

/// One addon to build.
#[derive(Debug, Clone)]
struct Target {
    /// Addon (directory) name.
    addon: String,
    /// What it is built from, for `odx.lock`.
    source: String,
    /// The git repository holding the objects.
    repo: PathBuf,
    /// The commit or tree to export.
    rev: String,
    /// The addon's directory in it (empty: the root).
    path: String,
    /// The addon directory's tree.
    tree: String,
    /// The pull request the source comes from, if any.
    pr: Option<u32>,
}

/// `odx build`.
pub(crate) fn run_build(args: &BuildArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    build(args, global, ctx).unwrap_or_else(|report| report)
}

fn build(args: &BuildArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    if args.sbom {
        return Err(Report::error(
            Exit::Usage,
            "odx build --sbom: not implemented yet".to_owned(),
        ));
    }
    let prepared = prepare(global, ctx)?;
    let Some(mut lock) = prepared.lock.clone() else {
        return Err(Report::error(
            Exit::Failure,
            format!(
                "{} is missing; run `odx lock`",
                prepared.lock_path.display()
            ),
        ));
    };
    let merger = Merger::new(Arc::new(OsRunner));
    let mut failures = Vec::new();
    let targets: Vec<Target> = targets(&prepared, &lock, &merger, args.project, &mut failures)
        .into_iter()
        .filter(|target| {
            args.addon.is_empty()
                || args
                    .addon
                    .iter()
                    .any(|pattern| git_aggregate_rs::fnmatch(&target.addon, pattern))
        })
        .collect();
    // One addon, one wheel: a second source of the same addon is an error.
    let mut seen: Vec<&Target> = Vec::new();
    for target in &targets {
        match seen.iter().find(|first| first.addon == target.addon) {
            Some(first) => failures.push(format!(
                "{}: comes from {} and from {}; keep one",
                target.addon, first.source, target.source
            )),
            None => seen.push(target),
        }
    }
    let targets: Vec<Target> = seen.into_iter().cloned().collect();
    let root = &prepared.project.root;
    let out_dir = root.join(
        args.out_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("dist/wheels")),
    );
    let mut report = Report::default();
    let mut built = Vec::new();
    for target in &targets {
        match build_one(ctx, &prepared, &merger, target, &out_dir) {
            Ok((dist, file, wheel)) => {
                let _ = writeln!(
                    report.out,
                    "built {dist} {} from {} into {}",
                    wheel.version,
                    target.source,
                    relative(&file, root)
                );
                built.push((dist.clone(), file));
                lock.wheels.insert(dist, wheel);
            }
            Err(e) => failures.push(format!("{}: {e}", target.addon)),
        }
    }
    if args.publish && !built.is_empty() {
        let index = args
            .index
            .clone()
            .or_else(|| {
                prepared
                    .project
                    .config
                    .platform
                    .as_ref()
                    .and_then(|platform| platform.index.clone())
            })
            .unwrap_or_else(|| "private".to_owned());
        match publish(ctx, &prepared, &index, &built) {
            Ok(()) => {
                for (dist, _) in &built {
                    lock.wheels
                        .entry(dist.clone())
                        .and_modify(|wheel| wheel.index = Some(index.clone()));
                }
                let _ = writeln!(report.out, "published {} wheels to {index}", built.len());
            }
            Err(e) => failures.push(e),
        }
    }
    if prepared.lock.as_ref() != Some(&lock) {
        write_lock(ctx, &prepared, &lock)?;
    }
    if targets.is_empty() && failures.is_empty() {
        report.out.push_str("nothing to build\n");
    }
    if !failures.is_empty() {
        report.exit = Exit::Failure;
        report.error = Some(format!("build failed for\n  {}", failures.join("\n  ")));
    }
    Ok(report)
}

/// The addons to build: the selected addons of every locked repository,
/// every locked package, and with `project` the project's own addons.
fn targets(
    prepared: &Prepared,
    lock: &Lockfile,
    merger: &Merger,
    project: bool,
    failures: &mut Vec<String>,
) -> Vec<Target> {
    let config = &prepared.project.config;
    let mut targets = Vec::new();
    for entry in &lock.repositories {
        let repo = prepared.cache.repo_dir(&entry.url);
        let patterns = config
            .repositories
            .iter()
            .find(|configured| configured.name.as_deref().unwrap_or(&configured.url) == entry.name)
            .and_then(|configured| configured.addons.clone())
            .unwrap_or_default();
        let Ok(dirs) = merger.dirs(&repo, &entry.result_tree) else {
            failures.push(format!(
                "{}: the locked result is not in the cache; run `odx aggregate --locked`",
                entry.name
            ));
            continue;
        };
        for dir in dirs {
            let selected = patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| git_aggregate_rs::fnmatch(&dir, pattern));
            let manifest =
                merger.show(&repo, &entry.result_tree, &format!("{dir}/__manifest__.py"));
            if !selected || manifest.is_none() {
                continue;
            }
            let tree = merger
                .object(&repo, &format!("{}:{dir}", entry.result_tree))
                .unwrap_or_default();
            targets.push(Target {
                addon: dir.clone(),
                source: format!("repository {}", entry.name),
                repo: repo.clone(),
                rev: entry.result_tree.clone(),
                path: dir,
                tree,
                pr: None,
            });
        }
    }
    for (dist, package) in &lock.packages {
        let repo = prepared.cache.repo_dir(&package.url);
        let path = package.subdirectory.clone().unwrap_or_default();
        let Ok(tree) = merger.object(&repo, &format!("{}:{path}", package.sha)) else {
            failures.push(format!(
                "{dist}: commit {} is not in the cache; run `odx lock`",
                package.sha
            ));
            continue;
        };
        let addon = path
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map_or_else(
                || {
                    normalize_name(dist)
                        .trim_start_matches("odoo-addon-")
                        .replace('-', "_")
                },
                str::to_owned,
            );
        targets.push(Target {
            addon,
            source: format!("package {dist}"),
            repo,
            rev: package.sha.clone(),
            path,
            tree,
            pr: package.source.pr,
        });
    }
    if project {
        let root = &prepared.project.root;
        for dir in config.project_addon_dirs() {
            for addon in odx_analyze::stats::addon_dirs(&root.join(&dir)) {
                let path = addon
                    .strip_prefix(root)
                    .unwrap_or(&addon)
                    .display()
                    .to_string()
                    .replace('\\', "/");
                let Ok(tree) = merger.object(root, &format!("HEAD:{path}")) else {
                    failures.push(format!("{path}: not committed; commit it first"));
                    continue;
                };
                targets.push(Target {
                    addon: path.rsplit('/').next().unwrap_or(&path).to_owned(),
                    source: "project".to_owned(),
                    repo: root.clone(),
                    rev: "HEAD".to_owned(),
                    path,
                    tree,
                    pr: None,
                });
            }
        }
    }
    targets
}

/// Export `target`, build it with its backend, give the wheel its odx
/// version and write it to `out_dir`.
fn build_one(
    ctx: &Context<'_>,
    prepared: &Prepared,
    merger: &Merger,
    target: &Target,
    out_dir: &Path,
) -> Result<(String, PathBuf, LockedWheel), String> {
    let scratch = prepared.cache.root().join("build").join(&target.tree);
    let _ = std::fs::remove_dir_all(&scratch);
    let src = scratch.join("src");
    let out = scratch.join("out");
    let path = (!target.path.is_empty()).then_some(target.path.as_str());
    let zip = merger
        .archive(&target.repo, &target.rev, path)
        .map_err(|e| e.to_string())?;
    let addon = target.addon.clone();
    let select = |name: &str| {
        path.map_or_else(
            || Some((addon.clone(), name.to_owned())),
            |path| subdirectory_entry(path, name),
        )
    };
    extract(&zip, &target.addon, &select, &|_| false, &src).map_err(|e| e.to_string())?;
    let dir = src.join(&target.addon);
    let pyproject = dir.join("pyproject.toml");
    if !pyproject.exists() {
        std::fs::write(&pyproject, WHOOL).map_err(|e| e.to_string())?;
    }
    // Backends such as whool list the files with git when the directory is
    // inside a repository; make it its own, with every file staged, so an
    // enclosing repository never changes what is built.
    for args in [&["init", "--quiet"][..], &["add", "--all"][..]] {
        // Best effort: without it the backend sees the directory as is.
        let _ = OsRunner.run(
            &Invocation::new("git")
                .args(args.iter().copied())
                .current_dir(&dir),
        );
    }
    let uv = uv_program(ctx.env);
    let invocation = Invocation::new(&uv)
        .args(["build", "--wheel"])
        .arg(dir.as_os_str())
        .arg("--out-dir")
        .arg(out.as_os_str())
        .env("WHOOL_POST_VERSION_STRATEGY_OVERRIDE", "none")
        .env("SOURCE_DATE_EPOCH", "315532800")
        .current_dir(&prepared.project.root);
    run_checked(&invocation, &format!("{uv} build"))?;
    let wheel = std::fs::read_dir(&out)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|file| file.extension().is_some_and(|extension| extension == "whl"))
        .ok_or_else(|| format!("{uv} build made no wheel"))?;
    // An unreadable wheel reads as empty, which retag refuses.
    let bytes = std::fs::read(&wheel).unwrap_or_default();
    let short = target.tree.get(..12).unwrap_or(&target.tree);
    let local = target.pr.map_or_else(
        || format!("odx.{short}"),
        |pr| format!("odx.pr{pr}.{short}"),
    );
    let retagged = retag(&bytes, &local)?;
    let file = out_dir.join(&retagged.file_name);
    std::fs::create_dir_all(out_dir)
        .and_then(|()| std::fs::write(&file, &retagged.bytes))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let _ = std::fs::remove_dir_all(&scratch);
    let dist = normalize_name(retagged.file_name.split('-').next().unwrap_or_default());
    Ok((
        dist,
        file,
        LockedWheel {
            version: retagged.version,
            sha256: format!("sha256:{}", hex(&Sha256::digest(&retagged.bytes))),
            index: None,
            source: target.source.clone(),
            tree: target.tree.clone(),
        },
    ))
}

/// `uv publish --index <index>` the built wheels; the index must be a
/// `[[tool.uv.index]]` that is not PyPI, which refuses local versions.
fn publish(
    ctx: &Context<'_>,
    prepared: &Prepared,
    index: &str,
    built: &[(String, PathBuf)],
) -> Result<(), String> {
    let edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let configured = edit
        .document()
        .get("tool")
        .and_then(|tool| tool.get("uv"))
        .and_then(|uv| uv.get("index"))
        .and_then(toml_edit::Item::as_array_of_tables)
        .and_then(|indexes| {
            indexes
                .iter()
                .find(|table| table.get("name").and_then(toml_edit::Item::as_str) == Some(index))
                .map(|table| {
                    ["url", "publish-url"]
                        .iter()
                        .filter_map(|key| table.get(key).and_then(toml_edit::Item::as_str))
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                })
        })
        .ok_or_else(|| {
            format!("publish: pyproject.toml has no [[tool.uv.index]] named {index:?}")
        })?;
    if configured.iter().any(|url| url.contains("pypi.org")) {
        return Err(format!(
            "publish: {index} is PyPI, which refuses the +odx local versions; publish to a private index"
        ));
    }
    let uv = uv_program(ctx.env);
    let invocation = built.iter().fold(
        Invocation::new(&uv)
            .args(["publish", "--index", index])
            .current_dir(&prepared.project.root),
        |invocation, (_, file)| invocation.arg(file.as_os_str()),
    );
    run_checked(&invocation, &format!("{uv} publish"))
}

/// Run `invocation`; a failure carries its standard error.
fn run_checked(invocation: &Invocation, what: &str) -> Result<(), String> {
    let output = OsRunner.run(invocation);
    if output.as_ref().is_ok_and(odx_core::Output::success) {
        return Ok(());
    }
    let detail = output.map_or_else(
        |e| e.to_string(),
        |out| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
    );
    Err(format!("{what} failed: {detail}"))
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
        .replace('\\', "/")
}
