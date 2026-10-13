use serde_json::{Value, json};

use crate::{ForgeClient, ForgeError, ForgeKind, HttpResponse, RepoUrl};

/// The REST API root of `repo`'s forge.
fn api(repo: &RepoUrl, kind: ForgeKind) -> String {
    match (kind, repo.host.as_str()) {
        (ForgeKind::Github, "github.com") => "https://api.github.com".to_owned(),
        (ForgeKind::Github, _) => format!("{}/api/v3", repo.origin()),
        (ForgeKind::Gitlab, _) => format!("{}/api/v4", repo.origin()),
        (ForgeKind::Gitea, _) => format!("{}/api/v1", repo.origin()),
    }
}

fn accepted(response: &HttpResponse, repo: &RepoUrl) -> Result<(), ForgeError> {
    if matches!(response.status, 200 | 201) {
        Ok(())
    } else {
        Err(ForgeError::Status {
            host: repo.host.clone(),
            status: response.status,
        })
    }
}

impl ForgeClient<'_> {
    /// Create `repo` as a private repository: in the organization (or
    /// GitLab group) named by its path, else, when that is no organization,
    /// for the authenticated user.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses (no token, no
    /// permission, name taken), and network errors.
    pub fn create_private(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<(), ForgeError> {
        let (owner, name) = repo.path.rsplit_once('/').unwrap_or(("", &repo.path));
        let api = api(repo, kind);
        if kind == ForgeKind::Gitlab {
            let namespace = self
                .json(
                    &format!("{api}/namespaces/{}", owner.replace('/', "%2F")),
                    repo,
                    kind,
                )?
                .ok_or_else(|| ForgeError::NotVisible(format!("{}/{owner}", repo.host)))?;
            let id = namespace
                .get("id")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            let body =
                json!({ "name": name, "path": name, "namespace_id": id, "visibility": "private" });
            return accepted(
                &self.post(&format!("{api}/projects"), repo, kind, &body)?,
                repo,
            );
        }
        let body = json!({ "name": name, "private": true });
        let response = self.post(&format!("{api}/orgs/{owner}/repos"), repo, kind, &body)?;
        if response.status == 404 {
            return accepted(
                &self.post(&format!("{api}/user/repos"), repo, kind, &body)?,
                repo,
            );
        }
        accepted(&response, repo)
    }

    /// Protect the vault's `odx/*` tags from being deleted or moved: a
    /// GitHub tag ruleset, GitLab protected tags, Gitea/Forgejo tag
    /// protection.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Status`] when the forge refuses, and network errors.
    pub fn protect_vault_tags(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<(), ForgeError> {
        let api = api(repo, kind);
        let path = &repo.path;
        let (url, body) = match kind {
            ForgeKind::Github => (
                format!("{api}/repos/{path}/rulesets"),
                json!({
                    "name": "odx vault",
                    "target": "tag",
                    "enforcement": "active",
                    "conditions": { "ref_name": { "include": ["refs/tags/odx/**"], "exclude": [] } },
                    "rules": [{ "type": "deletion" }, { "type": "update" }, { "type": "non_fast_forward" }],
                }),
            ),
            ForgeKind::Gitlab => (
                format!("{api}/projects/{}/protected_tags", path.replace('/', "%2F")),
                json!({ "name": "odx/*", "create_access_level": 30 }),
            ),
            ForgeKind::Gitea => (
                format!("{api}/repos/{path}/tag_protections"),
                json!({ "name_pattern": "odx/*" }),
            ),
        };
        accepted(&self.post(&url, repo, kind, &body)?, repo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};
    use std::time::Duration;

    fn no_sleep(_: Duration) {}

    fn run(
        url: &str,
        kind: ForgeKind,
        answers: &[u16],
        action: fn(&ForgeClient<'_>, &RepoUrl, ForgeKind) -> Result<(), ForgeError>,
    ) -> (Result<(), ForgeError>, Vec<String>) {
        let http = Scripted::new(
            answers
                .iter()
                .map(|status| Ok(reply(*status, r#"{"id":7}"#)))
                .collect(),
        );
        let env = MapEnv::default().with("GITEA_TOKEN", "t");
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let result = action(&client, &RepoUrl::parse(url).unwrap(), kind);
        (result, http.urls())
    }

    #[test]
    fn private_repositories_are_created() {
        let create =
            |client: &ForgeClient<'_>, repo: &RepoUrl, kind| client.create_private(repo, kind);
        let (result, urls) = run(
            "https://github.com/odoo/vault.git",
            ForgeKind::Github,
            &[201],
            create,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(
            urls,
            [r#"POST https://api.github.com/orgs/odoo/repos {"name":"vault","private":true}"#]
        );
        // Not an organization: the user's own repository.
        let (result, urls) = run(
            "https://forge.corp/me/vault",
            ForgeKind::Gitea,
            &[404, 201],
            create,
        );
        assert_eq!(result, Ok(()));
        assert!(
            urls[1].starts_with("POST https://forge.corp/api/v1/user/repos "),
            "{urls:?}"
        );
        let (result, _) = run(
            "https://ghe.corp/me/vault",
            ForgeKind::Github,
            &[404, 422],
            create,
        );
        assert_eq!(
            result,
            Err(ForgeError::Status {
                host: "ghe.corp".into(),
                status: 422
            })
        );
        let (result, _) = run(
            "https://ghe.corp/odoo/vault",
            ForgeKind::Github,
            &[403],
            create,
        );
        assert_eq!(
            result,
            Err(ForgeError::Status {
                host: "ghe.corp".into(),
                status: 403
            })
        );

        let (result, urls) = run(
            "git@gitlab.com:odoo/group/vault.git",
            ForgeKind::Gitlab,
            &[200, 201],
            create,
        );
        assert_eq!(result, Ok(()));
        assert_eq!(urls[0], "https://gitlab.com/api/v4/namespaces/odoo%2Fgroup");
        assert_eq!(
            urls[1],
            r#"POST https://gitlab.com/api/v4/projects {"name":"vault","namespace_id":7,"path":"vault","visibility":"private"}"#
        );
        let (result, _) = run(
            "https://gitlab.com/nobody/vault",
            ForgeKind::Gitlab,
            &[404],
            create,
        );
        assert_eq!(
            result,
            Err(ForgeError::NotVisible("gitlab.com/nobody".into()))
        );
    }

    /// `action` against an API whose request number `nth` fails to arrive
    /// (the others answer 404 for a GitLab namespace lookup, 404 then
    /// nothing for the organization fallback).
    fn failing(
        action: impl Fn(&ForgeClient<'_>, &RepoUrl) -> Result<(), ForgeError>,
        nth: usize,
    ) -> Result<(), ForgeError> {
        let mut answers: Vec<Result<HttpResponse, ForgeError>> =
            vec![Ok(reply(200, r#"{"id":1}"#)), Ok(reply(404, ""))];
        answers.truncate(nth);
        answers.push(Err(ForgeError::Http("down".into())));
        let http = Scripted::new(answers);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        action(
            &client,
            &RepoUrl::parse("https://forge.corp/odoo/vault").unwrap(),
        )
    }

    #[test]
    fn network_failures_while_creating() {
        let gitlab = |client: &ForgeClient<'_>, repo: &RepoUrl| {
            client.create_private(repo, ForgeKind::Gitlab)
        };
        let gitea = |client: &ForgeClient<'_>, repo: &RepoUrl| {
            client.create_private(repo, ForgeKind::Gitea)
        };
        assert!(failing(gitlab, 0).is_err(), "namespace lookup");
        assert!(failing(gitlab, 1).is_err(), "project creation");
        assert!(failing(gitea, 0).is_err(), "organization repository");
        let protect = |client: &ForgeClient<'_>, repo: &RepoUrl| {
            client.protect_vault_tags(repo, ForgeKind::Gitea)
        };
        assert!(failing(protect, 0).is_err(), "tag protection");
        // The organization is not found; the user's own repository fails.
        let http = Scripted::new(vec![
            Ok(reply(404, "")),
            Err(ForgeError::Http("down".into())),
        ]);
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let repo = RepoUrl::parse("https://forge.corp/odoo/vault").unwrap();
        assert!(client.create_private(&repo, ForgeKind::Gitea).is_err());
    }

    #[test]
    fn vault_tags_are_protected() {
        let protect =
            |client: &ForgeClient<'_>, repo: &RepoUrl, kind| client.protect_vault_tags(repo, kind);
        for (url, kind, expected) in [
            (
                "https://github.com/odoo/vault",
                ForgeKind::Github,
                "POST https://api.github.com/repos/odoo/vault/rulesets ",
            ),
            (
                "https://gitlab.com/odoo/vault",
                ForgeKind::Gitlab,
                "POST https://gitlab.com/api/v4/projects/odoo%2Fvault/protected_tags ",
            ),
            (
                "https://forge.corp/odoo/vault",
                ForgeKind::Gitea,
                "POST https://forge.corp/api/v1/repos/odoo/vault/tag_protections ",
            ),
        ] {
            let (result, urls) = run(url, kind, &[201], protect);
            assert_eq!(result, Ok(()));
            assert!(urls[0].starts_with(expected), "{urls:?}");
            assert!(urls[0].contains("odx/"), "{urls:?}");
        }
        let (result, _) = run(
            "https://forge.corp/odoo/vault",
            ForgeKind::Gitea,
            &[403],
            protect,
        );
        assert!(result.is_err());
        no_sleep(Duration::ZERO);
    }
}
