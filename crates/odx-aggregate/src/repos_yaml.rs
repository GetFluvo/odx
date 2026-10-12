use std::path::Path;

use odx_config::compat::{MergeSpec, RepoSpec};
use odx_core::repo_key;
use odx_git::{Aggregation, Cache, FetchPlan, Fetched, Forge, Source, Step};

use crate::{BuildOptions, PlanError, RepoAggregation, RepoPlan};

/// The source a `repos.yaml` ref stands for: a commit id when it is 7 to 40
/// (or 64) hexadecimal digits, else the ref as written, which the remote
/// resolves (branch, tag or full ref).
///
/// ```
/// use odx_aggregate::yaml_source;
/// use odx_git::Source;
///
/// assert_eq!(yaml_source("17.0"), Source::Ref("17.0".into()));
/// assert_eq!(yaml_source("refs/pull/12/head"), Source::Ref("refs/pull/12/head".into()));
/// assert_eq!(yaml_source("0123abc"), Source::Commit("0123abc".into()));
/// ```
pub fn yaml_source(reference: &str) -> Source {
    let hex = reference.bytes().all(|b| b.is_ascii_hexdigit());
    if hex && matches!(reference.len(), 7..=40 | 64) {
        Source::Commit(reference.to_owned())
    } else {
        Source::Ref(reference.to_owned())
    }
}

/// The remote of a merge, relative to the repository URL: `None` when it is
/// the same repository (in any spelling).
fn remote_of(merge: &MergeSpec, url: &str) -> Option<String> {
    (repo_key(&merge.url) != repo_key(url)).then(|| merge.url.clone())
}

/// One fetch plan per base URL of `repos`, each named after the target
/// directories it serves.
///
/// The base merge's remote is the repository; merges from other remotes are
/// fetched from their own URL into the same cache repository.
///
/// ```
/// use odx_aggregate::yaml_fetch_plans;
/// use odx_config::compat::parse_repos;
///
/// let file = parse_repos("./pos:\n  remotes:\n    oca: https://github.com/OCA/pos.git\n    me: https://github.com/me/pos.git\n  merges:\n    - oca 17.0\n    - me fix\n").unwrap();
/// let plans = yaml_fetch_plans(&file.repos);
/// assert_eq!(plans[0].names, ["./pos"]);
/// assert_eq!(plans[0].plan.sources[1].0.as_deref(), Some("https://github.com/me/pos.git"));
/// ```
pub fn yaml_fetch_plans(repos: &[RepoSpec]) -> Vec<RepoPlan> {
    let mut plans: Vec<RepoPlan> = Vec::new();
    for repo in repos {
        let sources = std::iter::once(&repo.base)
            .chain(&repo.merges)
            .map(|merge| {
                (
                    remote_of(merge, &repo.base.url),
                    yaml_source(&merge.reference),
                )
            });
        let key = repo_key(&repo.base.url);
        if let Some(existing) = plans.iter_mut().find(|p| repo_key(&p.plan.url) == key) {
            existing.names.push(repo.dir.clone());
            for source in sources {
                if !existing.plan.sources.contains(&source) {
                    existing.plan.sources.push(source);
                }
            }
        } else {
            let mut unique = Vec::new();
            for source in sources {
                if !unique.contains(&source) {
                    unique.push(source);
                }
            }
            plans.push(RepoPlan {
                names: vec![repo.dir.clone()],
                plan: FetchPlan {
                    url: repo.base.url.clone(),
                    forge: Forge::detect(&repo.base.url),
                    sources: unique,
                    mirror: None,
                    clone: None,
                },
            });
        }
    }
    plans
}

/// One aggregation per entry of `repos`, using the commits `fetched` for
/// its base URL. Target directories are relative to `root`.
///
/// # Errors
///
/// [`PlanError::NotFetched`] for an entry whose sources were not all
/// fetched.
pub fn yaml_aggregations(
    repos: &[RepoSpec],
    root: &Path,
    cache: &Cache,
    fetched: &[(String, Vec<Fetched>)],
    options: &BuildOptions,
) -> Vec<Result<RepoAggregation, PlanError>> {
    repos
        .iter()
        .map(|repo| {
            let not_fetched = || PlanError::NotFetched(repo.dir.clone());
            let key = repo_key(&repo.base.url);
            let commits = fetched
                .iter()
                .find(|(url, _)| repo_key(url) == key)
                .map(|(_, commits)| commits.as_slice())
                .ok_or_else(not_fetched)?;
            let sha_of = |merge: &MergeSpec| {
                let remote = remote_of(merge, &repo.base.url);
                let source = yaml_source(&merge.reference);
                commits
                    .iter()
                    .find(|f| f.remote == remote && f.source == source)
                    .map(|f| f.sha.clone())
                    .ok_or_else(not_fetched)
            };
            let base = sha_of(&repo.base)?;
            let steps = repo
                .merges
                .iter()
                .map(|merge| {
                    Ok(Step {
                        label: format!("{} {}", merge.remote, merge.reference),
                        sha: sha_of(merge)?,
                        skip_on_conflict: false,
                    })
                })
                .collect::<Result<Vec<_>, PlanError>>()?;
            Ok(RepoAggregation {
                name: repo.dir.clone(),
                aggregation: Aggregation {
                    cache_repo: cache.repo_dir(&repo.base.url),
                    target_dir: root.join(&repo.dir),
                    base,
                    steps,
                    strategy: options.strategy,
                    mergiraf: options.mergiraf.clone(),
                    keep_conflicts: options.keep_conflicts,
                    force: options.force,
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_config::compat::parse_repos;
    use odx_git::{Strategy, Via};

    const YAML: &str = "\
./pos:
  remotes:
    oca: https://github.com/OCA/pos.git
    me: https://github.com/me/pos.git
  merges:
    - oca 17.0
    - oca refs/pull/1/head
    - oca refs/pull/1/head
    - me fix
    - oca 0123456789abcdef0123456789abcdef01234567
./pos-copy:
  remotes:
    upstream: git@github.com:oca/pos
  merges:
    - upstream 17.0
    - upstream 16.0
./old:
  remotes:
    oca: https://github.com/OCA/pos.git
  merges:
    - oca 15.0
/abs/web:
  remotes:
    oca: https://github.com/OCA/web.git
  merges:
    - oca 17.0
";

    fn fetched(remote: Option<&str>, source: Source, sha: &str) -> Fetched {
        Fetched {
            remote: remote.map(str::to_owned),
            source,
            sha: sha.to_owned(),
            via: Via::Upstream,
        }
    }

    #[test]
    fn plans_share_a_url_and_keep_other_remotes_apart() {
        let file = parse_repos(YAML).unwrap();
        let plans = yaml_fetch_plans(&file.repos);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].names[..2], ["./pos", "./pos-copy"]);
        assert_eq!(
            plans[0].plan.sources,
            [
                (None, Source::Ref("17.0".into())),
                (None, Source::Ref("refs/pull/1/head".into())),
                (
                    Some("https://github.com/me/pos.git".into()),
                    Source::Ref("fix".into())
                ),
                (
                    None,
                    Source::Commit("0123456789abcdef0123456789abcdef01234567".into())
                ),
                (None, Source::Ref("16.0".into())),
                (None, Source::Ref("15.0".into())),
            ]
        );
        assert_eq!(plans[0].names[2], "./old");
        assert_eq!(plans[1].names, ["/abs/web"]);
        assert_eq!(plans[0].plan.forge, Forge::Github);
    }

    #[test]
    fn aggregations_use_the_fetched_commits() {
        let file = parse_repos(YAML).unwrap();
        let commits = vec![
            fetched(None, Source::Ref("17.0".into()), "base"),
            fetched(None, Source::Ref("refs/pull/1/head".into()), "pr"),
            fetched(
                Some("https://github.com/me/pos.git"),
                Source::Ref("fix".into()),
                "fix",
            ),
            fetched(
                None,
                Source::Commit("0123456789abcdef0123456789abcdef01234567".into()),
                "full",
            ),
        ];
        let fetched = vec![("https://github.com/OCA/pos.git".to_owned(), commits)];
        let cache = Cache::new("/cache");
        let options = BuildOptions {
            strategy: Strategy::Rebase,
            mergiraf: Some("mergiraf".into()),
            keep_conflicts: true,
            force: true,
        };
        let built = yaml_aggregations(&file.repos, Path::new("/w"), &cache, &fetched, &options);
        let pos = built[0].as_ref().unwrap();
        assert_eq!(pos.name, "./pos");
        assert_eq!(pos.aggregation.target_dir, Path::new("/w/./pos"));
        assert_eq!(pos.aggregation.base, "base");
        assert_eq!(
            pos.aggregation.cache_repo,
            cache.repo_dir("https://github.com/OCA/pos.git")
        );
        let steps: Vec<_> = pos
            .aggregation
            .steps
            .iter()
            .map(|s| (s.label.as_str(), s.sha.as_str(), s.skip_on_conflict))
            .collect();
        assert_eq!(
            steps,
            [
                ("oca refs/pull/1/head", "pr", false),
                ("oca refs/pull/1/head", "pr", false),
                ("me fix", "fix", false),
                (
                    "oca 0123456789abcdef0123456789abcdef01234567",
                    "full",
                    false
                ),
            ]
        );
        assert!(pos.aggregation.force && pos.aggregation.keep_conflicts);
        assert_eq!(pos.aggregation.strategy, Strategy::Rebase);
        // 16.0 was not fetched, and web not at all.
        assert_eq!(built[1], Err(PlanError::NotFetched("./pos-copy".into())));
        assert_eq!(built[2], Err(PlanError::NotFetched("./old".into())));
        assert_eq!(built[3], Err(PlanError::NotFetched("/abs/web".into())));
    }

    #[test]
    fn sources_from_refs() {
        assert_eq!(yaml_source("abcdef"), Source::Ref("abcdef".into()));
        assert_eq!(yaml_source(&"a".repeat(64)), Source::Commit("a".repeat(64)));
        assert_eq!(yaml_source(&"a".repeat(41)), Source::Ref("a".repeat(41)));
    }
}
