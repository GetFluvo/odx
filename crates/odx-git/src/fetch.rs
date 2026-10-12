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
}

/// Fetch every source of `plan` into the cache, with one `git fetch` per
/// remote URL, under the repository's advisory lock.
///
/// The bare cache repository is created on first use. Every source must
/// resolve to a commit afterwards.
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
    {
        if !dir.join("HEAD").exists() {
            backend.init_bare(&dir)?;
            fs::write(cache.url_file(&plan.url), format!("{}\n", plan.url))?;
        }
        let mut remotes: Vec<Option<&str>> = Vec::new();
        for (remote, _) in &plan.sources {
            let remote = remote.as_deref();
            if !remotes.contains(&remote) {
                remotes.push(remote);
            }
        }
        for remote in remotes {
            let mut refspecs: Vec<String> = plan
                .sources
                .iter()
                .filter(|(r, _)| r.as_deref() == remote)
                .map(|(_, source)| source.refspec(plan.forge, remote))
                .collect();
            refspecs.sort();
            refspecs.dedup();
            backend.fetch(&dir, remote.unwrap_or(&plan.url), &refspecs)?;
        }
        plan.sources
            .iter()
            .map(|(remote, source)| {
                let local = source.local_ref(plan.forge, remote.as_deref());
                let sha = backend
                    .resolve(&dir, &local)?
                    .ok_or_else(|| GitError::Failed {
                        command: "resolve".to_owned(),
                        stderr: format!("{source} did not resolve to a commit after fetching"),
                    })?;
                Ok(Fetched {
                    remote: remote.clone(),
                    source: source.clone(),
                    sha,
                })
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
