use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use odx_core::system::OsRunner;

use super::*;

/// The lock timeout for fetches that do not test locking.
const WAIT: Duration = Duration::from_secs(5);
use crate::{RepoLock, SystemGit};

/// Run git in `dir` and return its trimmed standard output.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "odx")
        .env("GIT_AUTHOR_EMAIL", "odx@example.com")
        .env("GIT_COMMITTER_NAME", "odx")
        .env("GIT_COMMITTER_EMAIL", "odx@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

/// An upstream repository with a base branch, a feature branch, a GitHub
/// style and a GitLab style pull request ref.
struct Upstream {
    dir: PathBuf,
    base: String,
    feature: String,
    pull: String,
    merge_request: String,
}

fn upstream(root: &Path, name: &str) -> Upstream {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet", "--initial-branch=17.0"]);
    let commit = |file: &str| {
        fs::write(dir.join(file), format!("{name}:{file}")).unwrap();
        git(&dir, &["add", file]);
        git(&dir, &["commit", "--quiet", "-m", file]);
        git(&dir, &["rev-parse", "HEAD"])
    };
    let base = commit("base.txt");
    git(&dir, &["checkout", "--quiet", "-b", "17.0-fix"]);
    let feature = commit("fix.txt");
    let pull = commit("pull.txt");
    git(&dir, &["update-ref", "refs/pull/1/head", &pull]);
    let merge_request = commit("mr.txt");
    git(
        &dir,
        &["update-ref", "refs/merge-requests/2/head", &merge_request],
    );
    git(&dir, &["update-ref", "refs/heads/17.0-fix", &feature]);
    git(&dir, &["checkout", "--quiet", "17.0"]);
    Upstream {
        dir,
        base,
        feature,
        pull,
        merge_request,
    }
}

fn system() -> SystemGit {
    SystemGit::new(Arc::new(OsRunner))
}

#[test]
fn fetches_branches_pull_requests_and_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let cache = Cache::new(tmp.path().join("cache"));
    let url = file_url(&up.dir);
    let plan = FetchPlan {
        url: url.clone(),
        forge: Forge::Github,
        sources: vec![
            (None, Source::Branch("17.0".into())),
            (None, Source::Pr(1)),
            (None, Source::Commit(up.feature[..10].to_owned())),
            (None, Source::Commit(up.base[..8].to_owned())),
            (None, Source::Commit(up.merge_request.clone())),
        ],
        mirror: None,
    };
    let fetched = fetch(&cache, &system(), &plan, WAIT).unwrap();
    let shas: Vec<&str> = fetched.iter().map(|f| f.sha.as_str()).collect();
    assert_eq!(
        shas,
        [
            up.base.as_str(),
            up.pull.as_str(),
            up.feature.as_str(),
            up.base.as_str(),
            up.merge_request.as_str()
        ]
    );
    assert_eq!(
        fs::read_to_string(cache.url_file(&url)).unwrap(),
        format!("{url}\n")
    );

    // A second fetch reuses the repository and follows a moved ref.
    git(
        &up.dir,
        &["update-ref", "refs/pull/1/head", &up.merge_request],
    );
    let again = fetch(&cache, &system(), &plan, WAIT).unwrap();
    assert_eq!(again[1].sha, up.merge_request);
}

#[test]
fn gitlab_and_gitea_layouts() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "web");
    let cache = Cache::new(tmp.path().join("cache"));
    let url = file_url(&up.dir);
    let gitlab = FetchPlan {
        url: url.clone(),
        forge: Forge::Gitlab,
        sources: vec![(None, Source::Pr(2))],
        mirror: None,
    };
    assert_eq!(
        fetch(&cache, &system(), &gitlab, WAIT).unwrap()[0].sha,
        up.merge_request
    );
    let gitea = FetchPlan {
        url,
        forge: Forge::Gitea,
        sources: vec![(None, Source::Pr(1))],
        mirror: None,
    };
    assert_eq!(
        fetch(&cache, &system(), &gitea, WAIT).unwrap()[0].sha,
        up.pull
    );
}

/// Counts fetches per URL while delegating to the system backend.
#[derive(Debug)]
struct Counting {
    inner: SystemGit,
    urls: Mutex<Vec<String>>,
}

impl GitBackend for Counting {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        self.inner.init_bare(dir)
    }

    fn fetch(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError> {
        self.urls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(url.to_owned());
        self.inner.fetch(dir, url, refspecs)
    }

    fn resolve(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
        self.inner.resolve(dir, rev)
    }
}

#[test]
fn one_fetch_per_remote_url() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let fork = upstream(tmp.path(), "fork");
    let cache = Cache::new(tmp.path().join("cache"));
    let fork_url = file_url(&fork.dir);
    let plan = FetchPlan {
        url: file_url(&up.dir),
        forge: Forge::Github,
        sources: vec![
            (None, Source::Branch("17.0".into())),
            (Some(fork_url.clone()), Source::Branch("17.0-fix".into())),
            (None, Source::Pr(1)),
            (Some(fork_url.clone()), Source::Pr(1)),
        ],
        mirror: None,
    };
    let backend = Counting {
        inner: system(),
        urls: Mutex::new(Vec::new()),
    };
    let fetched = fetch(&cache, &backend, &plan, WAIT).unwrap();
    assert_eq!(*backend.urls.lock().unwrap(), [plan.url, fork_url]);
    assert_eq!(fetched[1].sha, fork.feature);
    assert_eq!(fetched[3].sha, fork.pull);
    assert_ne!(
        fetched[1].sha, up.feature,
        "the fork's branch is distinct from upstream's"
    );
}

#[test]
fn errors() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let cache = Cache::new(tmp.path().join("cache"));
    let missing = FetchPlan {
        url: file_url(&up.dir),
        forge: Forge::Github,
        sources: vec![(None, Source::Pr(99))],
        mirror: None,
    };
    let err = fetch(&cache, &system(), &missing, WAIT).unwrap_err();
    assert!(err.to_string().starts_with("git fetch failed: "), "{err}");

    let unreachable = FetchPlan {
        url: file_url(&tmp.path().join("nope")),
        ..missing.clone()
    };
    assert!(matches!(
        fetch(&cache, &system(), &unreachable, WAIT),
        Err(GitError::Failed { .. })
    ));

    let no_git = SystemGit::with_program(Arc::new(OsRunner), "odx-surely-missing-git");
    let fresh = Cache::new(tmp.path().join("fresh"));
    let err = fetch(&fresh, &no_git, &missing, WAIT).unwrap_err();
    assert!(err.to_string().starts_with("cannot run git: "), "{err}");

    let _held = RepoLock::acquire(&cache.lock_file(&missing.url), Duration::ZERO).unwrap();
    let locked = fetch(&cache, &system(), &missing, Duration::ZERO).unwrap_err();
    assert!(
        locked
            .to_string()
            .ends_with("is locked by another odx process"),
        "{locked}"
    );
}

/// A backend whose fetches succeed but never produce the ref.
#[derive(Debug)]
struct Hollow;

impl GitBackend for Hollow {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        fs::create_dir_all(dir)?;
        fs::write(dir.join("HEAD"), "ref: refs/heads/main\n")?;
        Ok(())
    }

    fn fetch(&self, _: &Path, _: &str, _: &[String]) -> Result<(), GitError> {
        Ok(())
    }

    fn resolve(&self, _: &Path, _: &str) -> Result<Option<String>, GitError> {
        Ok(None)
    }
}

#[test]
fn unresolved_sources_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path());
    let plan = FetchPlan {
        url: "https://x/y.git".into(),
        forge: Forge::Github,
        sources: vec![(None, Source::Pr(1))],
        mirror: None,
    };
    let err = fetch(&cache, &Hollow, &plan, Duration::ZERO).unwrap_err();
    assert_eq!(
        err.to_string(),
        "git resolve failed: #1 did not resolve to a commit after fetching"
    );
    assert!(Hollow.fetch(Path::new("."), "u", &[]).is_ok());
}

#[test]
fn url_file_write_errors_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = Cache::new(tmp.path());
    let plan = FetchPlan {
        url: "https://x/y.git".into(),
        forge: Forge::Github,
        sources: vec![],
        mirror: None,
    };
    fs::create_dir_all(cache.url_file(&plan.url)).unwrap();
    assert!(matches!(
        fetch(&cache, &Hollow, &plan, Duration::ZERO),
        Err(GitError::Io(_))
    ));
}

/// A backend whose resolve fails outright.
#[derive(Debug)]
struct Broken;

impl GitBackend for Broken {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        Hollow.init_bare(dir)
    }

    fn fetch(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError> {
        Hollow.fetch(dir, url, refspecs)
    }

    fn resolve(&self, _: &Path, _: &str) -> Result<Option<String>, GitError> {
        Err(GitError::Io(std::io::Error::from(
            std::io::ErrorKind::Interrupted,
        )))
    }
}

#[test]
fn resolve_failures_propagate() {
    let tmp = tempfile::tempdir().unwrap();
    let plan = FetchPlan {
        url: "https://x/y.git".into(),
        forge: Forge::Github,
        sources: vec![(None, Source::Pr(1))],
        mirror: None,
    };
    let err = fetch(&Cache::new(tmp.path()), &Broken, &plan, Duration::ZERO).unwrap_err();
    assert!(err.to_string().starts_with("cannot run git"), "{err}");
    let no_git = SystemGit::with_program(Arc::new(OsRunner), "odx-surely-missing-git");
    assert!(no_git.resolve(tmp.path(), "HEAD").is_err());
}

/// A bare clone of `up` that only has the base branch and one vaulted commit.
fn mirror_of(root: &Path, up: &Upstream) -> PathBuf {
    let mirror = root.join("mirror.git");
    git(
        root,
        &["init", "--quiet", "--bare", mirror.to_str().unwrap()],
    );
    git(
        &up.dir,
        &[
            "push",
            "--quiet",
            mirror.to_str().unwrap(),
            &format!("{}:refs/odx/{}", up.pull, up.pull),
        ],
    );
    mirror
}

#[test]
fn locked_commits_come_from_the_mirror_first() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let mirror = mirror_of(tmp.path(), &up);
    let cache = Cache::new(tmp.path().join("cache"));
    // The upstream URL is unreachable: only the mirror can provide the commit.
    let plan = FetchPlan {
        url: file_url(&tmp.path().join("gone")),
        forge: Forge::Github,
        sources: vec![(None, Source::Commit(up.pull.clone()))],
        mirror: Some(file_url(&mirror)),
    };
    let backend = Counting {
        inner: system(),
        urls: Mutex::new(Vec::new()),
    };
    let fetched = fetch(&cache, &backend, &plan, WAIT).unwrap();
    assert_eq!(
        (fetched[0].sha.as_str(), fetched[0].via),
        (up.pull.as_str(), Via::Mirror)
    );
    assert_eq!(*backend.urls.lock().unwrap(), [file_url(&mirror)]);
}

#[test]
fn commits_the_mirror_lacks_come_from_upstream() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let mirror = mirror_of(tmp.path(), &up);
    let cache = Cache::new(tmp.path().join("cache"));
    let plan = FetchPlan {
        url: file_url(&up.dir),
        forge: Forge::Github,
        sources: vec![
            (None, Source::Commit(up.merge_request.clone())),
            (None, Source::Branch("17.0".into())),
        ],
        mirror: Some(file_url(&mirror)),
    };
    let fetched = fetch(&cache, &system(), &plan, WAIT).unwrap();
    assert_eq!(fetched[0].via, Via::Upstream);
    assert_eq!(fetched[0].sha, up.merge_request);

    // A mirror with nothing to offer for branches is not contacted.
    let branches_only = FetchPlan {
        sources: vec![(None, Source::Branch("17.0".into()))],
        ..plan
    };
    let backend = Counting {
        inner: system(),
        urls: Mutex::new(Vec::new()),
    };
    fetch(&cache, &backend, &branches_only, WAIT).unwrap();
    assert_eq!(*backend.urls.lock().unwrap(), [file_url(&up.dir)]);
}
