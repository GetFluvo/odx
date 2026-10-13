use std::path::{Path, PathBuf};

use odx_config::OdxConfig;
use odx_config::lock::{LockSource, LockedRepository, MergeStatus};
use odx_config::schema::{MergeSource, OnConflict, OnHeadChange, Repository};
use odx_core::repo_key;
use odx_git::{Forge, Source};

use crate::{PlanError, forge};

/// A source to fetch: what, and from which remote URL when that is not the
/// repository's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSource {
    /// The remote URL, when not the repository URL.
    pub remote: Option<String>,
    /// What to fetch.
    pub source: Source,
}

/// One merge on top of the base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedStep {
    /// How the merge is shown, e.g. `#123` or `oca refs/pull/123/head`.
    pub label: String,
    /// What to merge.
    pub from: PlannedSource,
    /// Skip the merge on a conflict instead of stopping.
    pub skip_on_conflict: bool,
    /// The configured source as `odx.lock` records it.
    pub lock: LockSource,
    /// What `odx lock --update` does when a pull request's head moved.
    pub on_head_change: OnHeadChange,
}

/// One repository to build: the configuration in a form that `[tool.odx]`
/// and `repos.yaml` share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The repository's name (its URL, or its target directory in
    /// `repos.yaml`, when unnamed).
    pub name: String,
    /// The repository URL; the cache entry is keyed by it.
    pub url: String,
    /// How the forge names pull request refs.
    pub forge: Forge,
    /// The working tree to build.
    pub target_dir: PathBuf,
    /// The base as configured (a branch, or a ref in `repos.yaml`).
    pub base_name: String,
    /// The base to fetch.
    pub base: PlannedSource,
    /// Merges, in order.
    pub steps: Vec<PlannedStep>,
    /// Whether the sources were replaced by commits from `odx.lock`.
    pub pinned: bool,
}

/// The working tree of a repository: `target_dir`, else
/// `<layout.aggregate_dir or "vendor">/<name>`, relative to `root`.
fn target_dir(config: &OdxConfig, root: &Path, repo: &Repository, name: &str) -> PathBuf {
    repo.target_dir.as_ref().map_or_else(
        || {
            let parent = config
                .layout
                .as_ref()
                .and_then(|layout| layout.aggregate_dir.clone())
                .unwrap_or_else(|| PathBuf::from("vendor"));
            root.join(parent)
                .join(name.rsplit('/').next().unwrap_or(name))
        },
        |dir| root.join(dir),
    )
}

/// One plan per configured repository (only `only`, when given), with
/// target directories relative to `root`.
///
/// ```
/// use odx_aggregate::plan_config;
/// use odx_config::parse_pyproject;
/// use std::path::Path;
///
/// let text = "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = [{ pr = 1 }]\n";
/// let config = parse_pyproject(text, Path::new("p.toml")).unwrap().unwrap().config;
/// let planned = plan_config(&config, Path::new("/p"), None).unwrap();
/// assert_eq!(planned[0].target_dir, Path::new("/p/vendor/pos"));
/// assert_eq!(planned[0].steps[0].label, "#1");
/// ```
///
/// # Errors
///
/// [`PlanError::UnknownRepository`] when `only` names no repository,
/// [`PlanError::InvalidMerge`] for an invalid merge entry.
pub fn plan_config(
    config: &OdxConfig,
    root: &Path,
    only: Option<&str>,
) -> Result<Vec<Planned>, PlanError> {
    let default_skip = config.on_conflict == Some(OnConflict::Skip);
    let mut planned = Vec::new();
    for repo in &config.repositories {
        let name = repo.name.clone().unwrap_or_else(|| repo.url.clone());
        if only.is_some_and(|only| only != name) {
            continue;
        }
        let base_name = repo
            .base
            .clone()
            .unwrap_or_else(|| config.odoo_version.clone());
        let mut steps = Vec::with_capacity(repo.merges.len());
        for merge in &repo.merges {
            let source = merge.source().map_err(|message| PlanError::InvalidMerge {
                repository: name.clone(),
                message,
            })?;
            let (remote, source, label) = match source {
                MergeSource::Pr(pr) => (None, Source::Pr(pr), format!("#{pr}")),
                MergeSource::Sha(sha) => (
                    None,
                    Source::Commit(sha.to_owned()),
                    format!("commit {sha}"),
                ),
                MergeSource::Branch { branch, remote } => (
                    remote.map(str::to_owned),
                    Source::Branch(branch.to_owned()),
                    format!("branch {branch}"),
                ),
            };
            steps.push(PlannedStep {
                label,
                from: PlannedSource { remote, source },
                skip_on_conflict: merge
                    .on_conflict
                    .map_or(default_skip, |c| c == OnConflict::Skip),
                lock: LockSource::of(merge),
                on_head_change: merge
                    .on_head_change
                    .or(config.on_head_change)
                    .unwrap_or_default(),
            });
        }
        planned.push(Planned {
            target_dir: target_dir(config, root, repo, &name),
            forge: repo.forge.map_or_else(|| Forge::detect(&repo.url), forge),
            url: repo.url.clone(),
            base: PlannedSource {
                remote: None,
                source: Source::Branch(base_name.clone()),
            },
            base_name,
            steps,
            pinned: false,
            name,
        });
    }
    match only {
        Some(only) if planned.is_empty() => Err(PlanError::UnknownRepository(only.to_owned())),
        _ => Ok(planned),
    }
}

/// Replace the sources of `planned` by the commits of its entry in `locked`.
///
/// Only an entry locked from the same configuration counts: the same name,
/// URL (in any spelling), base and merges in the same order, and no merge
/// skipped at lock time that may no longer be skipped. Returns whether it
/// was pinned.
pub fn pin(planned: &mut Planned, locked: &[LockedRepository]) -> bool {
    let Some(entry) = locked_entry(planned, locked) else {
        return false;
    };
    planned.base = PlannedSource {
        remote: None,
        source: Source::Commit(entry.base_sha.clone()),
    };
    for (step, merge) in planned.steps.iter_mut().zip(&entry.merges) {
        step.from.source = Source::Commit(merge.sha.clone());
    }
    planned.pinned = true;
    true
}

/// The entry of `locked` that was locked from the configuration of
/// `planned`, as [`pin`] decides.
pub fn locked_entry<'a>(
    planned: &Planned,
    locked: &'a [LockedRepository],
) -> Option<&'a LockedRepository> {
    let key = repo_key(&planned.url);
    locked.iter().find(|entry| {
        repo_key(&entry.url) == key
            && entry.name == planned.name
            && entry.base == planned.base_name
            && entry.merges.len() == planned.steps.len()
            && entry
                .merges
                .iter()
                .zip(&planned.steps)
                .all(|(merge, step)| {
                    step.lock == merge.source
                        && (merge.status != MergeStatus::Skipped || step.skip_on_conflict)
                })
    })
}
