use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::{Forge, Source};

/// A backend that sleeps in `fetch` and records the peak concurrency.
#[derive(Debug, Default)]
struct Slow {
    running: AtomicUsize,
    peak: AtomicUsize,
    calls: Mutex<Vec<String>>,
}

impl GitBackend for Slow {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n")?;
        Ok(())
    }

    fn fetch(&self, _: &Path, url: &str, _: &[String]) -> Result<(), GitError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        self.running.fetch_sub(1, Ordering::SeqCst);
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(url.to_owned());
        Ok(())
    }

    fn resolve(&self, _: &Path, _: &str) -> Result<Option<String>, GitError> {
        Ok(Some("0".repeat(40)))
    }
}

fn plan(url: &str) -> FetchPlan {
    FetchPlan {
        url: url.to_owned(),
        forge: Forge::Github,
        sources: vec![(None, Source::Pr(1))],
        mirror: None,
        clone: None,
    }
}

fn limits(jobs: usize, per_host: usize) -> Limits {
    Limits {
        jobs,
        per_host,
        lock_timeout: Duration::from_secs(5),
    }
}

async fn run(
    backend: &Arc<Slow>,
    plans: Vec<FetchPlan>,
    limits: Limits,
    cancel: &CancellationToken,
) -> Vec<Outcome> {
    let tmp = tempfile::tempdir().unwrap();
    let dynamic: Arc<dyn GitBackend> = Arc::clone(backend) as Arc<dyn GitBackend>;
    fetch_all(&Cache::new(tmp.path()), &dynamic, plans, limits, cancel).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn results_keep_plan_order_and_respect_jobs() {
    let backend = Arc::new(Slow::default());
    let plans = (0..6)
        .map(|i| plan(&format!("https://h{i}.example/r.git")))
        .collect();
    let outcomes = run(&backend, plans, limits(2, 6), &CancellationToken::new()).await;
    let urls: Vec<_> = outcomes.iter().map(|o| o.url.as_str()).collect();
    assert_eq!(
        urls,
        (0..6)
            .map(|i| format!("https://h{i}.example/r.git"))
            .collect::<Vec<_>>()
    );
    assert!(outcomes.iter().all(|o| o.result.is_ok()));
    assert_eq!(backend.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_host_limit() {
    let backend = Arc::new(Slow::default());
    let plans = vec![
        plan("https://github.com/OCA/pos.git"),
        plan("git@github.com:OCA/web.git"),
        plan("https://github.com/OCA/crm.git"),
    ];
    let outcomes = run(&backend, plans, limits(8, 1), &CancellationToken::new()).await;
    assert_eq!(outcomes.len(), 3);
    assert_eq!(backend.peak.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zero_limits_still_make_progress() {
    let backend = Arc::new(Slow::default());
    let outcomes = run(
        &backend,
        vec![plan("https://x/y.git")],
        limits(0, 0),
        &CancellationToken::new(),
    )
    .await;
    assert!(outcomes[0].result.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_before_start() {
    let backend = Arc::new(Slow::default());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcomes = run(
        &backend,
        vec![plan("https://x/a.git"), plan("https://x/b.git")],
        limits(2, 2),
        &cancel,
    )
    .await;
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o.result, Err(GitError::Cancelled)))
    );
    assert_eq!(GitError::Cancelled.to_string(), "cancelled");
    assert_eq!(backend.calls.lock().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_stops_waiting_plans_but_lets_running_ones_finish() {
    let backend = Arc::new(Slow::default());
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        trigger.cancel();
    });
    let outcomes = run(
        &backend,
        vec![plan("https://x/a.git"), plan("https://x/b.git")],
        limits(1, 1),
        &cancel,
    )
    .await;
    assert!(outcomes[0].result.is_ok());
    assert!(matches!(outcomes[1].result, Err(GitError::Cancelled)));
}

/// A backend that panics while fetching.
#[derive(Debug)]
struct Panicky;

impl GitBackend for Panicky {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        Slow::default().init_bare(dir)
    }

    fn fetch(&self, _: &Path, _: &str, _: &[String]) -> Result<(), GitError> {
        panic!("backend bug");
    }

    fn resolve(&self, _: &Path, _: &str) -> Result<Option<String>, GitError> {
        Ok(None)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_backend_becomes_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let backend: Arc<dyn GitBackend> = Arc::new(Panicky);
    assert_eq!(Panicky.resolve(Path::new("."), "x").unwrap(), None);
    let outcomes = fetch_all(
        &Cache::new(tmp.path()),
        &backend,
        vec![plan("https://x/y.git")],
        limits(1, 1),
        &CancellationToken::new(),
    )
    .await;
    assert!(
        outcomes[0]
            .result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("backend bug")
    );
}

#[test]
fn hosts() {
    assert_eq!(host("git@github.com:OCA/pos.git"), "github.com");
    assert_eq!(host("https://GitLab.example.com/x/y"), "gitlab.example.com");
}
