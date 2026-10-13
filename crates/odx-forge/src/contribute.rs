//! Contributing to a repository: who the token belongs to, whether it may
//! push, the user's fork, and opening a pull (or merge) request.

use serde_json::{Value, json};

use crate::admin::api;
use crate::{ForgeClient, ForgeError, ForgeKind, HttpResponse, RepoUrl};

/// A pull request opened by [`ForgeClient::open_pull_request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Its number (`iid` on GitLab).
    pub number: u64,
    /// Its page.
    pub url: String,
}

/// What [`ForgeClient::open_pull_request`] opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal<'a> {
    /// The repository the branch is in: the target or a fork of it.
    pub head: &'a RepoUrl,
    /// The branch.
    pub branch: &'a str,
    /// The branch it targets.
    pub base: &'a str,
    /// The title.
    pub title: &'a str,
    /// The description, in Markdown.
    pub body: &'a str,
    /// Open it as a draft (a `WIP:` title on Gitea/Forgejo).
    pub draft: bool,
}

/// The JSON of a `200` or `201` answer.
fn created(response: &HttpResponse, repo: &RepoUrl) -> Result<Value, ForgeError> {
    if !matches!(response.status, 200..=202) {
        return Err(ForgeError::Status {
            host: repo.host.clone(),
            status: response.status,
        });
    }
    serde_json::from_slice(&response.body).map_err(|_| ForgeError::Json(repo.host.clone()))
}

/// The repository a forge answer describes, from its page.
fn page(value: &Value, repo: &RepoUrl) -> Result<RepoUrl, ForgeError> {
    ["html_url", "web_url"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .and_then(RepoUrl::parse)
        .ok_or_else(|| ForgeError::Json(repo.host.clone()))
}

impl ForgeClient<'_> {
    /// The login of the user the token for `repo`'s host belongs to.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TokenRequired`] without a token the forge accepts,
    /// and network, status and JSON errors.
    pub fn login(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<String, ForgeError> {
        let user = self
            .json(&format!("{}/user", api(repo, kind)), repo, kind)
            .map_err(|e| match e {
                ForgeError::Status { status: 401, .. } => {
                    ForgeError::TokenRequired(repo.host.clone())
                }
                other => other,
            })?
            .unwrap_or_default();
        ["login", "username"]
            .iter()
            .find_map(|key| user.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| ForgeError::TokenRequired(repo.host.clone()))
    }

    /// Whether the token may push to `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the
    /// repository, and network, status and JSON errors.
    pub fn can_push(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<bool, ForgeError> {
        let value = self
            .json(&Self::repo_api(repo, kind), repo, kind)?
            .ok_or_else(|| ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path)))?;
        let permissions = value.get("permissions");
        let access = |key: &str| {
            permissions
                .and_then(|p| p.pointer(&format!("/{key}/access_level")))
                .and_then(Value::as_u64)
                .unwrap_or_default()
        };
        Ok(match kind {
            // Developer (30) and up may push.
            ForgeKind::Gitlab => access("project_access").max(access("group_access")) >= 30,
            ForgeKind::Github | ForgeKind::Gitea => permissions
                .and_then(|p| p.get("push"))
                .and_then(Value::as_bool)
                .unwrap_or_default(),
        })
    }

    /// The user's fork of `repo`, created when missing.
    ///
    /// # Errors
    ///
    /// [`ForgeError::TokenRequired`] without a token, and network, status
    /// and JSON errors.
    pub fn fork(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<RepoUrl, ForgeError> {
        let login = self.login(repo, kind)?;
        let name = repo.path.rsplit('/').next().unwrap_or_default();
        let mine = RepoUrl {
            path: format!("{login}/{name}"),
            ..repo.clone()
        };
        if let Some(existing) = self.json(&Self::repo_api(&mine, kind), repo, kind)? {
            return page(&existing, repo);
        }
        let response = self.post(
            &format!(
                "{}/{}",
                Self::repo_api(repo, kind),
                match kind {
                    ForgeKind::Gitlab => "fork",
                    ForgeKind::Github | ForgeKind::Gitea => "forks",
                }
            ),
            repo,
            kind,
            &json!({}),
        )?;
        page(&created(&response, repo)?, repo)
    }

    /// Open a pull request from `proposal.branch` of `proposal.head` into
    /// `proposal.base` of `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses (one is already open,
    /// no permission), and network and JSON errors.
    pub fn open_pull_request(
        &self,
        repo: &RepoUrl,
        kind: ForgeKind,
        proposal: &Proposal<'_>,
    ) -> Result<Opened, ForgeError> {
        let same = proposal.head.path == repo.path;
        let owner = proposal
            .head
            .path
            .rsplit_once('/')
            .map_or("", |(owner, _)| owner);
        let head = if same {
            proposal.branch.to_owned()
        } else {
            format!("{owner}:{}", proposal.branch)
        };
        let (url, body) = match kind {
            ForgeKind::Github => (
                format!("{}/pulls", Self::repo_api(repo, kind)),
                json!({
                    "title": proposal.title, "body": proposal.body, "head": head,
                    "base": proposal.base, "draft": proposal.draft,
                }),
            ),
            ForgeKind::Gitea => (
                format!("{}/pulls", Self::repo_api(repo, kind)),
                json!({
                    "title": draft_title("WIP: ", proposal),
                    "body": proposal.body, "head": head, "base": proposal.base,
                }),
            ),
            ForgeKind::Gitlab => {
                let target = self
                    .json(&Self::repo_api(repo, kind), repo, kind)?
                    .ok_or_else(|| {
                        ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path))
                    })?;
                (
                    format!("{}/merge_requests", Self::repo_api(proposal.head, kind)),
                    json!({
                        "title": draft_title("Draft: ", proposal),
                        "description": proposal.body,
                        "source_branch": proposal.branch,
                        "target_branch": proposal.base,
                        "target_project_id": target.get("id").cloned().unwrap_or_default(),
                    }),
                )
            }
        };
        let value = created(&self.post(&url, repo, kind, &body)?, repo)?;
        let number = ["number", "iid"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_u64));
        let url = ["html_url", "web_url"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str));
        match (number, url) {
            (Some(number), Some(url)) => Ok(Opened {
                number,
                url: url.to_owned(),
            }),
            _ => Err(ForgeError::Json(repo.host.clone())),
        }
    }
}

/// The title, with `prefix` for a draft.
fn draft_title(prefix: &str, proposal: &Proposal<'_>) -> String {
    let prefix = if proposal.draft { prefix } else { "" };
    format!("{prefix}{}", proposal.title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};

    fn client_run<T>(
        answers: Vec<(u16, &str)>,
        run: impl FnOnce(&ForgeClient<'_>) -> T,
    ) -> (T, Vec<String>) {
        let http = Scripted::new(
            answers
                .into_iter()
                .map(|(status, body)| Ok(reply(status, body)))
                .collect(),
        );
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
        let result = run(&client);
        (result, http.urls())
    }

    fn repo(url: &str) -> RepoUrl {
        RepoUrl::parse(url).unwrap()
    }

    #[test]
    fn logins() {
        let github = repo("https://github.com/OCA/pos");
        let (login, urls) = client_run(vec![(200, r#"{"login":"me"}"#)], |c| {
            c.login(&github, ForgeKind::Github)
        });
        assert_eq!(login, Ok("me".to_owned()));
        assert_eq!(urls, ["https://api.github.com/user"]);
        let gitlab = repo("https://gitlab.com/g/r");
        let (login, _) = client_run(vec![(200, r#"{"username":"me"}"#)], |c| {
            c.login(&gitlab, ForgeKind::Gitlab)
        });
        assert_eq!(login, Ok("me".to_owned()));
        for (status, body) in [(401, ""), (200, "{}")] {
            let (login, _) = client_run(vec![(status, body)], |c| {
                c.login(&github, ForgeKind::Github)
            });
            assert_eq!(
                login,
                Err(ForgeError::TokenRequired("github.com".to_owned()))
            );
        }
        let (login, _) = client_run(vec![(500, "")], |c| c.login(&github, ForgeKind::Github));
        assert_eq!(
            login,
            Err(ForgeError::Status {
                host: "github.com".to_owned(),
                status: 500
            })
        );
    }

    #[test]
    fn push_permissions() {
        let github = repo("https://github.com/OCA/pos");
        let gitlab = repo("https://gitlab.com/g/r");
        for (target, kind, body, expected) in [
            (
                &github,
                ForgeKind::Github,
                r#"{"permissions":{"push":true}}"#,
                true,
            ),
            (
                &github,
                ForgeKind::Gitea,
                r#"{"permissions":{"push":false}}"#,
                false,
            ),
            (&github, ForgeKind::Github, "{}", false),
            (
                &gitlab,
                ForgeKind::Gitlab,
                r#"{"permissions":{"project_access":null,"group_access":{"access_level":40}}}"#,
                true,
            ),
            (
                &gitlab,
                ForgeKind::Gitlab,
                r#"{"permissions":{"project_access":{"access_level":20}}}"#,
                false,
            ),
        ] {
            let (answer, _) = client_run(vec![(200, body)], |c| c.can_push(target, kind));
            assert_eq!(answer, Ok(expected), "{body}");
        }
        let (answer, _) = client_run(vec![], |c| c.can_push(&github, ForgeKind::Github));
        assert_eq!(
            answer,
            Err(ForgeError::NotVisible("github.com/OCA/pos".to_owned()))
        );
    }

    #[test]
    fn forks() {
        let github = repo("https://github.com/OCA/pos");
        // An existing fork is used.
        let (fork, urls) = client_run(
            vec![
                (200, r#"{"login":"me"}"#),
                (200, r#"{"html_url":"https://github.com/me/pos"}"#),
            ],
            |c| c.fork(&github, ForgeKind::Github),
        );
        assert_eq!(fork.unwrap().path, "me/pos");
        assert_eq!(urls[1], "https://api.github.com/repos/me/pos");
        // A missing one is created.
        let (fork, urls) = client_run(
            vec![
                (200, r#"{"login":"me"}"#),
                (404, ""),
                (202, r#"{"html_url":"https://github.com/me/pos"}"#),
            ],
            |c| c.fork(&github, ForgeKind::Github),
        );
        assert_eq!(fork.unwrap().path, "me/pos");
        assert!(urls[2].starts_with("POST https://api.github.com/repos/OCA/pos/forks "));
        let gitlab = repo("https://gitlab.com/g/r");
        let (fork, urls) = client_run(
            vec![
                (200, r#"{"username":"me"}"#),
                (404, ""),
                (201, r#"{"web_url":"https://gitlab.com/me/r"}"#),
            ],
            |c| c.fork(&gitlab, ForgeKind::Gitlab),
        );
        assert_eq!(fork.unwrap().path, "me/r");
        assert!(urls[2].starts_with("POST https://gitlab.com/api/v4/projects/g%2Fr/fork "));
        // Refusals and odd answers.
        for (status, body) in [(403, ""), (202, "not json"), (202, "{}")] {
            let (fork, _) = client_run(
                vec![(200, r#"{"login":"me"}"#), (404, ""), (status, body)],
                |c| c.fork(&github, ForgeKind::Github),
            );
            assert!(fork.is_err(), "{status} {body}");
        }
        let (fork, _) = client_run(vec![(401, "")], |c| c.fork(&github, ForgeKind::Github));
        assert!(fork.is_err());
        let (fork, _) = client_run(vec![(200, r#"{"login":"me"}"#), (500, "")], |c| {
            c.fork(&github, ForgeKind::Github)
        });
        assert!(fork.is_err());
    }

    #[test]
    fn network_failures() {
        let down = || Err(ForgeError::Http("down".to_owned()));
        let ok = |body: &str| Ok(reply(200, body));
        let github = repo("https://github.com/OCA/pos");
        let gitlab = repo("https://gitlab.com/g/r");
        let mine = repo("https://github.com/me/pos");
        let proposal = Proposal {
            head: &mine,
            branch: "b",
            base: "19.0",
            title: "t",
            body: "",
            draft: false,
        };
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let failed =
            |answers: Vec<Result<HttpResponse, ForgeError>>,
             run: &dyn Fn(&ForgeClient<'_>) -> Result<(), ForgeError>| {
                let http = Scripted::new(answers);
                let client = ForgeClient::new(&http, &env, &runner, &std::thread::sleep);
                assert_eq!(run(&client), Err(ForgeError::Http("down".to_owned())));
            };
        failed(vec![down()], &|c| {
            c.can_push(&github, ForgeKind::Github).map(drop)
        });
        failed(
            vec![ok(r#"{"login":"me"}"#), Ok(reply(404, "")), down()],
            &|c| c.fork(&github, ForgeKind::Github).map(drop),
        );
        failed(vec![down()], &|c| {
            c.open_pull_request(&github, ForgeKind::Github, &proposal)
                .map(drop)
        });
        failed(vec![down()], &|c| {
            c.open_pull_request(&gitlab, ForgeKind::Gitlab, &proposal)
                .map(drop)
        });
    }

    #[test]
    fn pull_requests() {
        let github = repo("https://github.com/OCA/pos");
        let mine = repo("https://github.com/me/pos");
        let proposal = |head| Proposal {
            head,
            branch: "19.0-mig-pos_x",
            base: "19.0",
            title: "[19.0][MIG] pos_x: Migration to 19.0",
            body: "body",
            draft: true,
        };
        let (opened, urls) = client_run(
            vec![(
                201,
                r#"{"number":5,"html_url":"https://github.com/OCA/pos/pull/5"}"#,
            )],
            |c| c.open_pull_request(&github, ForgeKind::Github, &proposal(&mine)),
        );
        assert_eq!(
            opened,
            Ok(Opened {
                number: 5,
                url: "https://github.com/OCA/pos/pull/5".to_owned()
            })
        );
        assert!(urls[0].starts_with("POST https://api.github.com/repos/OCA/pos/pulls "));
        assert!(urls[0].contains(r#""head":"me:19.0-mig-pos_x""#));
        assert!(urls[0].contains(r#""draft":true"#));

        // Gitea marks drafts in the title; a branch of the target itself.
        let gitea = repo("https://codeberg.org/o/r");
        let ready = Proposal {
            draft: false,
            ..proposal(&gitea)
        };
        for (proposal, title) in [
            (proposal(&gitea), "WIP: [19.0]"),
            (ready, "\"title\":\"[19.0]"),
        ] {
            let (opened, urls) = client_run(
                vec![(
                    201,
                    r#"{"number":2,"html_url":"https://codeberg.org/o/r/pulls/2"}"#,
                )],
                |c| c.open_pull_request(&gitea, ForgeKind::Gitea, &proposal),
            );
            assert_eq!(opened.unwrap().number, 2);
            assert!(urls[0].contains(r#""head":"19.0-mig-pos_x""#));
            assert!(urls[0].contains(title));
        }

        // GitLab opens the merge request on the fork, aimed at the target.
        let gitlab = repo("https://gitlab.com/g/r");
        let fork = repo("https://gitlab.com/me/r");
        let (opened, urls) = client_run(
            vec![
                (200, r#"{"id":42}"#),
                (
                    201,
                    r#"{"iid":3,"web_url":"https://gitlab.com/g/r/-/merge_requests/3"}"#,
                ),
            ],
            |c| c.open_pull_request(&gitlab, ForgeKind::Gitlab, &proposal(&fork)),
        );
        assert_eq!(opened.unwrap().number, 3);
        assert!(
            urls[1].starts_with("POST https://gitlab.com/api/v4/projects/me%2Fr/merge_requests ")
        );
        assert!(urls[1].contains(r#""target_project_id":42"#));
        assert!(urls[1].contains("Draft: [19.0]"));
        let (opened, _) = client_run(vec![], |c| {
            c.open_pull_request(&gitlab, ForgeKind::Gitlab, &proposal(&fork))
        });
        assert_eq!(
            opened,
            Err(ForgeError::NotVisible("gitlab.com/g/r".to_owned()))
        );

        // Refused, or an answer without number or page.
        for (status, body) in [(422, ""), (201, "{}")] {
            let (opened, _) = client_run(vec![(status, body)], |c| {
                c.open_pull_request(&github, ForgeKind::Github, &proposal(&mine))
            });
            assert!(opened.is_err(), "{status} {body}");
        }
    }
}
