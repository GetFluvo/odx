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
        title: text(value, "/title"),
        state,
        head_sha: text(value, "/head/sha"),
        base: text(value, "/base/ref"),
        merged_at: value
            .get("merged_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        labels,
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
        title: text(value, "/title"),
        state,
        head_sha: text(value, "/sha"),
        base: text(value, "/target_branch"),
        merged_at: value
            .get("merged_at")
            .and_then(Value::as_str)
            .map(str::to_owned),
        labels,
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
        let open = r#"{"title":"[17.0][FIX] pos","state":"open","merged":false,"merged_at":null,
            "head":{"sha":"abc"},"base":{"ref":"17.0"},"labels":[{"name":"needs review"},{"name":"bug"}]}"#;
        let (pr, urls) = pull("https://github.com/OCA/pos", ForgeKind::Github, 200, open);
        assert_eq!(
            pr.unwrap(),
            PullRequest {
                title: "[17.0][FIX] pos".to_owned(),
                state: PrState::Open,
                head_sha: "abc".to_owned(),
                base: "17.0".to_owned(),
                merged_at: None,
                labels: vec!["bug".to_owned(), "needs review".to_owned()],
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
                r#"{{"title":"t","state":"{state}","sha":"f","target_branch":"16.0","labels":["b","a",1]}}"#
            );
            let (pr, urls) = pull("https://gitlab.com/g/sub/r", ForgeKind::Gitlab, 200, &body);
            let pr = pr.unwrap();
            assert_eq!(
                (pr.state, pr.head_sha.as_str(), pr.base.as_str()),
                (expected, "f", "16.0")
            );
            assert_eq!(pr.labels, ["a", "b"]);
            assert_eq!(
                urls,
                ["https://gitlab.com/api/v4/projects/g%2Fsub%2Fr/merge_requests/7"]
            );
        }
        let (pr, _) = pull("https://gitlab.com/g/r", ForgeKind::Gitlab, 200, "{}");
        assert_eq!(pr.unwrap().labels, Vec::<String>::new());
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
