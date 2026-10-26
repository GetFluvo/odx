//! `odx sniff`: database migration scripts that merges and package
//! updates bring in, also reported at the end of `odx aggregate`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use odx_analyze::sniff::{
    AddonChange, Change, Script, addon_changes, manifest_addon, manifest_path, manifest_version,
    script,
};
use odx_config::OdxConfig;
use odx_config::lock::{LockedRepository, Lockfile};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_git::{GitError, Merger};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::prepare;
use crate::Context;
use crate::cli::{GlobalArgs, SniffArgs};

/// What one repository or package change does to database migrations.
pub(crate) struct Sniffed {
    /// Repository name or distribution.
    pub(crate) name: String,
    /// `repository` or `package`.
    pub(crate) kind: &'static str,
    /// Addons with migration scripts or a version change.
    pub(crate) addons: Vec<AddonChange>,
}

/// The migration scripts and manifest version changes between the trees
/// (or commits) `old` and `new` of the cache repository `repo`, under
/// `path` when given.
///
/// # Errors
///
/// When git cannot compare the trees, for instance because they are not in
/// the cache.
pub(crate) fn sniff_range(
    merger: &Merger,
    repo: &Path,
    old: &str,
    new: &str,
    path: Option<&str>,
) -> Result<Vec<AddonChange>, GitError> {
    let files = merger.changed_files(repo, old, new, path)?;
    Ok(sniff_files(merger, repo, old, new, &files))
}

/// The migration scripts and manifest version changes among `files`, the
/// files that differ between `old` and `new` in `repo`.
pub(crate) fn sniff_files(
    merger: &Merger,
    repo: &Path,
    old: &str,
    new: &str,
    files: &[(char, String)],
) -> Vec<AddonChange> {
    let mut scripts = Vec::new();
    let mut versions = BTreeMap::new();
    let version = |rev: &str, path: &str| {
        merger
            .show(repo, rev, path)
            .as_deref()
            .and_then(manifest_version)
    };
    for (status, file) in files {
        let change = match status {
            'A' => Change::Added,
            'M' => Change::Modified,
            _ => continue,
        };
        if let Some(addon) = manifest_addon(file) {
            versions.insert(addon.to_owned(), (version(old, file), version(new, file)));
        }
        scripts.extend(script(file, change));
    }
    // Addons that gained scripts without a manifest change: show their
    // (unchanged) version.
    for found in &scripts {
        if !versions.contains_key(&found.addon) {
            let current = manifest_path(found).and_then(|manifest| version(new, &manifest));
            versions.insert(found.addon.clone(), (current.clone(), current));
        }
    }
    addon_changes(scripts, &versions)
}

/// The text block of `sniffed`: one line per addon and per script.
pub(crate) fn describe(sniffed: &[Sniffed]) -> String {
    let mut text = String::new();
    for source in sniffed.iter().filter(|source| !source.addons.is_empty()) {
        let _ = writeln!(text, "migrations in {} ({}):", source.name, source.kind);
        for addon in &source.addons {
            let version = match (&addon.previous_version, &addon.version) {
                (Some(old), Some(new)) if old != new => format!(" {new} (was {old})"),
                (_, Some(new)) => format!(" {new}"),
                (_, None) => String::new(),
            };
            if addon.bumped_without_scripts() {
                let _ = writeln!(
                    text,
                    "  {}{version}: version changed without migration scripts",
                    addon.addon
                );
                continue;
            }
            let _ = writeln!(text, "  {}{version}", addon.addon);
            for found in &addon.scripts {
                let change = match found.change {
                    Change::Added => "added",
                    Change::Modified => "modified",
                };
                let _ = writeln!(
                    text,
                    "    {:<4} {change:<8} {}",
                    found.phase.as_str(),
                    found.path
                );
            }
        }
    }
    text
}

/// What `odx --json sniff` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx sniff")]
pub(crate) struct SniffJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// Migration scripts found.
    scripts: usize,
    /// Every repository or package that brings migration scripts or a
    /// version change.
    sources: Vec<SniffedJson<'a>>,
    /// Repositories or packages that could not be compared.
    errors: &'a [String],
}

/// What one repository or package change does to database migrations.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct SniffedJson<'a> {
    /// Repository name or distribution.
    name: &'a str,
    /// `repository` or `package`.
    kind: &'a str,
    /// Addons with migration scripts or a version change.
    addons: Vec<AddonChangeJson<'a>>,
}

/// What a change does to one addon's database migration.
#[derive(Debug, Serialize, JsonSchema)]
struct AddonChangeJson<'a> {
    /// The addon.
    addon: &'a str,
    /// The manifest version before the change, if known.
    previous_version: Option<&'a str>,
    /// The manifest version after the change, if known.
    version: Option<&'a str>,
    /// The version changed without migration scripts.
    bumped_without_scripts: bool,
    /// Migration scripts added or modified, sorted by path.
    scripts: &'a [Script],
}

/// `sniffed` as JSON.
pub(crate) fn sniffed_json(sniffed: &[Sniffed]) -> Vec<SniffedJson<'_>> {
    sniffed
        .iter()
        .map(|source| SniffedJson {
            name: &source.name,
            kind: source.kind,
            addons: source
                .addons
                .iter()
                .map(|addon| AddonChangeJson {
                    addon: &addon.addon,
                    previous_version: addon.previous_version.as_deref(),
                    version: addon.version.as_deref(),
                    bumped_without_scripts: addon.bumped_without_scripts(),
                    scripts: &addon.scripts,
                })
                .collect(),
        })
        .collect()
}

/// The number of migration scripts in `sniffed`.
pub(crate) fn script_count(sniffed: &[Sniffed]) -> usize {
    sniffed
        .iter()
        .flat_map(|source| &source.addons)
        .map(|addon| addon.scripts.len())
        .sum()
}

/// Whether migration scripts fail the command: `--fail-on-new` or
/// `[tool.odx.sniff] fail_on_new`.
pub(crate) fn fail_on_new(config: &OdxConfig) -> bool {
    config
        .sniff
        .as_ref()
        .and_then(|sniff| sniff.fail_on_new)
        .unwrap_or_default()
}

/// The migrations every locked repository brings on top of its base.
pub(crate) fn sniff_locked(
    merger: &Merger,
    cache: &odx_git::Cache,
    entries: &[&LockedRepository],
    problems: &mut Vec<String>,
) -> Vec<Sniffed> {
    entries
        .iter()
        .filter_map(|entry| {
            let repo = cache.repo_dir(&entry.url);
            match sniff_range(merger, &repo, &entry.base_sha, &entry.result_tree, None) {
                Ok(addons) => Some(Sniffed {
                    name: entry.name.clone(),
                    kind: "repository",
                    addons,
                }),
                Err(e) => {
                    problems.push(format!("{}: cannot compare: {e}", entry.name));
                    None
                }
            }
        })
        .collect()
}

/// `odx sniff [--since REV]`.
pub(crate) fn run_sniff(args: &SniffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    sniff(args, global, ctx).unwrap_or_else(|report| report)
}

fn sniff(args: &SniffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let Some(lock) = &prepared.lock else {
        return Err(Report::error(
            Exit::Failure,
            format!(
                "{} is missing; run `odx lock`",
                prepared.lock_path.display()
            ),
        ));
    };
    let merger = Merger::new(Arc::new(OsRunner));
    let mut problems = Vec::new();
    let sniffed = match &args.since {
        None => {
            let entries: Vec<&LockedRepository> = lock.repositories.iter().collect();
            sniff_locked(&merger, &prepared.cache, &entries, &mut problems)
        }
        Some(rev) => {
            let previous = previous_lock(&prepared.project.root, &prepared.lock_path, rev)?;
            since(&merger, &prepared.cache, &previous, lock, &mut problems)
        }
    };
    let scripts = script_count(&sniffed);
    let mut report = if global.json {
        Report {
            out: super::to_json(&SniffJson {
                schema: 1,
                scripts,
                sources: sniffed_json(&sniffed),
                errors: &problems,
            }),
            ..Report::default()
        }
    } else {
        let mut out = describe(&sniffed);
        if scripts == 0 {
            out.push_str("no migration scripts\n");
        }
        Report {
            out,
            ..Report::default()
        }
    };
    if !problems.is_empty() {
        report.exit = Exit::Failure;
        report.error = (!global.json).then(|| problems.join("\n"));
    } else if scripts > 0 && (args.fail_on_new || fail_on_new(&prepared.project.config)) {
        report.exit = Exit::Failure;
        report.error = Some(format!("{scripts} migration scripts (fail_on_new)"));
    }
    Ok(report)
}

/// The lockfile as committed at `rev`.
pub(crate) fn previous_lock(root: &Path, lock_path: &Path, rev: &str) -> Result<Lockfile, Report> {
    let relative = lock_path
        .strip_prefix(root)
        .unwrap_or(lock_path)
        .display()
        .to_string()
        .replace('\\', "/");
    let output = OsRunner
        .run(
            &Invocation::new("git")
                .arg("show")
                .arg(format!("{rev}:{relative}"))
                .current_dir(root),
        )
        .ok()
        .filter(odx_core::Output::success)
        .ok_or_else(|| {
            Report::error(
                Exit::Failure,
                format!("cannot read {relative} at {rev} from the project's git history"),
            )
        })?;
    Lockfile::parse(&String::from_utf8_lossy(&output.stdout))
        .map_err(|e| Report::error(Exit::Failure, format!("{relative} at {rev}: {e}")))
}

/// The migrations between two lockfile states: per repository from the
/// previous result to the current one (from the base for a new
/// repository), per package from the previous commit to the current one.
pub(crate) fn since(
    merger: &Merger,
    cache: &odx_git::Cache,
    previous: &Lockfile,
    lock: &Lockfile,
    problems: &mut Vec<String>,
) -> Vec<Sniffed> {
    let mut sniffed = Vec::new();
    for entry in &lock.repositories {
        let old = previous
            .repositories
            .iter()
            .find(|old| old.name == entry.name)
            .map_or(entry.base_sha.as_str(), |old| old.result_tree.as_str());
        if old == entry.result_tree {
            continue;
        }
        let repo = cache.repo_dir(&entry.url);
        match sniff_range(merger, &repo, old, &entry.result_tree, None) {
            Ok(addons) => sniffed.push(Sniffed {
                name: entry.name.clone(),
                kind: "repository",
                addons,
            }),
            Err(e) => problems.push(format!("{}: cannot compare: {e}", entry.name)),
        }
    }
    for (dist, package) in &lock.packages {
        let Some(old) = previous
            .packages
            .get(dist)
            .filter(|old| old.sha != package.sha)
        else {
            continue;
        };
        let repo = cache.repo_dir(&package.url);
        let path = package.subdirectory.as_deref();
        match sniff_range(merger, &repo, &old.sha, &package.sha, path) {
            Ok(addons) => sniffed.push(Sniffed {
                name: dist.clone(),
                kind: "package",
                addons,
            }),
            Err(e) => problems.push(format!("{dist}: cannot compare: {e}")),
        }
    }
    sniffed
}
