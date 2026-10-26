//! Aggregation, locking, vaulting, the force-push guard rail, auto-update and verification for odx.
//!
//! [`plan_config`] (for `[tool.odx]`) and [`plan_yaml`] (for a
//! git-aggregator `repos.yaml`) describe what to build; [`pin`] replaces the
//! sources by the commits of `odx.lock`. [`fetch_plans`] turns the plans
//! into one fetch per repository URL, [`aggregations`] turns the fetched
//! commits into working-tree builds, and [`run_pipeline`] runs both stages.

#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod aggregation;
mod guard;
mod locking;
mod packages;
mod pipeline;
mod plan;
mod repos_yaml;

pub use aggregation::{BuildOptions, RepoAggregation, aggregations};
pub use guard::{HeadDecision, classify, date, decide};
pub use locking::{Change, ChangeKind, lock_changes, lock_entry, source_label, update_lock};
pub use packages::{
    locked_package, package_entries, package_fetch_plans, package_source, rewrite_packages,
};
pub use pipeline::{local_clones, mergiraf_program, run_fetches, run_pipeline};
pub use plan::{Planned, PlannedSource, PlannedStep, locked_entry, pin, plan_config};
pub use repos_yaml::{plan_yaml, yaml_source};

use odx_config::schema::{ForgeKind, MergeStrategy};
use odx_core::repo_key;
use odx_git::{FetchPlan, Forge, Strategy};

/// Errors from planning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// `--repo` named no configured repository.
    #[error("no repository named {0:?} in [tool.odx]")]
    UnknownRepository(String),
    /// A repository's sources were not (all) fetched.
    #[error("{0}: not fetched")]
    NotFetched(String),
    /// The lock entry of a built repository could not be computed.
    #[error("{repository}: {message}")]
    Lock {
        /// The repository.
        repository: String,
        /// What failed.
        message: String,
    },
    /// A merge entry is invalid (configurations are validated when loaded,
    /// so this only happens for configurations built in code).
    #[error("{repository}: {message}")]
    InvalidMerge {
        /// The repository.
        repository: String,
        /// What is wrong.
        message: String,
    },
}

/// A fetch plan and the configured repositories it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPlan {
    /// Names of the repositories (their URL when unnamed) sharing this URL.
    pub names: Vec<String>,
    /// What to fetch.
    pub plan: FetchPlan,
}

/// The merge engine's strategy for a configured one.
///
/// ```
/// use odx_aggregate::strategy;
/// use odx_config::schema::MergeStrategy;
///
/// assert_eq!(strategy(MergeStrategy::Merge), odx_git::Strategy::Merge);
/// ```
pub const fn strategy(configured: MergeStrategy) -> Strategy {
    match configured {
        MergeStrategy::Rebase => Strategy::Rebase,
        MergeStrategy::Merge => Strategy::Merge,
    }
}

const fn forge(kind: ForgeKind) -> Forge {
    match kind {
        ForgeKind::Github => Forge::Github,
        ForgeKind::Gitlab => Forge::Gitlab,
        ForgeKind::Gitea => Forge::Gitea,
    }
}

/// One fetch plan per repository URL.
///
/// Each plan has the base and every merge source of the repositories that
/// share the URL (in any spelling), so each URL is fetched once. `mirror`
/// is tried first for full commit ids; `fallback` serves full commit ids the
/// repository no longer has.
///
/// ```
/// use odx_aggregate::{fetch_plans, plan_config};
/// use odx_config::parse_pyproject;
/// use std::path::Path;
///
/// let text = "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = [{ pr = 1 }]\n";
/// let config = parse_pyproject(text, Path::new("p.toml")).unwrap().unwrap().config;
/// let plans = fetch_plans(&plan_config(&config, Path::new("."), None).unwrap(), None, None);
/// assert_eq!(plans[0].names, ["pos"]);
/// assert_eq!(plans[0].plan.sources.len(), 2);
/// ```
pub fn fetch_plans(
    planned: &[Planned],
    mirror: Option<&str>,
    fallback: Option<&str>,
) -> Vec<RepoPlan> {
    let mut plans: Vec<RepoPlan> = Vec::new();
    for repo in planned {
        let sources = std::iter::once(&repo.base)
            .chain(repo.steps.iter().map(|step| &step.from))
            .map(|wanted| (wanted.remote.clone(), wanted.source.clone()));
        let key = repo_key(&repo.url);
        if !plans.iter().any(|p| repo_key(&p.plan.url) == key) {
            plans.push(RepoPlan {
                names: Vec::new(),
                plan: FetchPlan {
                    url: repo.url.clone(),
                    forge: repo.forge,
                    sources: Vec::new(),
                    mirror: mirror.map(str::to_owned),
                    fallback: fallback.map(str::to_owned),
                    clone: None,
                    max_age: None,
                    offline: false,
                },
            });
        }
        for entry in plans.iter_mut().filter(|p| repo_key(&p.plan.url) == key) {
            entry.names.push(repo.name.clone());
            for source in sources.clone() {
                if !entry.plan.sources.contains(&source) {
                    entry.plan.sources.push(source);
                }
            }
        }
    }
    plans
}

/// `fetched` as the builders take it.
#[cfg(test)]
pub(crate) fn borrowed(
    fetched: &[(String, Vec<odx_git::Fetched>)],
) -> Vec<(&str, &[odx_git::Fetched])> {
    fetched
        .iter()
        .map(|(url, commits)| (url.as_str(), commits.as_slice()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_config::lock::{LockSource, LockedMerge, LockedRepository, MergeStatus};
    use odx_config::{OdxConfig, parse_pyproject};
    use odx_git::Source;
    use std::path::Path;

    fn config(toml: &str) -> OdxConfig {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{toml}");
        parse_pyproject(&text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config
    }

    const REPOS: &str = r#"
[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"
base = "17.0-x"
merges = [
  { pr = 1 },
  { sha = "a1b2c3d4" },
  { branch = "fix", remote = "https://github.com/acme/pos.git" },
]

[[tool.odx.repositories]]
name = "pos-again"
url = "git@github.com:oca/pos"
merges = [{ pr = 1 }, { pr = 2 }]

[[tool.odx.repositories]]
url = "https://gitlab.example.com/odoo/web.git"
forge = "gitea"
"#;

    fn planned(toml: &str, only: Option<&str>) -> Vec<Planned> {
        plan_config(&config(toml), Path::new("/p"), only).unwrap()
    }

    #[test]
    fn one_plan_per_url_with_every_source() {
        let vault = Some("https://forge.example/vault.git");
        let plans = fetch_plans(&planned(REPOS, None), vault, vault);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].names, ["pos", "pos-again"]);
        assert_eq!(
            plans[0].plan.sources,
            [
                (None, Source::Branch("17.0-x".into())),
                (None, Source::Pr(1)),
                (None, Source::Commit("a1b2c3d4".into())),
                (
                    Some("https://github.com/acme/pos.git".into()),
                    Source::Branch("fix".into())
                ),
                (None, Source::Branch("17.0".into())),
                (None, Source::Pr(2)),
            ]
        );
        assert_eq!(plans[0].plan.forge, Forge::Github);
        assert_eq!(
            plans[0].plan.mirror.as_deref(),
            Some("https://forge.example/vault.git")
        );
        assert_eq!(plans[0].plan.fallback, plans[0].plan.mirror);
        assert_eq!(plans[1].names, ["https://gitlab.example.com/odoo/web.git"]);
        assert_eq!(plans[1].plan.forge, Forge::Gitea);
    }

    #[test]
    fn filters_and_forges() {
        let plans = fetch_plans(&planned(REPOS, Some("pos-again")), None, None);
        assert_eq!(plans.len(), 1);
        assert_eq!(
            (
                plans[0].plan.mirror.as_ref(),
                plans[0].plan.fallback.as_ref()
            ),
            (None, None)
        );
        assert_eq!(
            plan_config(&config(REPOS), Path::new("/p"), Some("nope")),
            Err(PlanError::UnknownRepository("nope".into()))
        );
        assert_eq!(
            PlanError::UnknownRepository("nope".into()).to_string(),
            "no repository named \"nope\" in [tool.odx]"
        );
        let forges = planned(
            "[[tool.odx.repositories]]\nurl = \"u\"\nforge = \"github\"\n[[tool.odx.repositories]]\nurl = \"v\"\nforge = \"gitlab\"\n",
            None,
        );
        assert_eq!(
            (forges[0].forge, forges[1].forge),
            (Forge::Github, Forge::Gitlab)
        );
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn locked(name: &str, url: &str, base: &str, sources: &[LockSource]) -> LockedRepository {
        LockedRepository {
            name: name.to_owned(),
            url: url.to_owned(),
            base: base.to_owned(),
            base_sha: sha('b'),
            result_tree: sha('t'),
            merges: sources
                .iter()
                .map(|source| LockedMerge {
                    source: source.clone(),
                    sha: sha('m'),
                    tree: sha('t'),
                    patch_id: sha('p'),
                    mirror_ref: None,
                    status: MergeStatus::Merged,
                    upstream_head: None,
                    upstream_head_ci: None,
                    upstream_head_classification: None,
                    pinned_since: None,
                })
                .collect(),
        }
    }

    fn skipped(mut entry: LockedRepository) -> LockedRepository {
        entry.merges[1].status = MergeStatus::Skipped;
        entry
    }

    #[test]
    fn pins_only_entries_locked_from_the_same_configuration() {
        let all = planned(REPOS, None);
        let again = &all[1];
        let sources: Vec<LockSource> = again.steps.iter().map(|s| s.lock.clone()).collect();
        let url = "https://github.com/OCA/pos.git";
        let mut repo = again.clone();
        assert!(pin(
            &mut repo,
            &[locked("pos-again", url, "17.0", &sources)]
        ));
        assert!(repo.pinned);
        assert_eq!(repo.base.source, Source::Commit(sha('b')));
        assert_eq!(repo.steps[1].from.source, Source::Commit(sha('m')));
        assert_eq!(repo.steps[1].label, "#2");
        // A merge that was skipped stays pinned while it may be skipped.
        let mut skipping = again.clone();
        skipping.steps[1].skip_on_conflict = true;
        assert!(pin(
            &mut skipping,
            &[skipped(locked("pos-again", url, "17.0", &sources))]
        ));
        for stale in [
            locked("other", url, "17.0", &sources),
            locked("pos-again", "https://x/y.git", "17.0", &sources),
            locked("pos-again", url, "16.0", &sources),
            locked("pos-again", url, "17.0", &sources[..1]),
            locked(
                "pos-again",
                url,
                "17.0",
                &[sources[1].clone(), sources[0].clone()],
            ),
            skipped(locked("pos-again", url, "17.0", &sources)),
        ] {
            let mut repo = again.clone();
            assert!(!pin(&mut repo, &[stale]));
            assert!(!repo.pinned);
            assert_eq!(&repo, again);
        }
    }

    #[test]
    fn strategies() {
        assert_eq!(strategy(MergeStrategy::Rebase), Strategy::Rebase);
        assert_eq!(strategy(MergeStrategy::Merge), Strategy::Merge);
    }
}
