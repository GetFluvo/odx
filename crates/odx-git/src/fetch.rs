use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::lock::RepoLock;
use crate::{Cache, Forge, GitBackend, GitError, LocalClone, Source};

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
    /// A private mirror to fetch full commit ids from when the repository
    /// no longer has them (force-pushed and collected upstream).
    pub fallback: Option<String>,
    /// A local clone of the repository to borrow objects from.
    pub clone: Option<LocalClone>,
    /// Serve every source from the cache, without network, when the cache
    /// repository was fetched within this long and holds them all; `None`
    /// always fetches.
    pub max_age: Option<Duration>,
}

/// Where a fetched source came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The repository URL or the source's own remote.
    Upstream,
    /// The private mirror.
    Mirror,
    /// A local clone, which was brought up to date first (update mode).
    Clone,
    /// Nothing was fetched: the commit was already in the cache.
    Cache,
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
    check_alternates(&dir)?;
    if let Some(fresh) = from_cache(cache, backend, &dir, plan) {
        return Ok(fresh);
    }
    if let Some(clone) = &plan.clone {
        borrow_from(&dir, clone)?;
    }
    let cached = cached_commits(backend, &dir, plan);
    let mirrored = plan
        .mirror
        .as_deref()
        .map(|mirror| fetch_from_mirror(backend, &dir, plan, mirror, &cached))
        .unwrap_or_default();
    let through_clone = match &plan.clone {
        Some(clone) if clone.update => fetch_through_clone(backend, &dir, plan, clone)?,
        _ => Vec::new(),
    };
    let mut vaulted: Vec<usize> = Vec::new();
    let mut remotes: Vec<Option<&str>> = plan.sources.iter().map(|(r, _)| r.as_deref()).collect();
    remotes.sort_unstable();
    remotes.dedup();
    for remote in remotes {
        let mut refspecs: Vec<String> = plan
            .sources
            .iter()
            .enumerate()
            .filter(|(i, (r, _))| {
                r.as_deref() == remote
                    && !cached.contains(i)
                    && !mirrored.contains(i)
                    && !through_clone.contains(i)
            })
            .map(|(_, (_, source))| source.refspec(plan.forge, remote))
            .collect();
        refspecs.sort();
        refspecs.dedup();
        if refspecs.is_empty() {
            continue;
        }
        if let Err(upstream) = backend.fetch(&dir, remote.unwrap_or(&plan.url), &refspecs) {
            // Locked commits that are gone upstream are still in the vault.
            let rescued = plan
                .fallback
                .as_deref()
                .filter(|_| remote.is_none())
                .is_some_and(|mirror| backend.fetch(&dir, mirror, &refspecs).is_ok());
            if !rescued {
                return Err(upstream);
            }
            vaulted.extend(
                plan.sources
                    .iter()
                    .enumerate()
                    .filter(|(i, (r, _))| {
                        r.is_none()
                            && !cached.contains(i)
                            && !mirrored.contains(i)
                            && !through_clone.contains(i)
                    })
                    .map(|(i, _)| i),
            );
        }
    }
    let fetched: Result<Vec<Fetched>, GitError> = plan
        .sources
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
            let via = if cached.contains(&i) {
                Via::Cache
            } else if mirrored.contains(&i) || vaulted.contains(&i) {
                Via::Mirror
            } else if through_clone.contains(&i) {
                Via::Clone
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
        .collect();
    fetched.inspect(|_| stamp(cache, &plan.url))
}

/// Record that the repository at `url` was just fetched. Without the stamp
/// the next run fetches again, so a failure to write it loses nothing.
fn stamp(cache: &Cache, url: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = fs::write(cache.fetched_file(url), format!("{now}\n"));
}

/// Every source of `plan` from the cache, when the plan allows that and
/// the repository was fetched within its `max_age` and holds them all.
fn from_cache(
    cache: &Cache,
    backend: &dyn GitBackend,
    dir: &Path,
    plan: &FetchPlan,
) -> Option<Vec<Fetched>> {
    let max_age = plan.max_age?;
    let stamp = fs::read_to_string(cache.fetched_file(&plan.url)).ok()?;
    let fetched = UNIX_EPOCH + Duration::from_secs(stamp.trim().parse().ok()?);
    if SystemTime::now().duration_since(fetched).ok()? > max_age {
        return None;
    }
    plan.sources
        .iter()
        .map(|(remote, source)| {
            let local = source.local_ref(plan.forge, remote.as_deref());
            Some(Fetched {
                remote: remote.clone(),
                source: source.clone(),
                sha: backend.resolve(dir, &local).ok().flatten()?,
                via: Via::Cache,
            })
        })
        .collect()
}

/// The indices of full commit ids already in the cache: commits are
/// immutable, so one fetched before needs no network.
fn cached_commits(backend: &dyn GitBackend, dir: &Path, plan: &FetchPlan) -> Vec<usize> {
    plan.sources
        .iter()
        .enumerate()
        .filter(|(_, (_, source))| {
            source.is_full_commit()
                && matches!(
                    backend.resolve(dir, &source.local_ref(plan.forge, None)),
                    Ok(Some(_))
                )
        })
        .map(|(i, _)| i)
        .collect()
}

/// The alternates file of a repository.
fn alternates_file(dir: &Path) -> std::path::PathBuf {
    dir.join("objects").join("info").join("alternates")
}

/// Fail when an alternate object directory has disappeared: the cache
/// repository would be missing objects its refs point at.
fn check_alternates(dir: &Path) -> Result<(), GitError> {
    let listed = fs::read_to_string(alternates_file(dir)).unwrap_or_default();
    listed
        .lines()
        .find(|line| !Path::new(line).is_dir())
        .map_or(Ok(()), |gone| {
            Err(GitError::Failed {
                command: "alternates".to_owned(),
                stderr: format!(
                    "{} borrows objects from {gone}, which no longer exists; delete {} to rebuild it",
                    dir.display(),
                    dir.display()
                ),
            })
        })
}

/// List the clone's object directory as an alternate of the cache repository.
fn borrow_from(dir: &Path, clone: &LocalClone) -> Result<(), GitError> {
    let objects = fs::canonicalize(clone.git_dir.join("objects"))?;
    let objects = objects.to_string_lossy();
    // Windows canonical paths carry a `\\?\` prefix git cannot read.
    let line = format!("{}\n", objects.strip_prefix(r"\\?\").unwrap_or(&objects));
    let file = alternates_file(dir);
    if fs::read_to_string(&file).unwrap_or_default() != line {
        fs::create_dir_all(file.with_file_name(""))?;
        fs::write(file, line)?;
    }
    Ok(())
}

/// Update mode: fetch the plan's base branches into the clone's
/// remote-tracking refs, then point the cache's refs at them (the objects
/// are already shared through the alternate). Returns the indices served.
fn fetch_through_clone(
    backend: &dyn GitBackend,
    dir: &Path,
    plan: &FetchPlan,
    clone: &LocalClone,
) -> Result<Vec<usize>, GitError> {
    let branches: Vec<(usize, &str)> = plan
        .sources
        .iter()
        .enumerate()
        .filter_map(|(i, (remote, source))| match (remote, source) {
            (None, Source::Branch(branch)) => Some((i, branch.as_str())),
            _ => None,
        })
        .collect();
    if branches.is_empty() {
        return Ok(Vec::new());
    }
    let tracking = |branch: &str| format!("refs/remotes/{}/{branch}", clone.remote);
    let into_clone: Vec<String> = branches
        .iter()
        .map(|(_, b)| format!("+refs/heads/{b}:{}", tracking(b)))
        .collect();
    backend.fetch(&clone.git_dir, &clone.remote, &into_clone)?;
    let into_cache: Vec<String> = branches
        .iter()
        .map(|(_, b)| {
            format!(
                "+{}:{}",
                tracking(b),
                Source::Branch((*b).to_owned()).local_ref(plan.forge, None)
            )
        })
        .collect();
    backend.fetch(dir, &clone.git_dir.to_string_lossy(), &into_cache)?;
    Ok(branches.into_iter().map(|(i, _)| i).collect())
}

/// Fetch the plan's full commit ids from `mirror` in one fetch; returns the
/// indices of the sources it provided (none when the fetch failed).
fn fetch_from_mirror(
    backend: &dyn GitBackend,
    dir: &Path,
    plan: &FetchPlan,
    mirror: &str,
    cached: &[usize],
) -> Vec<usize> {
    let commits: Vec<usize> = plan
        .sources
        .iter()
        .enumerate()
        .filter(|(i, (remote, source))| {
            remote.is_none() && source.is_full_commit() && !cached.contains(i)
        })
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
