use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::join_all;
use odx_core::repo_key;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{Cache, FetchPlan, Fetched, GitBackend, GitError, fetch};

/// Concurrency and waiting limits for [`fetch_all`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Fetches running at once in total (at least 1).
    pub jobs: usize,
    /// Fetches running at once per host (at least 1).
    pub per_host: usize,
    /// How long to wait for another process's repository lock.
    pub lock_timeout: Duration,
}

/// The result of fetching one plan.
#[derive(Debug)]
pub struct Outcome {
    /// The plan's repository URL.
    pub url: String,
    /// What was fetched, or why not.
    pub result: Result<Vec<Fetched>, GitError>,
}

/// The host part of a repository URL, for per-host limits.
fn host(url: &str) -> String {
    let key = repo_key(url);
    key.split('/').next().unwrap_or_default().to_owned()
}

/// Fetch every plan, concurrently within `limits`, and return one outcome
/// per plan in the order given.
///
/// Git work runs on blocking threads. Cancelling `cancel` stops plans that
/// have not started yet (they end with [`GitError::Cancelled`]); running
/// fetches finish, so git is never interrupted halfway through a ref update.
pub async fn fetch_all(
    cache: &Cache,
    backend: &Arc<dyn GitBackend>,
    plans: Vec<FetchPlan>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Vec<Outcome> {
    let jobs = Arc::new(Semaphore::new(limits.jobs.max(1)));
    let mut hosts: HashMap<String, Arc<Semaphore>> = HashMap::new();
    let mut tasks = Vec::new();
    for plan in plans {
        let per_host = Arc::clone(
            hosts
                .entry(host(&plan.url))
                .or_insert_with(|| Arc::new(Semaphore::new(limits.per_host.max(1)))),
        );
        let jobs = Arc::clone(&jobs);
        let cache = cache.clone();
        let backend = Arc::clone(backend);
        tasks.push(async move {
            let url = plan.url.clone();
            let permits = tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                permits = async { (jobs.acquire_owned().await, per_host.acquire_owned().await) } => Some(permits),
            };
            let result = match permits {
                Some(permits) => tokio::task::spawn_blocking(move || {
                    let _permits = permits;
                    fetch(&cache, backend.as_ref(), &plan, limits.lock_timeout)
                })
                .await
                .unwrap_or_else(|e| Err(GitError::Io(io::Error::other(e)))),
                None => Err(GitError::Cancelled),
            };
            Outcome { url, result }
        });
    }
    join_all(tasks).await
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
