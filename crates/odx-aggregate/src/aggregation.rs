use std::ffi::OsString;
use std::path::{Path, PathBuf};

use odx_config::OdxConfig;
use odx_config::schema::{MergeSource, OnConflict};
use odx_core::repo_key;
use odx_git::{Aggregation, Cache, Fetched, Source, Step, Strategy};

use crate::PlanError;

/// How one configured repository is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoAggregation {
    /// The repository's name (its URL when unnamed).
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

/// The working tree of a repository: `target_dir`, else
/// `<layout.aggregate_dir or "vendor">/<name>`, relative to `root`.
fn target_dir(
    config: &OdxConfig,
    root: &Path,
    repo: &odx_config::schema::Repository,
    name: &str,
) -> PathBuf {
    repo.target_dir.as_ref().map_or_else(
        || {
            let parent = config
                .layout
                .as_ref()
                .and_then(|layout| layout.aggregate_dir.clone())
                .unwrap_or_else(|| PathBuf::from("vendor"));
            root.join(parent)
                .join(name.rsplit('/').next().unwrap_or(name))
        },
        |dir| root.join(dir),
    )
}

fn sha_of<'a>(fetched: &'a [Fetched], remote: Option<&str>, source: &Source) -> Option<&'a str> {
    fetched
        .iter()
        .find(|f| f.remote.as_deref() == remote && f.source == *source)
        .map(|f| f.sha.as_str())
}

/// One aggregation per configured repository, using the commits `fetched`
/// for its URL. Repositories whose URL was not fetched are reported.
///
/// # Errors
///
/// [`PlanError::NotFetched`] for a repository without fetch results or with
/// a source missing from them, and [`PlanError::InvalidMerge`].
pub fn aggregations(
    config: &OdxConfig,
    root: &Path,
    only: Option<&str>,
    cache: &Cache,
    fetched: &[(String, Vec<Fetched>)],
    options: &BuildOptions,
) -> Vec<Result<RepoAggregation, PlanError>> {
    let default_skip = config.on_conflict == Some(OnConflict::Skip);
    config
        .repositories
        .iter()
        .map(|repo| (repo.name.clone().unwrap_or_else(|| repo.url.clone()), repo))
        .filter(|(name, _)| only.is_none_or(|only| only == name))
        .map(|(name, repo)| {
            let not_fetched = || PlanError::NotFetched(name.clone());
            let key = repo_key(&repo.url);
            let commits = fetched
                .iter()
                .find(|(url, _)| repo_key(url) == key)
                .map(|(_, commits)| commits.as_slice())
                .ok_or_else(not_fetched)?;
            let base_branch = repo.base.as_deref().unwrap_or(&config.odoo_version);
            let base = sha_of(commits, None, &Source::Branch(base_branch.to_owned()))
                .ok_or_else(not_fetched)?;
            let mut steps = Vec::with_capacity(repo.merges.len());
            for merge in &repo.merges {
                let source = merge.source().map_err(|message| PlanError::InvalidMerge {
                    repository: name.clone(),
                    message,
                })?;
                let (remote, source, label) = match source {
                    MergeSource::Pr(pr) => (None, Source::Pr(pr), format!("#{pr}")),
                    MergeSource::Sha(sha) => (
                        None,
                        Source::Commit(sha.to_owned()),
                        format!("commit {sha}"),
                    ),
                    MergeSource::Branch { branch, remote } => (
                        remote,
                        Source::Branch(branch.to_owned()),
                        format!("branch {branch}"),
                    ),
                };
                let sha = sha_of(commits, remote, &source).ok_or_else(not_fetched)?;
                steps.push(Step {
                    label,
                    sha: sha.to_owned(),
                    skip_on_conflict: merge
                        .on_conflict
                        .map_or(default_skip, |c| c == OnConflict::Skip),
                });
            }
            Ok(RepoAggregation {
                aggregation: Aggregation {
                    cache_repo: cache.repo_dir(&repo.url),
                    target_dir: target_dir(config, root, repo, &name),
                    base: base.to_owned(),
                    steps,
                    strategy: options.strategy,
                    mergiraf: options.mergiraf.clone(),
                    keep_conflicts: options.keep_conflicts,
                    force: options.force,
                },
                name,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_config::parse_pyproject;
    use odx_git::Via;

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
        let result = aggregations(&config, Path::new("/p"), None, &cache, &commits(), &options);
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
            &config,
            Path::new("/p"),
            None,
            &Cache::new("/c"),
            &commits(),
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
            &config,
            Path::new("/p"),
            Some("web"),
            &Cache::new("/c"),
            &commits(),
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
            &config,
            Path::new("/p"),
            None,
            &Cache::new("/c"),
            &commits(),
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
            &missing,
            Path::new("/p"),
            None,
            &Cache::new("/c"),
            &commits(),
            &BuildOptions::default(),
        );
        assert_eq!(result[0], Err(PlanError::NotFetched("pos".into())));
        let no_base = config(&format!(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nbase = \"16.0\"\n"
        ));
        let result = aggregations(
            &no_base,
            Path::new("/p"),
            None,
            &Cache::new("/c"),
            &commits(),
            &BuildOptions::default(),
        );
        assert_eq!(result[0], Err(PlanError::NotFetched("pos".into())));
        let mut invalid = config(&format!(
            "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"{URL}\"\nmerges = [{{ pr = 1 }}]\n"
        ));
        invalid.repositories[0].merges[0].branch = Some("x".into());
        let result = aggregations(
            &invalid,
            Path::new("/p"),
            None,
            &Cache::new("/c"),
            &commits(),
            &BuildOptions::default(),
        );
        assert_eq!(
            result[0].as_ref().unwrap_err().to_string(),
            "pos: set only one of `pr`, `sha` or `branch`"
        );
    }
}
