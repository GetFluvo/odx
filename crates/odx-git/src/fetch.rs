use std::fs;
use std::time::Duration;

use crate::lock::RepoLock;
use crate::{Cache, Forge, GitBackend, GitError, Source};

/// What to fetch for one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPlan {
    /// The repository URL; the cache entry is keyed by it.
    pub url: String,
    /// How the forge names pull request refs.
    pub forge: Forge,
    /// Sources, each with the remote URL it lives on when that is not `url`.
    pub sources: Vec<(Option<String>, Source)>,
    /// A private mirror to try first for full commit ids (`--prefer-mirror`).
    pub mirror: Option<String>,
}

/// Where a fetched source came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The repository URL or the source's own remote.
    Upstream,
    /// The private mirror.
    Mirror,
}

/// A fetched source and the commit it resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    /// The remote URL, when not the repository URL.
    pub remote: Option<String>,
    /// What was fetched.
    pub source: Source,
    /// The commit.
    pub sha: String,
    /// Where it came from.
    pub via: Via,
}

/// Fetch every source of `plan` into the cache, with one `git fetch` per
/// remote URL, under the repository's advisory lock.
///
/// With a mirror, full commit ids of the repository are fetched from the
/// mirror first, in one fetch; when the mirror lacks any of them, they are
/// fetched upstream instead. The bare cache repository is created on first
/// use. Every source must resolve to a commit afterwards.
///
/// # Errors
///
/// [`GitError::Locked`] when another process keeps the repository locked
/// past `lock_timeout`, [`GitError::Failed`] when git fails (unreachable
/// remote, missing ref), and [`GitError::Failed`] with command `resolve`
/// when a fetched source does not resolve to a commit.
pub fn fetch(
    cache: &Cache,
    backend: &dyn GitBackend,
    plan: &FetchPlan,
    lock_timeout: Duration,
) -> Result<Vec<Fetched>, GitError> {
    let dir = cache.repo_dir(&plan.url);
    let _lock = RepoLock::acquire(&cache.lock_file(&plan.url), lock_timeout)?;
    if !dir.join("HEAD").exists() {
        backend.init_bare(&dir)?;
        fs::write(cache.url_file(&plan.url), format!("{}\n", plan.url))?;
    }
    let mirrored = plan
        .mirror
        .as_deref()
        .map(|mirror| fetch_from_mirror(backend, &dir, plan, mirror))
        .unwrap_or_default();
    let mut remotes: Vec<Option<&str>> = plan.sources.iter().map(|(r, _)| r.as_deref()).collect();
    remotes.sort_unstable();
    remotes.dedup();
    for remote in remotes {
        let mut refspecs: Vec<String> = plan
            .sources
            .iter()
            .enumerate()
            .filter(|(i, (r, _))| r.as_deref() == remote && !mirrored.contains(i))
            .map(|(_, (_, source))| source.refspec(plan.forge, remote))
            .collect();
        refspecs.sort();
        refspecs.dedup();
        if !refspecs.is_empty() {
            backend.fetch(&dir, remote.unwrap_or(&plan.url), &refspecs)?;
        }
    }
    plan.sources
        .iter()
        .enumerate()
        .map(|(i, (remote, source))| {
            let local = source.local_ref(plan.forge, remote.as_deref());
            let sha = backend
                .resolve(&dir, &local)?
                .ok_or_else(|| GitError::Failed {
                    command: "resolve".to_owned(),
                    stderr: format!("{source} did not resolve to a commit after fetching"),
                })?;
            let via = if mirrored.contains(&i) {
                Via::Mirror
            } else {
                Via::Upstream
            };
            Ok(Fetched {
                remote: remote.clone(),
                source: source.clone(),
                sha,
                via,
            })
        })
        .collect()
}

/// Fetch the plan's full commit ids from `mirror` in one fetch; returns the
/// indices of the sources it provided (none when the fetch failed).
fn fetch_from_mirror(
    backend: &dyn GitBackend,
    dir: &std::path::Path,
    plan: &FetchPlan,
    mirror: &str,
) -> Vec<usize> {
    let commits: Vec<usize> = plan
        .sources
        .iter()
        .enumerate()
        .filter(|(_, (remote, source))| remote.is_none() && source.is_full_commit())
        .map(|(i, _)| i)
        .collect();
    let refspecs: Vec<String> = commits
        .iter()
        .filter_map(|i| plan.sources.get(*i))
        .map(|(_, source)| source.refspec(plan.forge, None))
        .collect();
    if refspecs.is_empty() || backend.fetch(dir, mirror, &refspecs).is_err() {
        return Vec::new();
    }
    commits
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
