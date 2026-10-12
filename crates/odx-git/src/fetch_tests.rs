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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
        fallback: None,
        clone: None,
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
fn cached_commits_are_not_fetched_again() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let mirror = mirror_of(tmp.path(), &up);
    let cache = Cache::new(tmp.path().join("cache"));
    let plan = FetchPlan {
        url: file_url(&up.dir),
        forge: Forge::Github,
        sources: vec![(None, Source::Commit(up.pull.clone()))],
        mirror: Some(file_url(&mirror)),
        fallback: None,
        clone: None,
    };
    fetch(&cache, &system(), &plan, WAIT).unwrap();
    let backend = Counting {
        inner: system(),
        urls: Mutex::new(Vec::new()),
    };
    let fetched = fetch(&cache, &backend, &plan, WAIT).unwrap();
    assert_eq!(fetched[0].via, Via::Cache);
    assert!(backend.urls.lock().unwrap().is_empty());
}

#[test]
fn commits_gone_upstream_come_from_the_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let mirror = mirror_of(tmp.path(), &up);
    let cache = Cache::new(tmp.path().join("cache"));
    // The upstream lost the commit (force-push and gc): its URL fails here.
    let plan = FetchPlan {
        url: file_url(&tmp.path().join("gone")),
        forge: Forge::Github,
        sources: vec![(None, Source::Commit(up.pull.clone()))],
        mirror: None,
        fallback: Some(file_url(&mirror)),
        clone: None,
    };
    let fetched = fetch(&cache, &system(), &plan, WAIT).unwrap();
    assert_eq!(
        (fetched[0].sha.as_str(), fetched[0].via),
        (up.pull.as_str(), Via::Mirror)
    );

    // The vault does not have it either: the upstream error is reported.
    let missing = FetchPlan {
        sources: vec![(None, Source::Commit(up.merge_request.clone()))],
        ..plan.clone()
    };
    let err = fetch(&cache, &system(), &missing, WAIT).unwrap_err();
    assert!(err.to_string().contains("gone"), "{err}");
    // Other remotes are not looked up in the vault.
    let fork = FetchPlan {
        sources: vec![(
            Some(file_url(&tmp.path().join("gone-fork"))),
            Source::Commit(up.pull.clone()),
        )],
        url: file_url(&up.dir),
        ..plan
    };
    let other = Cache::new(tmp.path().join("other-cache"));
    assert!(fetch(&other, &system(), &fork, WAIT).is_err());
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
        fallback: None,
        clone: None,
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

/// Objects stored in the repository itself (not borrowed through alternates).
fn own_objects(dir: &Path) -> usize {
    let out = git(dir, &["count-objects", "-v"]);
    out.lines()
        .filter_map(|line| line.split_once(": "))
        .filter(|(key, _)| *key == "count" || *key == "in-pack")
        .map(|(_, value)| value.parse::<usize>().unwrap())
        .sum()
}

fn clone_of(root: &Path, up: &Upstream, update: bool) -> LocalClone {
    let clones = root.join("clones");
    fs::create_dir_all(&clones).unwrap();
    git(&clones, &["clone", "--quiet", &file_url(&up.dir), "pos"]);
    LocalClone {
        git_dir: clones.join("pos").join(".git"),
        remote: "origin".into(),
        update,
    }
}

fn pos_plan(up: &Upstream, clone: Option<LocalClone>) -> FetchPlan {
    FetchPlan {
        url: file_url(&up.dir),
        forge: Forge::Github,
        sources: vec![
            (None, Source::Branch("17.0".into())),
            (None, Source::Branch("17.0-fix".into())),
            (None, Source::Pr(1)),
        ],
        mirror: None,
        fallback: None,
        clone,
    }
}

#[test]
fn reference_mode_borrows_objects_from_a_local_clone() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, false);
    let with_clone = Cache::new(tmp.path().join("with"));
    let without = Cache::new(tmp.path().join("without"));
    let borrowed = fetch(
        &with_clone,
        &system(),
        &pos_plan(&up, Some(clone.clone())),
        WAIT,
    )
    .unwrap();
    let alone = fetch(&without, &system(), &pos_plan(&up, None), WAIT).unwrap();
    assert_eq!(borrowed, alone);
    let dir = with_clone.repo_dir(&file_url(&up.dir));
    let alternates = fs::read_to_string(dir.join("objects/info/alternates")).unwrap();
    let canonical = fs::canonicalize(clone.git_dir.join("objects")).unwrap();
    let canonical = canonical.to_string_lossy();
    let expected = canonical.strip_prefix(r"\\?\").unwrap_or(&canonical);
    assert_eq!(alternates, format!("{expected}\n"));
    assert!(own_objects(&dir) < own_objects(&without.repo_dir(&file_url(&up.dir))));
    // Unchanged on the next run.
    fetch(&with_clone, &system(), &pos_plan(&up, Some(clone)), WAIT).unwrap();
}

#[test]
fn update_mode_keeps_the_clone_current_without_touching_its_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, true);
    let work = clone.git_dir.parent().unwrap().to_path_buf();
    fs::write(up.dir.join("new.txt"), "new").unwrap();
    git(&up.dir, &["add", "new.txt"]);
    git(&up.dir, &["commit", "--quiet", "-m", "new"]);
    let new_base = git(&up.dir, &["rev-parse", "HEAD"]);

    let cache = Cache::new(tmp.path().join("cache"));
    let fetched = fetch(&cache, &system(), &pos_plan(&up, Some(clone)), WAIT).unwrap();
    assert_eq!(
        (fetched[0].sha.as_str(), fetched[0].via),
        (new_base.as_str(), Via::Clone)
    );
    assert_eq!(fetched[1].via, Via::Clone);
    assert_eq!(fetched[2].via, Via::Upstream);
    assert_eq!(
        git(&work, &["rev-parse", "refs/remotes/origin/17.0"]),
        new_base
    );
    assert_eq!(
        git(&work, &["rev-parse", "17.0"]),
        up.base,
        "local branches are never moved"
    );
    assert_eq!(
        git(&work, &["status", "--porcelain"]),
        "",
        "the working tree is untouched"
    );
}

#[test]
fn update_mode_without_base_branches_fetches_upstream() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, true);
    let plan = FetchPlan {
        sources: vec![(None, Source::Pr(1))],
        ..pos_plan(&up, Some(clone))
    };
    let fetched = fetch(
        &Cache::new(tmp.path().join("cache")),
        &system(),
        &plan,
        WAIT,
    )
    .unwrap();
    assert_eq!(fetched[0].via, Via::Upstream);
}

#[test]
fn a_vanished_clone_is_reported_instead_of_corrupting_the_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, false);
    let cache = Cache::new(tmp.path().join("cache"));
    fetch(&cache, &system(), &pos_plan(&up, Some(clone)), WAIT).unwrap();
    fs::rename(tmp.path().join("clones"), tmp.path().join("moved")).unwrap();
    let err = fetch(&cache, &system(), &pos_plan(&up, None), WAIT).unwrap_err();
    assert!(
        err.to_string().starts_with("git alternates failed: "),
        "{err}"
    );
    assert!(
        err.to_string().contains("which no longer exists; delete"),
        "{err}"
    );
}

#[test]
fn a_clone_path_that_is_not_a_repository_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let bogus = LocalClone {
        git_dir: tmp.path().join("nope"),
        remote: "origin".into(),
        update: false,
    };
    let err = fetch(
        &Cache::new(tmp.path().join("cache")),
        &system(),
        &pos_plan(&up, Some(bogus)),
        WAIT,
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("cannot run git"), "{err}");
}

/// Delegates to the system backend but fails fetches whose URL contains `needle`.
#[derive(Debug)]
struct FailOn {
    needle: String,
}

impl GitBackend for FailOn {
    fn init_bare(&self, dir: &Path) -> Result<(), GitError> {
        system().init_bare(dir)
    }

    fn fetch(&self, dir: &Path, url: &str, refspecs: &[String]) -> Result<(), GitError> {
        if url.contains(&self.needle) {
            return Err(GitError::Failed {
                command: "fetch".into(),
                stderr: format!("refused {url}"),
            });
        }
        system().fetch(dir, url, refspecs)
    }

    fn resolve(&self, dir: &Path, rev: &str) -> Result<Option<String>, GitError> {
        system().resolve(dir, rev)
    }
}

#[test]
fn update_mode_failures_propagate() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, true);
    // The clone's own fetch fails (its remote name is refused).
    let refuse_remote = FailOn {
        needle: "origin".into(),
    };
    let err = fetch(
        &Cache::new(tmp.path().join("a")),
        &refuse_remote,
        &pos_plan(&up, Some(clone.clone())),
        WAIT,
    )
    .unwrap_err();
    assert_eq!(err.to_string(), "git fetch failed: refused origin");
    // Reading from the clone into the cache fails.
    let refuse_clone = FailOn {
        needle: "clones".into(),
    };
    let err = fetch(
        &Cache::new(tmp.path().join("b")),
        &refuse_clone,
        &pos_plan(&up, Some(clone)),
        WAIT,
    )
    .unwrap_err();
    assert!(
        err.to_string().starts_with("git fetch failed: refused"),
        "{err}"
    );
}

#[test]
fn alternates_write_failures_propagate() {
    let tmp = tempfile::tempdir().unwrap();
    let up = upstream(tmp.path(), "pos");
    let clone = clone_of(tmp.path(), &up, false);
    let plan = pos_plan(&up, Some(clone));

    let cache = Cache::new(tmp.path().join("a"));
    let dir = cache.repo_dir(&plan.url);
    system().init_bare(&dir).unwrap();
    fs::remove_dir_all(dir.join("objects/info")).unwrap();
    fs::write(dir.join("objects/info"), "blocks the directory").unwrap();
    assert!(
        fetch(&cache, &system(), &plan, WAIT)
            .unwrap_err()
            .to_string()
            .starts_with("cannot run git")
    );

    let cache = Cache::new(tmp.path().join("b"));
    let dir = cache.repo_dir(&plan.url);
    system().init_bare(&dir).unwrap();
    fs::create_dir_all(dir.join("objects/info/alternates")).unwrap();
    assert!(
        fetch(&cache, &system(), &plan, WAIT)
            .unwrap_err()
            .to_string()
            .starts_with("cannot run git")
    );
}
