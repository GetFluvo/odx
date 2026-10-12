//! Aggregation, locking, vaulting, the force-push guard rail, auto-update and verification for odx.
//!
//! Phase 2 provides [`fetch_plans`]: turning `[tool.odx]` into one fetch
//! plan per repository URL for the git cache.

use odx_config::OdxConfig;
use odx_config::schema::{ForgeKind, MergeSource};
use odx_core::repo_key;
use odx_git::{FetchPlan, Forge, Source};

/// Errors from planning.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// `--repo` named no configured repository.
    #[error("no repository named {0:?} in [tool.odx]")]
    UnknownRepository(String),
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

const fn forge(kind: ForgeKind) -> Forge {
    match kind {
        ForgeKind::Github => Forge::Github,
        ForgeKind::Gitlab => Forge::Gitlab,
        ForgeKind::Gitea => Forge::Gitea,
    }
}

/// One fetch plan per repository URL.
///
/// Each plan has the base branch and every merge source. Repositories that
/// share a URL (in any spelling) share one plan, so each URL is fetched
/// once. With `prefer_mirror`, the configured mirror is tried first for
/// locked commits.
///
/// ```
/// use odx_aggregate::fetch_plans;
/// use odx_config::parse_pyproject;
/// use std::path::Path;
///
/// let text = "[tool.odx]\nodoo_version = \"17.0\"\n[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\nmerges = [{ pr = 1 }]\n";
/// let config = parse_pyproject(text, Path::new("p.toml")).unwrap().unwrap().config;
/// let plans = fetch_plans(&config, None, false).unwrap();
/// assert_eq!(plans[0].names, ["pos"]);
/// assert_eq!(plans[0].plan.sources.len(), 2);
/// ```
///
/// # Errors
///
/// [`PlanError::UnknownRepository`] when `only` names no repository,
/// [`PlanError::InvalidMerge`] for an invalid merge entry.
pub fn fetch_plans(
    config: &OdxConfig,
    only: Option<&str>,
    prefer_mirror: bool,
) -> Result<Vec<RepoPlan>, PlanError> {
    let mirror = config
        .mirror
        .as_ref()
        .filter(|_| prefer_mirror)
        .map(|m| m.url.clone());
    let mut plans: Vec<RepoPlan> = Vec::new();
    for repo in &config.repositories {
        let name = repo.name.clone().unwrap_or_else(|| repo.url.clone());
        if only.is_some_and(|only| only != name) {
            continue;
        }
        let base = repo.base.as_deref().unwrap_or(&config.odoo_version);
        let mut sources = vec![(None, Source::Branch(base.to_owned()))];
        for merge in &repo.merges {
            let source = merge.source().map_err(|message| PlanError::InvalidMerge {
                repository: name.clone(),
                message,
            })?;
            sources.push(match source {
                MergeSource::Pr(pr) => (None, Source::Pr(pr)),
                MergeSource::Sha(sha) => (None, Source::Commit(sha.to_owned())),
                MergeSource::Branch { branch, remote } => {
                    (remote.map(str::to_owned), Source::Branch(branch.to_owned()))
                }
            });
        }
        let key = repo_key(&repo.url);
        if let Some(existing) = plans.iter_mut().find(|p| repo_key(&p.plan.url) == key) {
            existing.names.push(name);
            for source in sources {
                if !existing.plan.sources.contains(&source) {
                    existing.plan.sources.push(source);
                }
            }
        } else {
            plans.push(RepoPlan {
                names: vec![name],
                plan: FetchPlan {
                    url: repo.url.clone(),
                    forge: repo.forge.map_or_else(|| Forge::detect(&repo.url), forge),
                    sources,
                    mirror: mirror.clone(),
                    clone: None,
                },
            });
        }
    }
    match only {
        Some(only) if plans.is_empty() => Err(PlanError::UnknownRepository(only.to_owned())),
        _ => Ok(plans),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_config::parse_pyproject;
    use std::path::Path;

    fn config(toml: &str) -> OdxConfig {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{toml}");
        parse_pyproject(&text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config
    }

    const REPOS: &str = r#"
[tool.odx.mirror]
url = "https://forge.example/vault.git"

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

    #[test]
    fn one_plan_per_url_with_every_source() {
        let plans = fetch_plans(&config(REPOS), None, true).unwrap();
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
        assert_eq!(plans[1].names, ["https://gitlab.example.com/odoo/web.git"]);
        assert_eq!(plans[1].plan.forge, Forge::Gitea);
    }

    #[test]
    fn filters_and_mirror_preference() {
        let config = config(REPOS);
        let plans = fetch_plans(&config, Some("pos-again"), false).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].plan.mirror, None);
        assert_eq!(
            fetch_plans(&config, Some("nope"), false),
            Err(PlanError::UnknownRepository("nope".into()))
        );
        assert_eq!(
            PlanError::UnknownRepository("nope".into()).to_string(),
            "no repository named \"nope\" in [tool.odx]"
        );
        let forges = fetch_plans(&self::config("[[tool.odx.repositories]]\nurl = \"u\"\nforge = \"github\"\n[[tool.odx.repositories]]\nurl = \"v\"\nforge = \"gitlab\"\n"), None, false).unwrap();
        assert_eq!(
            (forges[0].plan.forge, forges[1].plan.forge),
            (Forge::Github, Forge::Gitlab)
        );
    }

    #[test]
    fn invalid_merges_built_in_code_are_reported() {
        let mut config =
            config("[[tool.odx.repositories]]\nname = \"x\"\nurl = \"u\"\nmerges = [{ pr = 1 }]\n");
        config.repositories[0].merges[0].sha = Some("abc1234".into());
        let err = fetch_plans(&config, None, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "x: set only one of `pr`, `sha` or `branch`"
        );
    }
}
