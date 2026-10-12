use std::path::PathBuf;

use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use super::addons::AddonsFile;
use super::repos::{RepoSpec, ReposFile};
use crate::Problem;
use crate::schema::{Merge, Repository};

/// The result of converting `repos.yaml` (and `addons.yaml`) to `[tool.odx]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// One repository per `repos.yaml` entry, in file order.
    pub repositories: Vec<Repository>,
    /// What could not be represented, or was skipped.
    pub warnings: Vec<Problem>,
}

/// Convert parsed `repos.yaml` and `addons.yaml` to `[[tool.odx.repositories]]`.
///
/// The first merge is the base branch on the repository URL. Later merges
/// become `{ pr }` for pull request refs on that remote, `{ sha }` for
/// commits and `{ branch, remote }` for branches (with `remote` only when it
/// differs from the repository URL).
pub fn import(repos: &ReposFile, addons: Option<&AddonsFile>) -> Import {
    let mut warnings = Vec::new();
    let mut repositories = Vec::new();
    for spec in &repos.repos {
        repositories.push(import_repo(spec, addons, &mut warnings));
    }
    if let Some(addons) = addons {
        for (name, _) in &addons.repos {
            if !repositories.iter().any(|r| r.name.as_deref() == Some(name)) {
                warnings.push(Problem {
                    key: format!("addons.yaml: {name}"),
                    message: "no repository with this name in repos.yaml".to_owned(),
                });
            }
        }
    }
    Import {
        repositories,
        warnings,
    }
}

fn import_repo(
    spec: &RepoSpec,
    addons: Option<&AddonsFile>,
    warnings: &mut Vec<Problem>,
) -> Repository {
    let mut warn = |message: String| {
        warnings.push(Problem {
            key: spec.dir.clone(),
            message,
        });
    };
    let base = &spec.base;
    let url = base.url.clone();
    let dir = spec.dir.trim_start_matches("./").trim_end_matches('/');
    let name = dir.rsplit('/').next().unwrap_or(dir).to_owned();
    let merges = spec
        .merges
        .iter()
        .map(|merge| {
            let same_remote = merge.url == url;
            let mut entry = empty_merge();
            match (pr_number(&merge.reference), is_commit(&merge.reference)) {
                (Some(pr), _) if same_remote => entry.pr = Some(pr),
                (_, true) => {
                    if !same_remote {
                        warn(format!(
                            "commit {} on remote `{}` will be fetched from the repository URL",
                            merge.reference, merge.remote
                        ));
                    }
                    entry.sha = Some(merge.reference.clone());
                }
                _ => {
                    entry.branch = Some(merge.reference.clone());
                    entry.remote = (!same_remote).then(|| merge.url.clone());
                }
            }
            entry
        })
        .collect();
    if let Some(target) = &spec.target
        && spec.remote_url(&target.remote) != Some(url.as_str())
    {
        warn(format!(
            "push remote `{}` is not imported; odx vaults locked commits to [tool.odx.mirror]",
            target.remote
        ));
    }
    if !spec.shell_command_after.is_empty() {
        warn("shell_command_after is not imported; odx does not run shell commands from configuration".to_owned());
    }
    Repository {
        name: Some(name.clone()),
        url,
        target_dir: Some(PathBuf::from(dir)),
        base: Some(base.reference.clone()),
        target_branch: spec.target.as_ref().map(|t| t.branch.clone()),
        forge: None,
        addons: addons.and_then(|a| a.addons(&name)).map(<[String]>::to_vec),
        merges,
    }
}

const fn empty_merge() -> Merge {
    Merge {
        pr: None,
        sha: None,
        branch: None,
        remote: None,
        on_conflict: None,
        on_head_change: None,
        track_upstream: None,
    }
}

/// The PR number of `refs/pull/N/head` (GitHub, Gitea) or
/// `refs/merge-requests/N/head` (GitLab).
pub(crate) fn pr_number(reference: &str) -> Option<u32> {
    ["refs/pull/", "refs/merge-requests/"]
        .into_iter()
        .find_map(|prefix| reference.strip_prefix(prefix)?.strip_suffix("/head"))?
        .parse()
        .ok()
}

/// Whether `reference` looks like an abbreviated or full commit id.
pub(crate) fn is_commit(reference: &str) -> bool {
    (7..=40).contains(&reference.len()) && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

impl Import {
    /// Render the repositories as `[[tool.odx.repositories]]` TOML.
    ///
    /// ```
    /// use odx_config::compat::{import, parse_repos};
    ///
    /// let repos = parse_repos("./pos:\n  remotes:\n    oca: https://github.com/OCA/pos.git\n  merges:\n    - oca 17.0\n    - oca refs/pull/123/head\n  target: oca 17.0-odx\n").unwrap();
    /// let toml = import(&repos, None).to_toml();
    /// assert_eq!(toml, "[[tool.odx.repositories]]\nname = \"pos\"\nurl = \"https://github.com/OCA/pos.git\"\ntarget_dir = \"pos\"\nbase = \"17.0\"\ntarget_branch = \"17.0-odx\"\nmerges = [\n    { pr = 123 },\n]\n");
    /// ```
    pub fn to_toml(&self) -> String {
        let mut tables = ArrayOfTables::new();
        for repo in &self.repositories {
            tables.push(repository_table(repo));
        }
        let mut odx = Table::new();
        odx.set_implicit(true);
        odx.insert("repositories", Item::ArrayOfTables(tables));
        let mut tool = Table::new();
        tool.set_implicit(true);
        tool.insert("odx", Item::Table(odx));
        let mut doc = DocumentMut::new();
        doc.insert("tool", Item::Table(tool));
        doc.to_string()
    }
}

/// A `[[tool.odx.repositories]]` table for `repo`, merges one per line.
pub(crate) fn repository_table(repo: &Repository) -> Table {
    let mut table = Table::new();
    let mut set = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            table.insert(key, Item::Value(value));
        }
    };
    set("name", repo.name.as_deref().map(Value::from));
    set("url", Some(Value::from(repo.url.as_str())));
    set(
        "target_dir",
        repo.target_dir
            .as_ref()
            .map(|dir| Value::from(dir.to_string_lossy().as_ref())),
    );
    set("base", repo.base.as_deref().map(Value::from));
    set(
        "target_branch",
        repo.target_branch.as_deref().map(Value::from),
    );
    set(
        "addons",
        repo.addons
            .as_ref()
            .map(|globs| Value::Array(globs.iter().map(String::as_str).collect())),
    );
    if !repo.merges.is_empty() {
        let mut merges = Array::new();
        for merge in &repo.merges {
            let mut entry = InlineTable::new();
            if let Some(pr) = merge.pr {
                entry.insert("pr", Value::from(i64::from(pr)));
            }
            for (key, value) in [
                ("sha", &merge.sha),
                ("branch", &merge.branch),
                ("remote", &merge.remote),
            ] {
                if let Some(value) = value {
                    entry.insert(key, Value::from(value.as_str()));
                }
            }
            let mut value = Value::InlineTable(entry);
            value.decor_mut().set_prefix("\n    ");
            merges.push_formatted(value);
        }
        merges.set_trailing_comma(true);
        merges.set_trailing("\n");
        table.insert("merges", Item::Value(Value::Array(merges)));
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::{parse_addons, parse_repos};

    const REPOS: &str = r"
./vendor/pos:
  remotes:
    oca: https://github.com/OCA/pos.git
    acme: https://github.com/acme/pos.git
  merges:
    - oca 17.0
    - oca refs/pull/123/head
    - acme refs/pull/9/head
    - oca a1b2c3d4
    - acme 0123abcd
    - oca 17.0-hotfix
    - acme 17.0-fix-rounding
  target: acme 17.0-aggregated
  shell_command_after: make
./web/:
  remotes:
    oca: https://gitlab.example.com/OCA/web.git
  merges:
    - oca 17.0
    - oca refs/merge-requests/7/head
";

    #[test]
    fn maps_every_merge_form_and_reports_what_is_lost() {
        let repos = parse_repos(REPOS).unwrap();
        let addons = parse_addons("pos: [pos_*]\nghost: ['*']\n").unwrap();
        let result = import(&repos, Some(&addons));
        assert_eq!(result.repositories.len(), 2);
        let pos = &result.repositories[0];
        assert_eq!(pos.name.as_deref(), Some("pos"));
        assert_eq!(pos.target_dir, Some(PathBuf::from("vendor/pos")));
        assert_eq!(pos.addons.as_deref(), Some(&["pos_*".to_owned()][..]));
        let rendered = result.to_toml();
        assert!(rendered.contains(
            "merges = [\n    { pr = 123 },\n    { branch = \"refs/pull/9/head\", remote = \"https://github.com/acme/pos.git\" },\n    { sha = \"a1b2c3d4\" },\n    { sha = \"0123abcd\" },\n    { branch = \"17.0-hotfix\" },\n    { branch = \"17.0-fix-rounding\", remote = \"https://github.com/acme/pos.git\" },\n]\n"
        ), "{rendered}");
        assert!(rendered.contains("addons = [\"pos_*\"]\n"));
        let web = &result.repositories[1];
        assert_eq!(web.name.as_deref(), Some("web"));
        assert_eq!(web.merges[0].pr, Some(7));
        assert_eq!(web.target_branch, None);
        let found: Vec<_> = result.warnings.iter().map(ToString::to_string).collect();
        assert_eq!(
            found,
            [
                "./vendor/pos: commit 0123abcd on remote `acme` will be fetched from the repository URL",
                "./vendor/pos: push remote `acme` is not imported; odx vaults locked commits to [tool.odx.mirror]",
                "./vendor/pos: shell_command_after is not imported; odx does not run shell commands from configuration",
                "addons.yaml: ghost: no repository with this name in repos.yaml",
            ]
        );
    }

    #[test]
    fn rendered_toml_is_valid_config() {
        let repos = parse_repos(REPOS).unwrap();
        let rendered = import(&repos, None).to_toml();
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n\n{rendered}");
        let project = crate::parse_pyproject(&text, std::path::Path::new("p.toml"))
            .unwrap()
            .unwrap();
        assert_eq!(
            project.config.repositories,
            import(&repos, None).repositories
        );
    }

    #[test]
    fn base_only_repository_has_no_merges_key() {
        let repos = parse_repos("./x:\n  remotes:\n    o: u\n  merges: [o 17.0]\n").unwrap();
        assert!(!import(&repos, None).to_toml().contains("merges"));
    }

    #[test]
    fn pr_and_commit_detection() {
        assert_eq!(pr_number("refs/pull/12/head"), Some(12));
        assert_eq!(pr_number("refs/pull/x/head"), None);
        assert_eq!(pr_number("refs/pull/12/merge"), None);
        assert!(is_commit("abcdef0"));
        assert!(!is_commit("abcdef"));
        assert!(!is_commit("17.0-fix"));
    }

    #[test]
    fn out_of_range_pr_refs_stay_branches() {
        let repos = parse_repos(
            "./x:\n  remotes:\n    o: u\n  merges:\n    - o 17.0\n    - o refs/pull/18446744073709551615/head\n",
        )
        .unwrap();
        let merge = &import(&repos, None).repositories[0].merges[0];
        assert_eq!(
            merge.branch.as_deref(),
            Some("refs/pull/18446744073709551615/head")
        );
    }
}
