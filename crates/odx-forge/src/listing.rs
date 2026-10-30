//! Organization listings for the addon index: the addons on a series
//! branch of every repository an organization (or GitLab group) owns.
//!
//! GitHub answers with one GraphQL query per fifty repositories, the
//! directories at the root of their series branch included: those are the
//! addons, apart from hidden ones and `setup` (asking for every
//! directory's manifest as well makes GitHub time out on large
//! organizations). GitLab and Gitea/Forgejo list the repositories and then
//! read each series tree, where only directories with a manifest count.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::{ForgeClient, ForgeError, ForgeKind, RepoUrl};

/// Repository pages read at most, so a huge organization cannot run away.
const MAX_PAGES: usize = 50;

/// Tree pages read at most per repository (GitLab pages by a hundred).
const TREE_PAGES: usize = 100;

/// Fifty repositories of an owner with the entries at the root of the
/// series branch.
const QUERY: &str = "query($owner:String!,$after:String,$expr:String!){\
repositoryOwner(login:$owner){repositories(first:50,after:$after,isArchived:false,\
orderBy:{field:NAME,direction:ASC}){pageInfo{hasNextPage endCursor}\
nodes{url object(expression:$expr){...on Tree{entries{name type}}}}}}}";

/// The entries of a tree object.
fn entries(tree: &Value) -> &[Value] {
    tree.get("entries")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The addons among a tree's paths: directories at its root with a
/// manifest.
fn addons(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| path.strip_suffix("/__manifest__.py"))
        .filter(|dir| !dir.contains('/'))
        .map(str::to_owned)
        .collect()
}

impl ForgeClient<'_> {
    /// Every addon on branch `series` of the repositories `owner` (an
    /// organization, user or GitLab group on `site`'s host) owns, as
    /// (repository URL, addon), skipping archived repositories and those
    /// without the branch.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TokenRequired`] for GitHub without a token,
    /// [`ForgeError::NotVisible`] when the owner is unknown, and network,
    /// status, JSON and rate-limit errors.
    pub fn organization_addons(
        &self,
        site: &RepoUrl,
        kind: ForgeKind,
        owner: &str,
        series: &str,
    ) -> Result<Vec<(String, String)>, ForgeError> {
        match kind {
            ForgeKind::Github => self.github_listing(site, owner, series),
            ForgeKind::Gitlab | ForgeKind::Gitea => self.rest_listing(site, kind, owner, series),
        }
    }

    /// The manifest of addon `addon` on branch `series` of `repo`
    /// (`<addon>/__manifest__.py`); `None` when it is not there, or the
    /// branch is missing.
    ///
    /// # Errors
    ///
    /// Network, status, JSON and rate-limit errors, and a file that is not
    /// base64-encoded UTF-8.
    pub fn manifest(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        series: &str,
        addon: &str,
    ) -> Result<Option<String>, ForgeError> {
        let api = Self::repo_api(repo, kind);
        let url = match kind {
            ForgeKind::Gitlab => {
                format!("{api}/repository/files/{addon}%2F__manifest__.py?ref={series}")
            }
            ForgeKind::Github | ForgeKind::Gitea => {
                format!("{api}/contents/{addon}/__manifest__.py?ref={series}")
            }
        };
        let Some(file) = self.json(&url, repo, kind)? else {
            return Ok(None);
        };
        // Every forge sends the file base64-encoded, GitHub in lines.
        let encoded: String = file
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        STANDARD
            .decode(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .map(Some)
            .ok_or_else(|| ForgeError::Json(format!("{}: {url}", repo.host)))
    }

    fn github_listing(
        &self,
        site: &RepoUrl,
        owner: &str,
        series: &str,
    ) -> Result<Vec<(String, String)>, ForgeError> {
        let url = if site.host == "github.com" {
            "https://api.github.com/graphql".to_owned()
        } else {
            format!("{}/api/graphql", site.origin())
        };
        let mut found = Vec::new();
        let mut after = Value::Null;
        for _ in 0..MAX_PAGES {
            let body = json!({
                "query": QUERY,
                "variables": {"owner": owner, "after": after, "expr": format!("{series}:")},
            });
            let response = self.post(&url, site, ForgeKind::Github, &body)?;
            match response.status {
                200 => {}
                401 => return Err(ForgeError::TokenRequired(site.host.clone())),
                status => {
                    return Err(ForgeError::Status {
                        host: site.host.clone(),
                        status,
                    });
                }
            }
            let value: Value = serde_json::from_slice(&response.body).unwrap_or_default();
            let Some(repos) = value.pointer("/data/repositoryOwner/repositories") else {
                return Err(if value.get("errors").is_some() || value.is_null() {
                    ForgeError::Json(site.host.clone())
                } else {
                    ForgeError::NotVisible(format!("{}/{owner}", site.host))
                });
            };
            for node in repos
                .get("nodes")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
            {
                let repository = node.get("url").and_then(Value::as_str).unwrap_or_default();
                for entry in entries(&node["object"]) {
                    let name = entry["name"].as_str().unwrap_or_default();
                    let addon =
                        entry["type"] == "tree" && !name.starts_with('.') && name != "setup";
                    if addon {
                        found.push((repository.to_owned(), name.to_owned()));
                    }
                }
            }
            if repos.pointer("/pageInfo/hasNextPage") != Some(&Value::Bool(true)) {
                break;
            }
            after = repos
                .pointer("/pageInfo/endCursor")
                .cloned()
                .unwrap_or_default();
        }
        Ok(found)
    }

    fn rest_listing(
        &self,
        site: &RepoUrl,
        kind: ForgeKind,
        owner: &str,
        series: &str,
    ) -> Result<Vec<(String, String)>, ForgeError> {
        let origin = site.origin();
        let (per_page, link) = match kind {
            ForgeKind::Gitlab => (100, "web_url"),
            ForgeKind::Github | ForgeKind::Gitea => (50, "html_url"),
        };
        let mut found = Vec::new();
        for page in 1..=MAX_PAGES {
            let url = match kind {
                ForgeKind::Gitlab => format!(
                    "{origin}/api/v4/groups/{}/projects?include_subgroups=true&archived=false&per_page={per_page}&page={page}",
                    owner.replace('/', "%2F")
                ),
                ForgeKind::Github | ForgeKind::Gitea => {
                    format!("{origin}/api/v1/orgs/{owner}/repos?limit={per_page}&page={page}")
                }
            };
            let Some(listed) = self.json(&url, site, kind)? else {
                return Err(ForgeError::NotVisible(format!("{}/{owner}", site.host)));
            };
            let repos = listed.as_array().map_or(&[][..], Vec::as_slice);
            for repo in repos.iter().filter(|repo| repo["archived"] != true) {
                let repository = repo[link].as_str().unwrap_or_default();
                for addon in addons(&self.tree(site, kind, repo, series)?) {
                    found.push((repository.to_owned(), addon));
                }
            }
            if repos.len() < per_page {
                break;
            }
        }
        Ok(found)
    }

    /// The paths on branch `series` of `repo` (a listed repository);
    /// empty when the branch is missing.
    fn tree(
        &self,
        site: &RepoUrl,
        kind: ForgeKind,
        repo: &Value,
        series: &str,
    ) -> Result<Vec<String>, ForgeError> {
        let origin = site.origin();
        let mut paths = Vec::new();
        for page in 1..=TREE_PAGES {
            let url = match kind {
                ForgeKind::Gitlab => format!(
                    "{origin}/api/v4/projects/{}/repository/tree?ref={series}&recursive=true&per_page=100&page={page}",
                    repo["id"]
                ),
                ForgeKind::Github | ForgeKind::Gitea => format!(
                    "{origin}/api/v1/repos/{}/git/trees/{series}?recursive=true&per_page=10000&page={page}",
                    repo["full_name"].as_str().unwrap_or_default()
                ),
            };
            let Some(value) = self.json(&url, site, kind)? else {
                break;
            };
            let listed = match kind {
                ForgeKind::Gitlab => value.as_array(),
                ForgeKind::Github | ForgeKind::Gitea => value.get("tree").and_then(Value::as_array),
            }
            .map_or(&[][..], Vec::as_slice);
            paths.extend(
                listed
                    .iter()
                    .filter_map(|entry| entry["path"].as_str())
                    .map(str::to_owned),
            );
            let more = match kind {
                ForgeKind::Gitlab => listed.len() == 100,
                ForgeKind::Github | ForgeKind::Gitea => {
                    value.get("truncated") == Some(&Value::Bool(true))
                }
            };
            if !more {
                break;
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};

    type Listed = Result<Vec<(String, String)>, ForgeError>;

    fn list(
        site: &str,
        kind: ForgeKind,
        owner: &str,
        answers: Vec<(u16, String)>,
    ) -> (Listed, Vec<String>) {
        let http = Scripted::new(
            answers
                .into_iter()
                .map(|(status, body)| Ok(reply(status, &body)))
                .collect(),
        );
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
        let site = RepoUrl::parse(site).unwrap();
        let result = client.organization_addons(&site, kind, owner, "17.0");
        (result, http.urls())
    }

    fn found(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(repo, addon)| ((*repo).to_owned(), (*addon).to_owned()))
            .collect()
    }

    #[test]
    fn github_pages_through_graphql() {
        let first = json!({"data": {"repositoryOwner": {"repositories": {
            "pageInfo": {"hasNextPage": true, "endCursor": "c1"},
            "nodes": [
                {"url": "https://github.com/OCA/pos", "object": {"entries": [
                    {"name": "pos_x", "type": "tree"},
                    {"name": "setup", "type": "tree"},
                    {"name": ".github", "type": "tree"},
                    {"name": "README.md", "type": "blob"},
                ]}},
                {"url": "https://github.com/OCA/old", "object": null},
            ],
        }}}});
        let second = json!({"data": {"repositoryOwner": {"repositories": {
            "pageInfo": {"hasNextPage": false, "endCursor": null},
            "nodes": [{"url": "https://github.com/OCA/web", "object": {"entries": [
                {"name": "web_y", "type": "tree"},
            ]}}],
        }}}});
        let (result, urls) = list(
            "https://github.com/OCA/pos",
            ForgeKind::Github,
            "OCA",
            vec![(200, first.to_string()), (200, second.to_string())],
        );
        assert_eq!(
            result.unwrap(),
            found(&[
                ("https://github.com/OCA/pos", "pos_x"),
                ("https://github.com/OCA/web", "web_y"),
            ])
        );
        assert_eq!(urls.len(), 2);
        assert!(urls[0].starts_with("POST https://api.github.com/graphql "));
        assert!(urls[0].contains(r#""after":null"#));
        assert!(urls[0].contains(r#""expr":"17.0:""#));
        assert!(urls[1].contains(r#""after":"c1""#));
    }

    #[test]
    fn github_failures() {
        let enterprise = "https://git.example.com/acme/x";
        let (result, urls) = list(
            enterprise,
            ForgeKind::Github,
            "acme",
            vec![(401, String::new())],
        );
        assert_eq!(
            result,
            Err(ForgeError::TokenRequired("git.example.com".to_owned()))
        );
        assert!(urls[0].starts_with("POST https://git.example.com/api/graphql "));
        let (result, _) = list(
            enterprise,
            ForgeKind::Github,
            "acme",
            vec![(502, String::new())],
        );
        assert_eq!(
            result,
            Err(ForgeError::Status {
                host: "git.example.com".to_owned(),
                status: 502
            })
        );
        for body in ["not json", r#"{"errors":[{"message":"x"}]}"#] {
            let (result, _) = list(
                enterprise,
                ForgeKind::Github,
                "acme",
                vec![(200, body.to_owned())],
            );
            assert_eq!(result, Err(ForgeError::Json("git.example.com".to_owned())));
        }
        let missing = r#"{"data":{"repositoryOwner":null}}"#.to_owned();
        let (result, _) = list(enterprise, ForgeKind::Github, "acme", vec![(200, missing)]);
        assert_eq!(
            result,
            Err(ForgeError::NotVisible("git.example.com/acme".to_owned()))
        );
    }

    #[test]
    fn network_failures() {
        let down = || Err(ForgeError::Http("down".to_owned()));
        let repos = json!([{"full_name": "o/r", "html_url": "https://codeberg.org/o/r"}]);
        for (kind, answers) in [
            (ForgeKind::Github, vec![down()]),
            (ForgeKind::Gitea, vec![down()]),
            (
                ForgeKind::Gitea,
                vec![Ok(reply(200, &repos.to_string())), down()],
            ),
        ] {
            let http = Scripted::new(answers);
            let env = MapEnv::default();
            let runner = ScriptedRunner::default();
            let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
            let site = RepoUrl::parse("https://codeberg.org/o/r").unwrap();
            assert_eq!(
                client.organization_addons(&site, kind, "o", "17.0"),
                Err(ForgeError::Http("down".to_owned()))
            );
        }
    }

    #[test]
    fn gitea_lists_repositories_and_trees() {
        let mut repos: Vec<Value> = (0..50)
            .map(|n| json!({"full_name": format!("o/r{n}"), "html_url": format!("https://codeberg.org/o/r{n}"), "archived": n > 0}))
            .collect();
        repos[1]["archived"] = json!(false);
        let tree = json!({"truncated": true, "tree": [
            {"path": "a_x/__manifest__.py"},
            {"path": "setup/a_x/__manifest__.py"},
            {"path": "README.md"},
        ]});
        let (result, urls) = list(
            "https://codeberg.org/o/r",
            ForgeKind::Gitea,
            "o",
            vec![
                (200, Value::Array(repos).to_string()),
                (200, tree.to_string()),
                (
                    200,
                    json!({"tree": [{"path": "a_y/__manifest__.py"}]}).to_string(),
                ),
                (404, String::new()),
                (200, "[]".to_owned()),
            ],
        );
        assert_eq!(
            result.unwrap(),
            found(&[
                ("https://codeberg.org/o/r0", "a_x"),
                ("https://codeberg.org/o/r0", "a_y"),
            ])
        );
        assert_eq!(
            urls,
            [
                "https://codeberg.org/api/v1/orgs/o/repos?limit=50&page=1",
                "https://codeberg.org/api/v1/repos/o/r0/git/trees/17.0?recursive=true&per_page=10000&page=1",
                "https://codeberg.org/api/v1/repos/o/r0/git/trees/17.0?recursive=true&per_page=10000&page=2",
                "https://codeberg.org/api/v1/repos/o/r1/git/trees/17.0?recursive=true&per_page=10000&page=1",
                "https://codeberg.org/api/v1/orgs/o/repos?limit=50&page=2",
            ]
        );
        let (result, _) = list("https://codeberg.org/o/r", ForgeKind::Gitea, "o", vec![]);
        assert_eq!(
            result,
            Err(ForgeError::NotVisible("codeberg.org/o".to_owned()))
        );
    }

    #[test]
    fn an_addon_on_a_branch_is_its_manifest() {
        // "{'name': 'rma'}", in lines as GitHub sends it.
        let github = r#"{"content":"eyduYW1lJzog\nJ3JtYSd9\n","encoding":"base64"}"#;
        for (url, kind, status, body, asked) in [
            (
                "https://github.com/OCA/rma",
                ForgeKind::Github,
                200,
                github,
                "https://api.github.com/repos/OCA/rma/contents/rma/__manifest__.py?ref=17.0",
            ),
            (
                "https://gitlab.com/g/rma",
                ForgeKind::Gitlab,
                404,
                "",
                "https://gitlab.com/api/v4/projects/g%2Frma/repository/files/rma%2F__manifest__.py?ref=17.0",
            ),
            (
                "https://codeberg.org/o/rma",
                ForgeKind::Gitea,
                500,
                "",
                "https://codeberg.org/api/v1/repos/o/rma/contents/rma/__manifest__.py?ref=17.0",
            ),
        ] {
            let http = Scripted::new(vec![Ok(reply(status, body))]);
            let env = MapEnv::default();
            let runner = ScriptedRunner::default();
            let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
            let repo = RepoUrl::parse(url).unwrap();
            let found = client.manifest(&repo, kind, "17.0", "rma");
            assert_eq!(http.urls(), [asked]);
            match status {
                200 => assert_eq!(found, Ok(Some("{'name': 'rma'}".to_owned()))),
                404 => assert_eq!(found, Ok(None)),
                _ => assert!(found.is_err()),
            }
        }
        // A file that is no base64 says so.
        let http = Scripted::new(vec![Ok(reply(200, r#"{"content":"!!"}"#))]);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
        let repo = RepoUrl::parse("https://codeberg.org/o/rma").unwrap();
        assert!(matches!(
            client.manifest(&repo, ForgeKind::Gitea, "17.0", "rma"),
            Err(ForgeError::Json(_))
        ));
    }

    #[test]
    fn gitlab_pages_through_groups_and_trees() {
        let full: Vec<Value> = (0..100).map(|n| json!({"path": format!("f{n}")})).collect();
        let (result, urls) = list(
            "https://gitlab.com/g/s/r",
            ForgeKind::Gitlab,
            "g/s",
            vec![
                (
                    200,
                    json!([{"id": 7, "web_url": "https://gitlab.com/g/s/r"}]).to_string(),
                ),
                (200, Value::Array(full).to_string()),
                (200, json!([{"path": "b_z/__manifest__.py"}]).to_string()),
            ],
        );
        assert_eq!(
            result.unwrap(),
            found(&[("https://gitlab.com/g/s/r", "b_z")])
        );
        assert_eq!(
            urls,
            [
                "https://gitlab.com/api/v4/groups/g%2Fs/projects?include_subgroups=true&archived=false&per_page=100&page=1",
                "https://gitlab.com/api/v4/projects/7/repository/tree?ref=17.0&recursive=true&per_page=100&page=1",
                "https://gitlab.com/api/v4/projects/7/repository/tree?ref=17.0&recursive=true&per_page=100&page=2",
            ]
        );
    }
}
