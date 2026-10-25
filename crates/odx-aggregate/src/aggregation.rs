use std::ffi::OsString;

use odx_core::repo_key;
use odx_git::{Aggregation, Cache, Fetched, Step, Strategy};

use crate::{PlanError, Planned, PlannedSource};

/// How one repository is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoAggregation {
    /// The repository's name.
    pub name: String,
    /// What to build.
    pub aggregation: Aggregation,
}

/// Options that apply to every repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildOptions {
    /// How merges are applied.
    pub strategy: Strategy,
    /// The mergiraf executable, when mergiraf is used.
    pub mergiraf: Option<OsString>,
    /// Leave conflicted merges in place.
    pub keep_conflicts: bool,
    /// Discard local changes and replace foreign directories.
    pub force: bool,
}

fn sha_of<'a>(fetched: &'a [Fetched], wanted: &PlannedSource) -> Option<&'a str> {
    fetched
        .iter()
        .find(|f| f.remote == wanted.remote && f.source == wanted.source)
        .map(|f| f.sha.as_str())
}

/// One aggregation per planned repository, using the commits `fetched`
/// for its URL.
///
/// # Errors
///
/// [`PlanError::NotFetched`] for a repository without fetch results or with
/// a source missing from them.
pub fn aggregations(
    planned: &[Planned],
    cache: &Cache,
    fetched: &[(&str, &[Fetched])],
    options: &BuildOptions,
) -> Vec<Result<RepoAggregation, PlanError>> {
    planned
        .iter()
        .map(|repo| {
            let not_fetched = || PlanError::NotFetched(repo.name.clone());
            let key = repo_key(&repo.url);
            let commits = fetched
                .iter()
                .find(|(url, _)| repo_key(url) == key)
                .map(|(_, commits)| *commits)
                .ok_or_else(not_fetched)?;
            let base = sha_of(commits, &repo.base).ok_or_else(not_fetched)?;
            let steps = repo
                .steps
                .iter()
                .map(|step| {
                    Ok(Step {
                        label: step.label.clone(),
                        sha: sha_of(commits, &step.from)
                            .ok_or_else(not_fetched)?
                            .to_owned(),
                        skip_on_conflict: step.skip_on_conflict,
                    })
                })
                .collect::<Result<Vec<_>, PlanError>>()?;
            Ok(RepoAggregation {
                name: repo.name.clone(),
                aggregation: Aggregation {
                    cache_repo: cache.repo_dir(&repo.url),
                    target_dir: repo.target_dir.clone(),
                    base: base.to_owned(),
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
    use crate::plan_config;
    use odx_config::{OdxConfig, parse_pyproject};
    use odx_git::{Source, Via};
    use std::path::Path;

    fn config(toml: &str) -> OdxConfig {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{toml}");
        parse_pyproject(&text, Path::new("p.toml"))
            .unwrap()
            .unwrap()
            .config
    }

    fn fetched(remote: Option<&str>, source: Source, sha: &str) -> Fetched {
        Fetched {
            remote: remote.map(str::to_owned),
            source,
            sha: sha.to_owned(),
            via: Via::Upstream,
        }
    }

    const URL: &str = "https://github.com/OCA/pos.git";

    fn commits() -> Vec<(String, Vec<Fetched>)> {
        vec![(
            "git@github.com:oca/pos".to_owned(),
            vec![
                fetched(None, Source::Branch("17.0".into()), "base"),
                fetched(None, Source::Pr(1), "p1"),
                fetched(None, Source::Commit("abc1234".into()), "full"),
                fetched(
                    Some("https://x/fork.git"),
                    Source::Branch("fix".into()),
                    "fork",
                ),
            ],
        )]
    }

    #[test]
    fn steps_follow_the_configuration() {
        let config = config(&format!(
            "on_conflict = \"skip\"\n[layout]\naggregate_dir = \"src\"\n[[repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nmerges = [{{ pr = 1, on_conflict = \"abort\" }}, {{ sha = \"abc1234\" }}, {{ branch = \"fix\", remote = \"https://x/fork.git\" }}]\n"
        ).replace("[layout]", "[tool.odx.layout]").replace("[[repositories]]", "[[tool.odx.repositories]]"));
        let options = BuildOptions {
            strategy: Strategy::Merge,
            mergiraf: Some("mergiraf".into()),
            keep_conflicts: true,
            force: true,
        };
        let cache = Cache::new("/cache");
        let result = aggregations(
            &plan_config(&config, Path::new("/p"), None).unwrap(),
            &cache,
            &crate::borrowed(&commits()),
            &options,
        );
        let repo = result[0].as_ref().unwrap();
        assert_eq!(repo.name, "pos");
        let a = &repo.aggregation;
        assert_eq!(a.target_dir, Path::new("/p/src/pos"));
        assert_eq!(a.cache_repo, cache.repo_dir(URL));
        assert_eq!(a.base, "base");
        assert_eq!(a.strategy, Strategy::Merge);
        let steps: Vec<_> = a
            .steps
            .iter()
            .map(|s| (s.label.as_str(), s.sha.as_str(), s.skip_on_conflict))
            .collect();
        assert_eq!(
            steps,
            [
                ("#1", "p1", false),
                ("commit abc1234", "full", true),
                ("branch fix", "fork", true)
            ]
        );
        assert_eq!(
            (a.keep_conflicts, a.force, a.mergiraf.is_some()),
            (true, true, true)
        );
    }

    #[test]
    fn target_dirs_and_filters() {
        let config = config(&format!(
            "[[tool.odx.repositories]]\nurl = \"{URL}\"\ntarget_dir = \"addons/pos\"\nbase = \"17.0\"\n[[tool.odx.repositories]]\nname = \"web\"\nurl = \"https://github.com/OCA/web.git\"\n"
        ));
        let result = aggregations(
            &plan_config(&config, Path::new("/p"), None).unwrap(),
            &Cache::new("/c"),
            &crate::borrowed(&commits()),
            &BuildOptions::default(),
        );
        let pos = result[0].as_ref().unwrap();
        assert_eq!(
            (pos.name.as_str(), pos.aggregation.target_dir.as_path()),
            (URL, Path::new("/p/addons/pos"))
        );
        assert_eq!(
            result[1].as_ref().unwrap_err(),
            &PlanError::NotFetched("web".into())
        );
        let only = aggregations(
            &plan_config(&config, Path::new("/p"), Some("web")).unwrap(),
            &Cache::new("/c"),
            &crate::borrowed(&commits()),
            &BuildOptions::default(),
        );
        assert_eq!(only.len(), 1);
    }

    #[test]
    fn default_target_dir_uses_vendor_and_the_last_name_segment() {
        let config = config(
            "[[tool.odx.repositories]]\nurl = \"https://github.com/OCA/pos.git\"\nmerges = [{ pr = 1 }]\n",
        );
        let result = aggregations(
            &plan_config(&config, Path::new("/p"), None).unwrap(),
            &Cache::new("/c"),
            &crate::borrowed(&commits()),
            &BuildOptions::default(),
        );
        assert_eq!(
            result[0].as_ref().unwrap().aggregation.target_dir,
            Path::new("/p/vendor/pos.git")
        );
    }

    #[test]
    fn missing_sources_and_invalid_merges() {
        let missing = config(&format!(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nmerges = [{{ pr = 9 }}]\n"
        ));
        let result = aggregations(
            &plan_config(&missing, Path::new("/p"), None).unwrap(),
            &Cache::new("/c"),
            &crate::borrowed(&commits()),
            &BuildOptions::default(),
        );
        assert_eq!(result[0], Err(PlanError::NotFetched("pos".into())));
        let no_base = config(&format!(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nbase = \"16.0\"\n"
        ));
        let result = aggregations(
            &plan_config(&no_base, Path::new("/p"), None).unwrap(),
            &Cache::new("/c"),
            &crate::borrowed(&commits()),
            &BuildOptions::default(),
        );
        assert_eq!(result[0], Err(PlanError::NotFetched("pos".into())));
        let mut invalid = config(&format!(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nmerges = [{{ pr = 1 }}]\n"
        ));
        invalid.repositories[0].merges[0].branch = Some("x".into());
        assert_eq!(
            plan_config(&invalid, Path::new("/p"), None)
                .unwrap_err()
                .to_string(),
            "pos: set only one of `pr`, `sha` or `branch`"
        );
    }
}
