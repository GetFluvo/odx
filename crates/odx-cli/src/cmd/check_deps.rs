//! `odx check-deps`: compare the dependencies the project's manifests
//! declare with what the project provides.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use odx_aggregate::plan_config;
use odx_analyze::core::core_addons;
use odx_analyze::manifest::{Manifest, parse_manifest};
use odx_analyze::stats::addon_dirs;
use odx_config::Problem;
use odx_config::deps::{
    add_to_group, all_requirements, normalize_name, parse_requirement, read_group,
};
use odx_config::edit::PyprojectEdit;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_forge::{CachedHttp, ReqwestHttp};
use odx_git::Merger;
use odx_index::sources::{distribution, package_index};
use odx_vendor::record::VendorRecord;

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::vendor::vendor_manifest;
use crate::Context;
use crate::cli::{CheckDepsArgs, GlobalArgs};

/// A scanned addon: name, where it comes from, its manifest.
struct Scanned {
    name: String,
    dir: PathBuf,
    manifest: Manifest,
    /// Why the manifest could not be read, when it could not.
    unreadable: Option<String>,
}

/// What check-deps found.
#[derive(Default)]
struct Findings {
    /// (addon, needed by).
    missing: Vec<(String, String)>,
    /// (python package, needed by).
    python: Vec<(String, String)>,
    /// (addon, sources).
    duplicates: Vec<(String, Vec<String>)>,
    /// (path, why).
    stray: Vec<(String, &'static str)>,
    /// Declared addon packages nothing depends on.
    unused: Vec<String>,
    /// (addon, its license, dependency, its license).
    licenses: Vec<(String, String, String, String)>,
    /// Packages `--fix` added: (distribution, group).
    fixed: Vec<(String, String)>,
}

impl Findings {
    const fn problems(&self) -> usize {
        self.missing.len()
            + self.python.len()
            + self.duplicates.len()
            + self.stray.len()
            + self.unused.len()
            + self.licenses.len()
    }
}

/// `odx check-deps`.
pub(crate) fn run_check_deps(
    args: &CheckDepsArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    check_deps(args, global, ctx).unwrap_or_else(|report| report)
}

fn name_of(dir: &Path) -> String {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The addon a distribution installs, if it is an addon package.
fn addon_of(dist: &str) -> Option<String> {
    let dist = normalize_name(dist);
    dist.strip_prefix("odoo-addon-")
        .or_else(|| dist.split_once("-addon-").map(|(_, addon)| addon))
        .map(|addon| addon.replace('-', "_"))
}

/// How strongly a license binds what depends on it: 3 AGPL, 2 GPL, 1
/// LGPL, 0 permissive or proprietary.
fn copyleft(license: &str) -> u8 {
    let license = license.to_ascii_uppercase();
    ["LGPL", "GPL", "AGPL"]
        .iter()
        .zip(1..)
        .filter(|(prefix, _)| license.starts_with(*prefix))
        .map(|(_, strength)| strength)
        .max()
        .unwrap_or(0)
}

/// Whether an addon under `depender` may not depend on one under
/// `dependency`: AGPL code needs AGPL dependers, GPL code (A)GPL ones.
fn incompatible(depender: &str, dependency: &str) -> bool {
    let needed = copyleft(dependency);
    needed >= 2 && copyleft(depender) < needed
}

/// Addons of the project's own directories that git does not track.
fn untracked(root: &Path, own: &[Scanned]) -> Vec<String> {
    let git = |args: &[&str]| {
        OsRunner
            .run(
                &Invocation::new("git")
                    .arg("-C")
                    .arg(root.as_os_str())
                    .args(args),
            )
            .is_ok_and(|output| output.success() && !output.stdout.is_empty())
    };
    if !git(&["rev-parse", "--git-dir"]) {
        return Vec::new();
    }
    own.iter()
        .filter(|addon| {
            let manifest = addon.dir.join("__manifest__.py");
            !git(&["ls-files", "--", &manifest.display().to_string()])
        })
        .map(|addon| relative(root, &addon.dir))
        .collect()
}

fn relative(root: &Path, dir: &Path) -> String {
    // Configured directories may be written with `/` on Windows too.
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .collect::<PathBuf>()
        .display()
        .to_string()
}

fn scan(dir: PathBuf) -> Scanned {
    let manifest = std::fs::read_to_string(dir.join("__manifest__.py"))
        .map_err(|e| e.to_string())
        .and_then(|text| parse_manifest(&text));
    Scanned {
        name: name_of(&dir),
        dir,
        unreadable: manifest.as_ref().err().cloned(),
        manifest: manifest.unwrap_or_default(),
    }
}

/// What the project has: who provides which addon, the addons to scan,
/// the declared addon packages and Python requirements.
pub(crate) struct Inventory {
    pub(crate) sources: BTreeMap<String, Vec<String>>,
    own: Vec<Scanned>,
    aggregated: Vec<Scanned>,
    /// Addon → distribution, for declared addon packages.
    packages: BTreeMap<String, String>,
    /// Normalized names of every declared requirement.
    declared: BTreeSet<String>,
}

impl Inventory {
    fn scanned(&self) -> impl Iterator<Item = &Scanned> {
        self.own.iter().chain(&self.aggregated)
    }
}

pub(crate) fn inventory(prepared: &Prepared) -> Inventory {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let own: Vec<Scanned> = config
        .project_addon_dirs()
        .iter()
        .flat_map(|dir| addon_dirs(&root.join(dir)))
        .map(scan)
        .collect();
    for addon in &own {
        sources
            .entry(addon.name.clone())
            .or_default()
            .push("the project".to_owned());
    }
    let planned = plan_config(config, root, None).unwrap_or_default();
    let merger = Merger::new(Arc::new(OsRunner));
    let mut aggregated: Vec<Scanned> = Vec::new();
    for (repo, configured) in planned.iter().zip(&config.repositories) {
        let patterns = configured.addons.clone().unwrap_or_default();
        let selected = |name: &str| {
            patterns.is_empty()
                || patterns
                    .iter()
                    .any(|pattern| git_aggregate_rs::fnmatch(name, pattern))
        };
        let label = format!("repository {}", repo.name);
        let built = addon_dirs(&repo.target_dir);
        if built.is_empty() {
            // Not built yet: what the cache has of its base.
            let base = repo
                .base
                .source
                .local_ref(repo.forge, repo.base.remote.as_deref());
            let cached = merger
                .addons(&prepared.cache.repo_dir(&repo.url), &base)
                .unwrap_or_default();
            for name in cached.into_iter().filter(|name| selected(name)) {
                sources.entry(name).or_default().push(label.clone());
            }
        }
        for dir in built.into_iter().filter(|dir| selected(&name_of(dir))) {
            let addon = scan(dir);
            sources
                .entry(addon.name.clone())
                .or_default()
                .push(label.clone());
            aggregated.push(addon);
        }
    }
    let doc = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let requirements = all_requirements(doc.document());
    let mut packages: BTreeMap<String, String> = BTreeMap::new();
    for (group, requirement) in &requirements {
        if let Some(addon) = addon_of(&requirement.name) {
            let label = group.as_ref().map_or_else(
                || format!("package {}", requirement.name),
                |group| format!("package {} ({group})", requirement.name),
            );
            sources.entry(addon.clone()).or_default().push(label);
            packages.insert(addon, requirement.name.clone());
        }
    }
    Inventory {
        sources,
        own,
        aggregated,
        packages,
        declared: requirements
            .iter()
            .map(|(_, requirement)| normalize_name(&requirement.name))
            .collect(),
    }
}

/// Missing addons and Python packages, and addons from two sources.
fn missing(
    inventory: &Inventory,
    core: &BTreeSet<&str>,
    provided: &BTreeSet<String>,
    findings: &mut Findings,
) {
    for addon in inventory.scanned() {
        for dep in &addon.manifest.depends {
            let known = core.contains(dep.as_str())
                || provided.contains(dep)
                || inventory.sources.contains_key(dep);
            if !known {
                findings.missing.push((dep.clone(), addon.name.clone()));
            }
        }
        for package in &addon.manifest.python {
            let name = parse_requirement(package).map_or_else(|_| package.clone(), |r| r.name);
            if !inventory.declared.contains(&normalize_name(&name)) {
                findings.python.push((name, addon.name.clone()));
            }
        }
    }
    for (addon, from) in &inventory.sources {
        if from.len() > 1 {
            findings.duplicates.push((addon.clone(), from.clone()));
        }
    }
}

/// Addon directories nobody put there: in vendored groups but not in the
/// vendor manifest, or in the project but not tracked by git.
fn stray(prepared: &Prepared, inventory: &Inventory, findings: &mut Findings) {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let record = std::fs::read_to_string(root.join(vendor_manifest(config)))
        .ok()
        .and_then(|text| VendorRecord::parse(&text).ok());
    if let Some(record) = record {
        for (group, dir) in config.group_dirs() {
            for addon in addon_dirs(&root.join(&dir)) {
                let name = name_of(&addon);
                if !record
                    .addons
                    .iter()
                    .any(|v| v.group == group && v.name == name)
                {
                    findings
                        .stray
                        .push((relative(root, &addon), "not vendored by odx vendor"));
                }
            }
        }
    }
    findings.stray.extend(
        untracked(root, &inventory.own)
            .into_iter()
            .map(|path| (path, "not tracked by git")),
    );
}

/// Addons whose license their dependencies' licenses do not allow.
fn licenses(inventory: &Inventory, core: &BTreeSet<&str>, findings: &mut Findings) {
    let known: BTreeMap<&str, &str> = inventory
        .scanned()
        .filter_map(|addon| {
            addon
                .manifest
                .license
                .as_deref()
                .map(|license| (addon.name.as_str(), license))
        })
        .chain(core.iter().map(|addon| (*addon, "LGPL-3")))
        .collect();
    for addon in inventory.scanned() {
        let mine = addon.manifest.license.as_deref().unwrap_or("LGPL-3");
        for dep in &addon.manifest.depends {
            if let Some(theirs) = known
                .get(dep.as_str())
                .filter(|theirs| incompatible(mine, theirs))
            {
                findings.licenses.push((
                    addon.name.clone(),
                    mine.to_owned(),
                    dep.clone(),
                    (*theirs).to_owned(),
                ));
            }
        }
    }
}

fn check_deps(
    args: &CheckDepsArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let config = &prepared.project.config;
    let series = &config.odoo_version;
    let mut report = Report::default();
    let core: BTreeSet<&str> = core_addons(series).map_or_else(
        || {
            report.warnings.push(Problem {
                key: "odoo_version".to_owned(),
                message: format!(
                    "no list of Odoo's own addons for {series}; they count as missing"
                ),
            });
            BTreeSet::new()
        },
        |core| core.into_iter().collect(),
    );
    let provided: BTreeSet<String> = config
        .check_deps
        .as_ref()
        .and_then(|check| check.provided.clone())
        .unwrap_or_default()
        .into_iter()
        .collect();
    let inventory = inventory(&prepared);
    // An addon whose manifest odx cannot read has no dependencies to check:
    // say so rather than find it fine.
    report
        .warnings
        .extend(inventory.scanned().filter_map(|addon| {
            addon.unreadable.as_ref().map(|why| Problem {
                key: addon.name.clone(),
                message: format!(
                    "the manifest cannot be read ({why}); its dependencies are not checked"
                ),
            })
        }));
    let mut findings = Findings::default();
    missing(&inventory, &core, &provided, &mut findings);
    stray(&prepared, &inventory, &mut findings);
    if args.unused {
        let used: BTreeSet<&String> = inventory
            .scanned()
            .flat_map(|addon| &addon.manifest.depends)
            .collect();
        findings.unused = inventory
            .packages
            .iter()
            .filter(|(addon, _)| !used.contains(addon))
            .map(|(_, dist)| dist.clone())
            .collect();
    }
    if args.licenses {
        licenses(&inventory, &core, &mut findings);
    }
    if args.fix && !findings.missing.is_empty() {
        fix(ctx, &prepared, global, &mut findings)?;
    }
    let scanned = inventory.scanned().count();
    report.out = if global.json {
        format!("{}\n", json(&findings))
    } else {
        text(&findings, scanned)
    };
    if findings.problems() > 0 {
        report.exit = Exit::Failure;
    }
    Ok(report)
}

/// Add the missing addons the package index has a release of to the
/// default group.
fn fix(
    ctx: &Context<'_>,
    prepared: &Prepared,
    global: &GlobalArgs,
    findings: &mut Findings,
) -> Result<(), Report> {
    let config = &prepared.project.config;
    let group_name = config
        .groups
        .as_ref()
        .and_then(|groups| groups.default.clone())
        .ok_or_else(|| {
            Report::error(
                Exit::Usage,
                "--fix needs [tool.odx.groups] default, the group to add packages to".to_owned(),
            )
        })?;
    let mut edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let group = read_group(edit.document(), &group_name).ok_or_else(|| {
        Report::error(
            Exit::Usage,
            format!("dependency group {group_name:?} is not declared"),
        )
    })?;
    let pypi = ctx
        .env
        .var("ODX_PYPI_URL")
        .as_deref()
        .unwrap_or("https://pypi.org/pypi")
        .to_owned();
    let http =
        ReqwestHttp::new().map(|http| CachedHttp::new(http, prepared.cache.root().join("http")));
    let series = &config.odoo_version;
    let wanted: BTreeSet<String> = findings
        .missing
        .iter()
        .map(|(addon, _)| addon.clone())
        .collect();
    for addon in wanted {
        let found = http
            .as_ref()
            .ok()
            .filter(|_| !global.offline)
            .and_then(|http| package_index(http, &pypi, &addon, series).ok().flatten());
        if found.is_some() {
            let dist = distribution(&addon, series);
            add_to_group(edit.document_mut(), &group, &dist);
            findings.missing.retain(|(missing, _)| *missing != addon);
            findings.fixed.push((dist, group_name.clone()));
        }
    }
    if !findings.fixed.is_empty() {
        let file = &prepared.project.file;
        std::fs::write(file, edit.document().to_string())
            .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
    }
    Ok(())
}

fn text(findings: &Findings, scanned: usize) -> String {
    let mut out = String::new();
    for (dist, group) in &findings.fixed {
        let _ = writeln!(out, "added {dist} to group {group}; run uv lock");
    }
    for (addon, by) in &findings.missing {
        let _ = writeln!(out, "missing: {addon} (needed by {by})");
    }
    for (package, by) in &findings.python {
        let _ = writeln!(out, "missing python package: {package} (needed by {by})");
    }
    for (addon, from) in &findings.duplicates {
        let _ = writeln!(out, "duplicate: {addon} in {}", from.join(", "));
    }
    for (path, why) in &findings.stray {
        let _ = writeln!(out, "stray: {path} ({why})");
    }
    for dist in &findings.unused {
        let _ = writeln!(out, "unused: {dist} (no scanned addon depends on it)");
    }
    for (addon, mine, dep, theirs) in &findings.licenses {
        let _ = writeln!(out, "license: {addon} ({mine}) depends on {dep} ({theirs})");
    }
    if findings.problems() == 0 {
        let _ = writeln!(out, "the dependencies of {scanned} addons are provided");
    }
    out
}

fn json(findings: &Findings) -> serde_json::Value {
    let pairs = |list: &[(String, String)], a: &str, b: &str| {
        list.iter()
            .map(|(x, y)| serde_json::json!({ a: x, b: y }))
            .collect::<Vec<_>>()
    };
    serde_json::json!({
        "schema": 1,
        "missing": pairs(&findings.missing, "addon", "needed_by"),
        "python": pairs(&findings.python, "package", "needed_by"),
        "duplicates": findings.duplicates.iter().map(|(addon, from)| serde_json::json!({"addon": addon, "sources": from})).collect::<Vec<_>>(),
        "stray": findings.stray.iter().map(|(path, why)| serde_json::json!({"path": path, "reason": why})).collect::<Vec<_>>(),
        "unused": findings.unused,
        "licenses": findings.licenses.iter().map(|(addon, mine, dep, theirs)| serde_json::json!({"addon": addon, "license": mine, "depends_on": dep, "its_license": theirs})).collect::<Vec<_>>(),
        "fixed": pairs(&findings.fixed, "package", "group"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_compatibility() {
        assert!(incompatible("LGPL-3", "AGPL-3"));
        assert!(incompatible("OPL-1", "GPL-3 or any later version"));
        assert!(!incompatible("AGPL-3", "AGPL-3"));
        assert!(!incompatible("GPL-3", "LGPL-3"));
        assert!(!incompatible("Other proprietary", "LGPL-3"));
        assert!(!incompatible("AGPL-3", "GPL-3"));
        assert_eq!(addon_of("odoo14-addon-pos-x").as_deref(), Some("pos_x"));
        assert_eq!(addon_of("requests"), None);
    }
}
