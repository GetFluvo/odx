//! Pull requests (merge requests on GitLab): state, head, labels.

use serde_json::Value;

use crate::{ForgeClient, ForgeError, ForgeKind, RepoUrl};

/// Where a pull request stands upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    /// Open (including drafts).
    Open,
    /// Merged into its base branch.
    Merged,
    /// Closed without being merged.
    Closed,
}

impl PrState {
    /// `open`, `merged` or `closed`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

/// A pull request as the forge reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// Its number (`iid` on GitLab).
    pub number: u64,
    /// Its title.
    pub title: String,
    /// Open, merged or closed.
    pub state: PrState,
    /// The commit its head points at.
    pub head_sha: String,
    /// The branch it targets.
    pub base: String,
    /// When it was merged (`YYYY-MM-DD…`), if it was.
    pub merged_at: Option<String>,
    /// Its labels, sorted.
    pub labels: Vec<String>,
    /// The page of the repository its branch lives in, when the forge
    /// says (GitHub, Gitea/Forgejo).
    pub head_repository: Option<String>,
    /// The branch it comes from.
    pub head_branch: String,
    /// Whether maintainers of the target may push to that branch.
    pub maintainer_can_modify: bool,
    /// Who opened it.
    pub author: String,
    /// When it was opened (`YYYY-MM-DD…`).
    pub created_at: String,
}

fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A GitHub or Gitea/Forgejo pull request.
fn github_like(value: &Value) -> PullRequest {
    let merged = value.get("merged").and_then(Value::as_bool) == Some(true)
        || value.get("merged_at").is_some_and(Value::is_string);
    let state = match (text(value, "/state").as_str(), merged) {
        (_, true) => PrState::Merged,
        ("open", _) => PrState::Open,
        _ => PrState::Closed,
    };
    let mut labels: Vec<String> = value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| labels.iter().map(|label| text(label, "/name")).collect())
        .unwrap_or_default();
    labels.sort();
    PullRequest {
        number: value
            .get("number")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        title: text(value, "/title"),
        state,
        head_sha: text(value, "/head/sha"),
        base: text(value, "/base/ref"),
        merged_at: value
            .get("merged_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        labels,
        head_repository: value
            .pointer("/head/repo/html_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        head_branch: text(value, "/head/ref"),
        maintainer_can_modify: ["/maintainer_can_modify", "/allow_maintainer_edit"]
            .iter()
            .any(|key| value.pointer(key) == Some(&Value::Bool(true))),
        author: text(value, "/user/login"),
        created_at: text(value, "/created_at"),
    }
}

/// A GitLab merge request.
fn gitlab(value: &Value) -> PullRequest {
    let state = match text(value, "/state").as_str() {
        "opened" => PrState::Open,
        "merged" => PrState::Merged,
        _ => PrState::Closed,
    };
    let mut labels: Vec<String> = value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    labels.sort();
    PullRequest {
        number: value.get("iid").and_then(Value::as_u64).unwrap_or_default(),
        title: text(value, "/title"),
        state,
        head_sha: text(value, "/sha"),
        base: text(value, "/target_branch"),
        merged_at: value
            .get("merged_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        labels,
        head_repository: None,
        head_branch: text(value, "/source_branch"),
        maintainer_can_modify: value.get("allow_collaboration") == Some(&Value::Bool(true)),
        author: text(value, "/author/username"),
        created_at: text(value, "/created_at"),
    }
}

impl ForgeClient<'_> {
    /// Pull request `number` of `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show it, and
    /// network, status, JSON and rate-limit errors.
    pub fn pull_request(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
    ) -> Result<PullRequest, ForgeError> {
        let base = Self::repo_api(repo, kind);
        let url = match kind {
            ForgeKind::Gitlab => format!("{base}/merge_requests/{number}"),
            ForgeKind::Github | ForgeKind::Gitea => format!("{base}/pulls/{number}"),
        };
        let value = self.json(&url, repo, kind)?.ok_or_else(|| {
            ForgeError::NotVisible(format!("{}/{}#{number}", repo.host, repo.path))
        })?;
        Ok(match kind {
            ForgeKind::Gitlab => gitlab(&value),
            ForgeKind::Github | ForgeKind::Gitea => github_like(&value),
        })
    }
}

impl ForgeClient<'_> {
    /// The commits of pull request `number` (the first hundred), in the
    /// forge's order.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show it, and
    /// network, status, JSON and rate-limit errors.
    pub fn pull_request_commits(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
    ) -> Result<Vec<String>, ForgeError> {
        let base = Self::repo_api(repo, kind);
        let url = match kind {
            ForgeKind::Gitlab => format!("{base}/merge_requests/{number}/commits?per_page=100"),
            ForgeKind::Github => format!("{base}/pulls/{number}/commits?per_page=100"),
            ForgeKind::Gitea => format!("{base}/pulls/{number}/commits?limit=50"),
        };
        let value = self.json(&url, repo, kind)?.ok_or_else(|| {
            ForgeError::NotVisible(format!("{}/{}#{number}", repo.host, repo.path))
        })?;
        Ok(value
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|commit| {
                commit
                    .get("sha")
                    .or_else(|| commit.get("id"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned)
            .collect())
    }

    /// The open pull requests of `repo` that target branch `base` (the
    /// first hundred).
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the
    /// repository, and network, status, JSON and rate-limit errors.
    pub fn open_pull_requests(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        base: &str,
    ) -> Result<Vec<PullRequest>, ForgeError> {
        let api = Self::repo_api(repo, kind);
        let url = match kind {
            ForgeKind::Gitlab => {
                format!("{api}/merge_requests?state=opened&target_branch={base}&per_page=100")
            }
            ForgeKind::Github => format!("{api}/pulls?state=open&base={base}&per_page=100"),
            ForgeKind::Gitea => format!("{api}/pulls?state=open&limit=50"),
        };
        let value = self
            .json(&url, repo, kind)?
            .ok_or_else(|| ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path)))?;
        let parse = match kind {
            ForgeKind::Gitlab => gitlab,
            ForgeKind::Github | ForgeKind::Gitea => github_like,
        };
        Ok(value
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(parse)
            .filter(|pr| pr.base == base)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpResponse;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};
    use std::time::Duration;

    fn no_sleep(_: Duration) {}

    fn pull(
        url: &str,
        kind: ForgeKind,
        status: u16,
        body: &str,
    ) -> (Result<PullRequest, ForgeError>, Vec<String>) {
        let http = Scripted::new(vec![Ok(reply(status, body))]);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let result = client.pull_request(&RepoUrl::parse(url).unwrap(), kind, 7);
        (result, http.urls())
    }

    #[test]
    fn github_pull_requests() {
        let open = r#"{"number":7,"title":"[17.0][FIX] pos","state":"open","merged":false,"merged_at":null,
            "head":{"sha":"abc","ref":"17.0-fix","repo":{"html_url":"https://github.com/me/pos"}},
            "maintainer_can_modify":true,"base":{"ref":"17.0"},"labels":[{"name":"needs review"},{"name":"bug"}],
            "user":{"login":"joe"},"created_at":"2026-01-01T00:00:00Z"}"#;
        let (pr, urls) = pull("https://github.com/OCA/pos", ForgeKind::Github, 200, open);
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                number: 7,
                title: "[17.0][FIX] pos".to_owned(),
                state: PrState::Open,
                head_sha: "abc".to_owned(),
                base: "17.0".to_owned(),
                merged_at: None,
                labels: vec!["bug".to_owned(), "needs review".to_owned()],
                head_repository: Some("https://github.com/me/pos".to_owned()),
                head_branch: "17.0-fix".to_owned(),
                maintainer_can_modify: true,
                author: "joe".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            }
        );
        assert_eq!(urls, ["https://api.github.com/repos/OCA/pos/pulls/7"]);
        let merged = r#"{"state":"closed","merged_at":"2026-01-02T03:04:05Z","head":{"sha":"d"}}"#;
        let (pr, urls) = pull("https://ghe.corp/o/r", ForgeKind::Github, 200, merged);
        let pr = pr.unwrap();
        assert_eq!(pr.state, PrState::Merged);
        assert_eq!(pr.merged_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(pr.labels, Vec::<String>::new());
        assert_eq!(urls, ["https://ghe.corp/api/v3/repos/o/r/pulls/7"]);
        let closed = r#"{"state":"closed","merged":false,"merged_at":null}"#;
        let (pr, _) = pull("https://github.com/o/r", ForgeKind::Github, 200, closed);
        assert_eq!(pr.unwrap().state, PrState::Closed);
        assert_eq!(
            [PrState::Open, PrState::Merged, PrState::Closed].map(PrState::name),
            ["open", "merged", "closed"]
        );
    }

    #[test]
    fn gitea_and_gitlab_pull_requests() {
        let gitea = r#"{"state":"closed","merged":true,"merged_at":"2026-02-01T00:00:00+01:00","head":{"sha":"e"},"base":{"ref":"18.0"}}"#;
        let (pr, urls) = pull("https://codeberg.org/o/r", ForgeKind::Gitea, 200, gitea);
        assert_eq!(pr.unwrap().state, PrState::Merged);
        assert_eq!(urls, ["https://codeberg.org/api/v1/repos/o/r/pulls/7"]);
        for (state, expected) in [
            ("opened", PrState::Open),
            ("merged", PrState::Merged),
            ("closed", PrState::Closed),
            ("locked", PrState::Closed),
        ] {
            let body = format!(
                r#"{{"title":"t","state":"{state}","sha":"f","target_branch":"16.0","labels":["b","a",1],"author":{{"username":"ann"}}}}"#
            );
            let (pr, urls) = pull("https://gitlab.com/g/sub/r", ForgeKind::Gitlab, 200, &body);
            let pr = pr.unwrap();
            assert_eq!(
                (pr.state, pr.head_sha.as_str(), pr.base.as_str()),
                (expected, "f", "16.0")
            );
            assert_eq!(pr.labels, ["a", "b"]);
            assert_eq!(pr.author, "ann");
            assert_eq!(
                urls,
                ["https://gitlab.com/api/v4/projects/g%2Fsub%2Fr/merge_requests/7"]
            );
        }
        let (pr, _) = pull("https://gitlab.com/g/r", ForgeKind::Gitlab, 200, "{}");
        assert_eq!(pr.unwrap().labels, Vec::<String>::new());
    }

    #[test]
    fn pull_request_commits_per_forge() {
        let list = |url: &str, kind: ForgeKind, status: u16, body: &str| {
            let http = Scripted::new(vec![Ok(reply(status, body))]);
            let env = MapEnv::default();
            let runner = ScriptedRunner::default();
            let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
            let result = client.pull_request_commits(&RepoUrl::parse(url).unwrap(), kind, 7);
            (result, http.urls())
        };
        let (commits, urls) = list(
            "https://github.com/OCA/pos",
            ForgeKind::Github,
            200,
            r#"[{"sha":"a"},{"sha":"b"},{}]"#,
        );
        assert_eq!(commits.unwrap(), ["a", "b"]);
        assert_eq!(
            urls,
            ["https://api.github.com/repos/OCA/pos/pulls/7/commits?per_page=100"]
        );
        let (commits, urls) = list(
            "https://gitlab.com/g/r",
            ForgeKind::Gitlab,
            200,
            r#"[{"id":"c"}]"#,
        );
        assert_eq!(commits.unwrap(), ["c"]);
        assert_eq!(
            urls,
            ["https://gitlab.com/api/v4/projects/g%2Fr/merge_requests/7/commits?per_page=100"]
        );
        let (commits, urls) = list("https://codeberg.org/o/r", ForgeKind::Gitea, 200, "{}");
        assert_eq!(commits.unwrap(), Vec::<String>::new());
        assert_eq!(
            urls,
            ["https://codeberg.org/api/v1/repos/o/r/pulls/7/commits?limit=50"]
        );
        let (commits, _) = list("https://codeberg.org/o/r", ForgeKind::Gitea, 404, "");
        assert_eq!(
            commits,
            Err(ForgeError::NotVisible("codeberg.org/o/r#7".to_owned()))
        );
        let http = Scripted::new(vec![Err(ForgeError::Http("down".to_owned()))]);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let repo = RepoUrl::parse("https://github.com/OCA/pos").unwrap();
        assert_eq!(
            client.pull_request_commits(&repo, ForgeKind::Github, 7),
            Err(ForgeError::Http("down".to_owned()))
        );
    }

    #[test]
    fn open_pull_requests_per_forge() {
        let list = |url: &str, kind: ForgeKind, status: u16, body: &str| {
            let http = Scripted::new(vec![Ok(reply(status, body))]);
            let env = MapEnv::default();
            let runner = ScriptedRunner::default();
            let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
            let result = client.open_pull_requests(&RepoUrl::parse(url).unwrap(), kind, "19.0");
            (result, http.urls())
        };
        let github = r#"[{"number":3,"title":"[19.0][MIG] pos_x","state":"open","base":{"ref":"19.0"}},
            {"number":4,"title":"other","state":"open","base":{"ref":"18.0"}}]"#;
        let (prs, urls) = list("https://github.com/OCA/pos", ForgeKind::Github, 200, github);
        let prs = prs.unwrap();
        assert_eq!((prs.len(), prs[0].number), (1, 3));
        assert_eq!(
            urls,
            ["https://api.github.com/repos/OCA/pos/pulls?state=open&base=19.0&per_page=100"]
        );
        let (prs, urls) = list("https://codeberg.org/o/r", ForgeKind::Gitea, 200, github);
        assert_eq!(prs.unwrap().len(), 1);
        assert_eq!(
            urls,
            ["https://codeberg.org/api/v1/repos/o/r/pulls?state=open&limit=50"]
        );
        let gitlab =
            r#"[{"iid":9,"title":"[19.0][MIG] x","state":"opened","target_branch":"19.0"}]"#;
        let (prs, urls) = list("https://gitlab.com/g/r", ForgeKind::Gitlab, 200, gitlab);
        assert_eq!(prs.unwrap()[0].number, 9);
        assert_eq!(
            urls,
            [
                "https://gitlab.com/api/v4/projects/g%2Fr/merge_requests?state=opened&target_branch=19.0&per_page=100"
            ]
        );
        let (prs, _) = list("https://github.com/o/r", ForgeKind::Github, 200, "{}");
        assert_eq!(prs.unwrap(), Vec::new());
        let (prs, _) = list("https://github.com/o/r", ForgeKind::Github, 500, "");
        assert!(prs.is_err());
        let (prs, _) = list("https://github.com/o/r", ForgeKind::Github, 404, "");
        assert!(
            prs.unwrap_err()
                .to_string()
                .starts_with("cannot see github.com/o/r")
        );
    }

    #[test]
    fn rate_limits_are_waited_out() {
        let limited = HttpResponse {
            status: 429,
            headers: vec![("retry-after".to_owned(), "1".to_owned())],
            body: Vec::new(),
        };
        let http = Scripted::new(vec![Ok(limited), Ok(reply(200, r#"{"state":"open"}"#))]);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let pr = client.pull_request(
            &RepoUrl::parse("https://github.com/o/r").unwrap(),
            ForgeKind::Github,
            1,
        );
        assert_eq!(pr.unwrap().state, PrState::Open);
    }

    #[test]
    fn missing_pull_requests() {
        let (pr, _) = pull("https://github.com/o/r", ForgeKind::Github, 500, "");
        assert_eq!(pr.unwrap_err().to_string(), "github.com answered HTTP 500");
        let (pr, _) = pull("https://github.com/o/r", ForgeKind::Github, 404, "");
        assert_eq!(
            pr.unwrap_err().to_string(),
            "cannot see github.com/o/r#7: it does not exist, or the token for its host cannot read it"
        );
    }
}
