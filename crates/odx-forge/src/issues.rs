//! Issues, labels and comments, for the bot's dashboard issue and its pull
//! requests (GitHub and Gitea/Forgejo; on both, a pull request is also an
//! issue).

use serde_json::{Value, json};

use crate::contribute::created;
use crate::{ForgeClient, ForgeError, ForgeKind, RepoUrl};

/// Whether an issue is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueState {
    /// Open.
    Open,
    /// Closed (merged, for a pull request).
    Closed,
}

impl IssueState {
    /// `open` or `closed`, as both forges spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// An issue, or a pull request seen as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// Its number.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// Its description, in Markdown.
    pub body: String,
    /// Open or closed.
    pub state: IssueState,
    /// Its labels, sorted.
    pub labels: Vec<String>,
    /// Its page.
    pub url: String,
    /// Whether it is a pull request.
    pub pull: bool,
}

/// A comment on an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Its id.
    pub id: u64,
    /// Who wrote it.
    pub author: String,
    /// What it says, in Markdown.
    pub body: String,
}

/// What [`ForgeClient::edit_issue`] changes; `None` keeps it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IssueEdit<'a> {
    /// A new title.
    pub title: Option<&'a str>,
    /// A new description.
    pub body: Option<&'a str>,
    /// Open or close it.
    pub state: Option<IssueState>,
}

fn text(value: &Value, pointer: &str) -> String {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn issue(value: &Value) -> Issue {
    let mut labels: Vec<String> = value
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| labels.iter().map(|label| text(label, "/name")).collect())
        .unwrap_or_default();
    labels.sort();
    Issue {
        number: value
            .get("number")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        title: text(value, "/title"),
        body: text(value, "/body"),
        state: if text(value, "/state") == "open" {
            IssueState::Open
        } else {
            IssueState::Closed
        },
        labels,
        url: text(value, "/html_url"),
        pull: value.get("pull_request").is_some_and(Value::is_object),
    }
}

fn comment(value: &Value) -> Comment {
    Comment {
        id: value.get("id").and_then(Value::as_u64).unwrap_or_default(),
        author: text(value, "/user/login"),
        body: text(value, "/body"),
    }
}

/// The repository's issue API, or why there is none.
fn issues_api(repo: &RepoUrl, kind: ForgeKind) -> Result<String, ForgeError> {
    match kind {
        ForgeKind::Github | ForgeKind::Gitea => {
            Ok(format!("{}/issues", ForgeClient::repo_api(repo, kind)))
        }
        ForgeKind::Gitlab => Err(ForgeError::Unsupported(format!(
            "{}: issues and labels are not supported on GitLab yet",
            repo.host
        ))),
    }
}

/// A `2xx` answer without a body worth reading.
fn done(status: u16, repo: &RepoUrl) -> Result<(), ForgeError> {
    if matches!(status, 200..=204) {
        Ok(())
    } else {
        Err(ForgeError::Status {
            host: repo.host.clone(),
            status,
        })
    }
}

impl ForgeClient<'_> {
    /// The issues (or, with `pulls`, the pull requests) of `repo` that
    /// carry `label`, open and closed, newest first (the first hundred).
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the
    /// repository, [`ForgeError::Unsupported`] on GitLab, and network,
    /// status, JSON and rate-limit errors.
    pub fn labelled(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        label: &str,
        pulls: bool,
    ) -> Result<Vec<Issue>, ForgeError> {
        let api = issues_api(repo, kind)?;
        let url = match kind {
            ForgeKind::Gitea => format!(
                "{api}?state=all&labels={label}&type={}&limit=50",
                if pulls { "pulls" } else { "issues" }
            ),
            _ => format!("{api}?state=all&labels={label}&per_page=100"),
        };
        let value = self
            .json(&url, repo, kind)?
            .ok_or_else(|| ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path)))?;
        Ok(value
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(issue)
            .filter(|found| found.pull == pulls && found.labels.iter().any(|l| l == label))
            .collect())
    }

    /// Open an issue on `repo` with `labels`, created when missing.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses, [`ForgeError::Unsupported`]
    /// on GitLab, and network and JSON errors.
    pub fn create_issue(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<Issue, ForgeError> {
        let api = issues_api(repo, kind)?;
        let labels = self.label_refs(repo, kind, labels)?;
        let response = self.post(
            &api,
            repo,
            kind,
            &json!({"title": title, "body": body, "labels": labels}),
        )?;
        created(&response, repo).map(|value| issue(&value))
    }

    /// Change the title, description or state of issue (or pull request)
    /// `number` of `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses, [`ForgeError::Unsupported`]
    /// on GitLab, and network and JSON errors.
    pub fn edit_issue(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
        edit: &IssueEdit<'_>,
    ) -> Result<Issue, ForgeError> {
        let api = issues_api(repo, kind)?;
        let mut body = serde_json::Map::new();
        if let Some(title) = edit.title {
            body.insert("title".to_owned(), json!(title));
        }
        if let Some(text) = edit.body {
            body.insert("body".to_owned(), json!(text));
        }
        if let Some(state) = edit.state {
            body.insert("state".to_owned(), json!(state.name()));
        }
        let response = self.patch(&format!("{api}/{number}"), repo, kind, &Value::Object(body))?;
        created(&response, repo).map(|value| issue(&value))
    }

    /// Add `labels` to issue (or pull request) `number` of `repo`,
    /// creating them when missing.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses, [`ForgeError::Unsupported`]
    /// on GitLab, and network errors.
    pub fn add_labels(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
        labels: &[&str],
    ) -> Result<(), ForgeError> {
        let api = issues_api(repo, kind)?;
        let labels = self.label_refs(repo, kind, labels)?;
        let response = self.post(
            &format!("{api}/{number}/labels"),
            repo,
            kind,
            &json!({ "labels": labels }),
        )?;
        done(response.status, repo)
    }

    /// The comments on issue `number` of `repo`, oldest first (the first
    /// hundred).
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the issue,
    /// [`ForgeError::Unsupported`] on GitLab, and network, status, JSON and
    /// rate-limit errors.
    pub fn issue_comments(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
    ) -> Result<Vec<Comment>, ForgeError> {
        let api = issues_api(repo, kind)?;
        let url = match kind {
            ForgeKind::Gitea => format!("{api}/{number}/comments"),
            _ => format!("{api}/{number}/comments?per_page=100"),
        };
        let value = self.json(&url, repo, kind)?.ok_or_else(|| {
            ForgeError::NotVisible(format!("{}/{}#{number}", repo.host, repo.path))
        })?;
        Ok(value
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .map(comment)
            .collect())
    }

    /// Comment `body` on issue (or pull request) `number` of `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses, [`ForgeError::Unsupported`]
    /// on GitLab, and network and JSON errors.
    pub fn comment(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        number: u64,
        body: &str,
    ) -> Result<Comment, ForgeError> {
        let api = issues_api(repo, kind)?;
        let response = self.post(
            &format!("{api}/{number}/comments"),
            repo,
            kind,
            &json!({ "body": body }),
        )?;
        created(&response, repo).map(|value| comment(&value))
    }

    /// How the forge wants `labels` named in a request: by name on
    /// GitHub, which creates missing ones itself; by id on Gitea/Forgejo,
    /// creating the missing ones first.
    fn label_refs(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        labels: &[&str],
    ) -> Result<Vec<Value>, ForgeError> {
        if kind != ForgeKind::Gitea || labels.is_empty() {
            return Ok(labels.iter().map(|name| json!(name)).collect());
        }
        let api = format!("{}/labels", Self::repo_api(repo, kind));
        let known = self
            .json(&format!("{api}?limit=50"), repo, kind)?
            .ok_or_else(|| ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path)))?;
        let known = known.as_array().map_or(&[][..], Vec::as_slice);
        labels
            .iter()
            .map(|name| {
                let id = known
                    .iter()
                    .find(|label| label.get("name").and_then(Value::as_str) == Some(name))
                    .map_or_else(
                        || {
                            self.post(&api, repo, kind, &json!({"name": name, "color": "#ededed"}))
                                .and_then(|response| created(&response, repo))
                        },
                        |label| Ok(label.clone()),
                    )?
                    .get("id")
                    .cloned();
                id.ok_or_else(|| ForgeError::Json(repo.host.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpResponse;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};

    fn run<T>(
        answers: Vec<Result<HttpResponse, ForgeError>>,
        call: impl FnOnce(&ForgeClient<'_>) -> T,
    ) -> (T, Vec<String>) {
        let http = Scripted::new(answers);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
        let result = call(&client);
        (result, http.urls())
    }

    fn github() -> RepoUrl {
        RepoUrl::parse("https://github.com/acme/project").unwrap()
    }

    fn gitea() -> RepoUrl {
        RepoUrl::parse("https://git.acme.test/acme/project").unwrap()
    }

    const LISTED: &str = r#"[
        {"number":1,"title":"odx upstream dashboard (17.0)","body":"<!-- odx:dashboard series=17.0 -->",
         "state":"open","html_url":"https://github.com/acme/project/issues/1",
         "labels":[{"name":"odx:dashboard"},{"name":"bot"}]},
        {"number":2,"title":"a pull","state":"closed","labels":[{"name":"odx:dashboard"}],
         "pull_request":{"url":"x"}},
        {"number":3,"title":"unlabelled","state":"open","labels":[]}
    ]"#;

    #[test]
    fn lists_by_label() {
        let (found, urls) = run(vec![Ok(reply(200, LISTED))], |c| {
            c.labelled(&github(), ForgeKind::Github, "odx:dashboard", false)
        });
        assert_eq!(
            found.unwrap(),
            [Issue {
                number: 1,
                title: "odx upstream dashboard (17.0)".to_owned(),
                body: "<!-- odx:dashboard series=17.0 -->".to_owned(),
                state: IssueState::Open,
                labels: vec!["bot".to_owned(), "odx:dashboard".to_owned()],
                url: "https://github.com/acme/project/issues/1".to_owned(),
                pull: false,
            }]
        );
        assert_eq!(
            urls,
            [
                "https://api.github.com/repos/acme/project/issues?state=all&labels=odx:dashboard&per_page=100"
            ]
        );
        let (found, urls) = run(vec![Ok(reply(200, LISTED))], |c| {
            c.labelled(&gitea(), ForgeKind::Gitea, "odx:dashboard", true)
        });
        let found = found.unwrap();
        assert_eq!(
            (found.len(), found[0].number, found[0].state),
            (1, 2, IssueState::Closed)
        );
        assert_eq!(
            urls,
            [
                "https://git.acme.test/api/v1/repos/acme/project/issues?state=all&labels=odx:dashboard&type=pulls&limit=50"
            ]
        );
        let (found, urls) = run(vec![Ok(reply(200, "[]"))], |c| {
            c.labelled(&gitea(), ForgeKind::Gitea, "odx:dashboard", false)
        });
        assert_eq!(found.unwrap(), []);
        assert!(urls[0].contains("&type=issues&"));
        let (found, _) = run(vec![Ok(reply(200, "{}"))], |c| {
            c.labelled(&github(), ForgeKind::Github, "x", false)
        });
        assert_eq!(found.unwrap(), []);
        let (found, _) = run(vec![], |c| {
            c.labelled(&github(), ForgeKind::Github, "x", false)
        });
        assert_eq!(
            found,
            Err(ForgeError::NotVisible("github.com/acme/project".to_owned()))
        );
        assert_eq!(IssueState::Closed.name(), "closed");
        let (found, _) = run(vec![Err(ForgeError::Http("down".to_owned()))], |c| {
            c.labelled(&github(), ForgeKind::Github, "x", false)
        });
        assert_eq!(found, Err(ForgeError::Http("down".to_owned())));
        let (found, _) = run(vec![Err(ForgeError::Http("down".to_owned()))], |c| {
            c.issue_comments(&github(), ForgeKind::Github, 1)
        });
        assert_eq!(found, Err(ForgeError::Http("down".to_owned())));
    }

    #[test]
    fn gitlab_is_not_supported() {
        let gitlab = RepoUrl::parse("https://gitlab.com/g/r").unwrap();
        let unsupported: Result<(), ForgeError> = Err(ForgeError::Unsupported(
            "gitlab.com: issues and labels are not supported on GitLab yet".to_owned(),
        ));
        let (found, urls) = run(vec![], |c| {
            c.labelled(&gitlab, ForgeKind::Gitlab, "x", false)
        });
        assert_eq!(found.map(drop), unsupported);
        assert_eq!(urls, Vec::<String>::new());
        let (created, _) = run(vec![], |c| {
            c.create_issue(&gitlab, ForgeKind::Gitlab, "t", "b", &[])
        });
        assert_eq!(created.map(drop), unsupported);
        let (edited, _) = run(vec![], |c| {
            c.edit_issue(&gitlab, ForgeKind::Gitlab, 1, &IssueEdit::default())
        });
        assert_eq!(edited.map(drop), unsupported);
        let (added, _) = run(vec![], |c| {
            c.add_labels(&gitlab, ForgeKind::Gitlab, 1, &["x"])
        });
        assert_eq!(added, unsupported);
        let (comments, _) = run(vec![], |c| c.issue_comments(&gitlab, ForgeKind::Gitlab, 1));
        assert_eq!(comments.map(drop), unsupported);
        let (commented, _) = run(vec![], |c| c.comment(&gitlab, ForgeKind::Gitlab, 1, "b"));
        assert_eq!(commented.map(drop), unsupported);
    }

    const CREATED: &str = r#"{"number":4,"title":"t","body":"b","state":"open",
        "html_url":"https://x/issues/4","labels":[{"name":"odx:dashboard"}]}"#;

    #[test]
    fn comments_on_issues() {
        let (comment, urls) = run(
            vec![Ok(reply(
                201,
                r#"{"id":9,"user":{"login":"odx-bot"},"body":"b"}"#,
            ))],
            |c| c.comment(&github(), ForgeKind::Github, 4, "b"),
        );
        assert_eq!(
            comment.unwrap(),
            Comment {
                id: 9,
                author: "odx-bot".to_owned(),
                body: "b".to_owned(),
            }
        );
        assert_eq!(
            urls,
            [r#"POST https://api.github.com/repos/acme/project/issues/4/comments {"body":"b"}"#]
        );
        let (comment, _) = run(vec![Ok(reply(403, ""))], |c| {
            c.comment(&github(), ForgeKind::Github, 4, "b")
        });
        assert!(matches!(
            comment,
            Err(ForgeError::Status { status: 403, .. })
        ));
    }

    #[test]
    fn creates_issues() {
        let (issue, urls) = run(vec![Ok(reply(201, CREATED))], |c| {
            c.create_issue(&github(), ForgeKind::Github, "t", "b", &["odx:dashboard"])
        });
        assert_eq!(issue.unwrap().number, 4);
        assert_eq!(
            urls,
            [
                r#"POST https://api.github.com/repos/acme/project/issues {"body":"b","labels":["odx:dashboard"],"title":"t"}"#
            ]
        );
        // Gitea names labels by id: a known one is looked up, a missing one
        // created.
        let (issue, urls) = run(
            vec![
                Ok(reply(200, r#"[{"id":7,"name":"odx:dashboard"}]"#)),
                Ok(reply(201, r#"{"id":8,"name":"bot"}"#)),
                Ok(reply(201, CREATED)),
            ],
            |c| {
                c.create_issue(
                    &gitea(),
                    ForgeKind::Gitea,
                    "t",
                    "b",
                    &["odx:dashboard", "bot"],
                )
            },
        );
        assert_eq!(issue.unwrap().number, 4);
        assert_eq!(
            urls,
            [
                "https://git.acme.test/api/v1/repos/acme/project/labels?limit=50",
                r##"POST https://git.acme.test/api/v1/repos/acme/project/labels {"color":"#ededed","name":"bot"}"##,
                r#"POST https://git.acme.test/api/v1/repos/acme/project/issues {"body":"b","labels":[7,8],"title":"t"}"#,
            ]
        );
        // Without labels, none are looked up.
        let (issue, urls) = run(vec![Ok(reply(201, CREATED))], |c| {
            c.create_issue(&gitea(), ForgeKind::Gitea, "t", "b", &[])
        });
        assert_eq!(issue.unwrap().number, 4);
        assert_eq!(urls.len(), 1);
    }

    #[test]
    fn label_failures() {
        let create = |answers| {
            run(answers, |c| {
                c.create_issue(&gitea(), ForgeKind::Gitea, "t", "b", &["bot"])
                    .map(drop)
            })
            .0
        };
        let host = "git.acme.test".to_owned();
        assert_eq!(
            create(vec![]),
            Err(ForgeError::NotVisible(
                "git.acme.test/acme/project".to_owned()
            ))
        );
        assert_eq!(
            create(vec![Ok(reply(200, "{}")), Ok(reply(403, ""))]),
            Err(ForgeError::Status {
                host: host.clone(),
                status: 403
            })
        );
        assert_eq!(
            create(vec![Ok(reply(200, "[]")), Ok(reply(201, "{}"))]),
            Err(ForgeError::Json(host.clone()))
        );
        assert_eq!(
            create(vec![
                Ok(reply(200, r#"[{"id":1,"name":"bot"}]"#)),
                Ok(reply(422, ""))
            ]),
            Err(ForgeError::Status { host, status: 422 })
        );
        let down = || Err(ForgeError::Http("down".to_owned()));
        let (added, _) = run(vec![down()], |c| {
            c.add_labels(&gitea(), ForgeKind::Gitea, 1, &["bot"])
        });
        assert_eq!(added, Err(ForgeError::Http("down".to_owned())));
        let (added, _) = run(vec![down()], |c| {
            c.add_labels(&github(), ForgeKind::Github, 1, &["bot"])
        });
        assert_eq!(added, Err(ForgeError::Http("down".to_owned())));
        let (created, _) = run(vec![down()], |c| {
            c.create_issue(&github(), ForgeKind::Github, "t", "b", &[])
                .map(drop)
        });
        assert_eq!(created, Err(ForgeError::Http("down".to_owned())));
    }

    #[test]
    fn edits_issues() {
        let (edited, urls) = run(vec![Ok(reply(200, CREATED))], |c| {
            c.edit_issue(
                &github(),
                ForgeKind::Github,
                4,
                &IssueEdit {
                    title: Some("t"),
                    body: Some("b"),
                    state: Some(IssueState::Open),
                },
            )
        });
        assert_eq!(edited.unwrap().state, IssueState::Open);
        assert_eq!(
            urls,
            [
                r#"PATCH https://api.github.com/repos/acme/project/issues/4 {"body":"b","state":"open","title":"t"}"#
            ]
        );
        let (edited, urls) = run(vec![Ok(reply(201, CREATED))], |c| {
            c.edit_issue(&gitea(), ForgeKind::Gitea, 4, &IssueEdit::default())
        });
        assert!(edited.is_ok());
        assert_eq!(
            urls,
            ["PATCH https://git.acme.test/api/v1/repos/acme/project/issues/4 {}"]
        );
        let (edited, _) = run(vec![Ok(reply(404, ""))], |c| {
            c.edit_issue(&github(), ForgeKind::Github, 4, &IssueEdit::default())
        });
        assert!(edited.is_err());
        let (edited, _) = run(vec![Err(ForgeError::Http("down".to_owned()))], |c| {
            c.edit_issue(&github(), ForgeKind::Github, 4, &IssueEdit::default())
        });
        assert!(edited.is_err());
    }

    #[test]
    fn adds_labels() {
        let (added, urls) = run(vec![Ok(reply(200, "[]"))], |c| {
            c.add_labels(&github(), ForgeKind::Github, 4, &["odx:add"])
        });
        assert_eq!(added, Ok(()));
        assert_eq!(
            urls,
            [
                r#"POST https://api.github.com/repos/acme/project/issues/4/labels {"labels":["odx:add"]}"#
            ]
        );
        let (added, urls) = run(
            vec![
                Ok(reply(200, r#"[{"id":3,"name":"odx:add"}]"#)),
                Ok(reply(200, "[]")),
            ],
            |c| c.add_labels(&gitea(), ForgeKind::Gitea, 4, &["odx:add"]),
        );
        assert_eq!(added, Ok(()));
        assert_eq!(
            urls[1],
            r#"POST https://git.acme.test/api/v1/repos/acme/project/issues/4/labels {"labels":[3]}"#
        );
        let (added, _) = run(vec![Ok(reply(403, ""))], |c| {
            c.add_labels(&github(), ForgeKind::Github, 4, &["odx:add"])
        });
        assert_eq!(
            added,
            Err(ForgeError::Status {
                host: "github.com".to_owned(),
                status: 403
            })
        );
        let (added, _) = run(vec![], |c| {
            c.add_labels(&gitea(), ForgeKind::Gitea, 4, &["odx:add"])
        });
        assert!(added.is_err());
    }

    #[test]
    fn reads_comments() {
        let comments = r#"[{"id":10,"user":{"login":"dev"},"body":"/odx verdict"},{}]"#;
        let (found, urls) = run(vec![Ok(reply(200, comments))], |c| {
            c.issue_comments(&github(), ForgeKind::Github, 1)
        });
        assert_eq!(
            found.unwrap(),
            [
                Comment {
                    id: 10,
                    author: "dev".to_owned(),
                    body: "/odx verdict".to_owned()
                },
                Comment {
                    id: 0,
                    author: String::new(),
                    body: String::new()
                }
            ]
        );
        assert_eq!(
            urls,
            ["https://api.github.com/repos/acme/project/issues/1/comments?per_page=100"]
        );
        let (found, urls) = run(vec![Ok(reply(200, "{}"))], |c| {
            c.issue_comments(&gitea(), ForgeKind::Gitea, 1)
        });
        assert_eq!(found.unwrap(), []);
        assert_eq!(
            urls,
            ["https://git.acme.test/api/v1/repos/acme/project/issues/1/comments"]
        );
        let (found, _) = run(vec![], |c| {
            c.issue_comments(&github(), ForgeKind::Github, 1)
        });
        assert_eq!(
            found,
            Err(ForgeError::NotVisible(
                "github.com/acme/project#1".to_owned()
            ))
        );
    }
}
