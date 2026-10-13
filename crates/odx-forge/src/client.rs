use std::time::Duration;

use odx_core::{Env, Invocation, ProcessRunner};
use serde_json::Value;

use crate::{ForgeError, Http, HttpResponse, RepoUrl};

/// A forge implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    /// GitHub or GitHub Enterprise Server.
    Github,
    /// GitLab.
    Gitlab,
    /// Gitea or Forgejo.
    Gitea,
}

impl ForgeKind {
    /// The forge's name in messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Github => "GitHub",
            Self::Gitlab => "GitLab",
            Self::Gitea => "Gitea/Forgejo",
        }
    }

    /// The forge a host most likely runs, by its name alone (no network):
    /// enough to pick its token variables, as `ODX_TOKEN_<HOST>` works for
    /// any.
    ///
    /// ```
    /// use odx_forge::ForgeKind;
    ///
    /// assert_eq!(ForgeKind::guess("github.com"), ForgeKind::Github);
    /// assert_eq!(ForgeKind::guess("gitlab.example.com"), ForgeKind::Gitlab);
    /// assert_eq!(ForgeKind::guess("codeberg.org"), ForgeKind::Gitea);
    /// ```
    pub fn guess(host: &str) -> Self {
        if host.contains("github") {
            Self::Github
        } else if host.contains("gitlab") {
            Self::Gitlab
        } else {
            Self::Gitea
        }
    }
}

/// Who can read a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Anyone.
    Public,
    /// Every signed-in user of the forge (GitLab, Gitea, GitHub Enterprise).
    Internal,
    /// Only members.
    Private,
}

/// How many times a rate-limited request is tried.
const ATTEMPTS: usize = 3;

/// The longest wait for a rate limit to reset.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// The token for `host`: `ODX_TOKEN_<HOST>` (upper case, other characters
/// as `_`), then the forge's conventional variables, then for GitHub
/// `gh auth token`. Empty values are ignored.
///
/// ```
/// use odx_core::fake::{MapEnv, ScriptedRunner};
/// use odx_forge::{ForgeKind, token};
///
/// let env = MapEnv::default().with("ODX_TOKEN_FORGE_EXAMPLE_COM", "t1").with("GITEA_TOKEN", "t2");
/// let runner = ScriptedRunner::default();
/// assert_eq!(token("forge.example.com", ForgeKind::Gitea, &env, &runner).as_deref(), Some("t1"));
/// assert_eq!(token("other.example.com", ForgeKind::Gitea, &env, &runner).as_deref(), Some("t2"));
/// ```
pub fn token(
    host: &str,
    kind: ForgeKind,
    env: &dyn Env,
    runner: &dyn ProcessRunner,
) -> Option<String> {
    token_source(host, kind, env, runner).map(|(token, _)| token)
}

/// The token for `host`, as [`token`] finds it, with where it came from:
/// an environment variable's name, `gh auth token`, or the store
/// `odx auth login` writes to.
pub fn token_source(
    host: &str,
    kind: ForgeKind,
    env: &dyn Env,
    runner: &dyn ProcessRunner,
) -> Option<(String, String)> {
    let own: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let conventional: &[&str] = match (kind, host) {
        (ForgeKind::Github, "github.com") => &["GITHUB_TOKEN", "GH_TOKEN"],
        (ForgeKind::Github, _) => &["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"],
        (ForgeKind::Gitlab, _) => &["GITLAB_TOKEN"],
        (ForgeKind::Gitea, _) => &["GITEA_TOKEN", "FORGEJO_TOKEN"],
    };
    std::iter::once(format!("ODX_TOKEN_{own}"))
        .chain(conventional.iter().map(|name| (*name).to_owned()))
        .find_map(|name| {
            env.var(&name)
                .filter(|value| !value.is_empty())
                .map(|value| (value, name))
        })
        .or_else(|| {
            (kind == ForgeKind::Github)
                .then(|| {
                    runner
                        .run(&Invocation::new("gh").args(["auth", "token", "--hostname", host]))
                        .ok()
                        .filter(odx_core::Output::success)
                        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                        .filter(|token| !token.is_empty())
                        .map(|token| (token, "gh auth token".to_owned()))
                })
                .flatten()
        })
        .or_else(|| {
            let store = crate::secrets::secrets(env);
            store
                .get(host)
                .ok()
                .flatten()
                .filter(|token| !token.is_empty())
                .map(|token| (token, store.describe()))
        })
}

/// Read-only access to forge APIs.
pub struct ForgeClient<'a> {
    http: &'a dyn Http,
    env: &'a dyn Env,
    runner: &'a dyn ProcessRunner,
    sleep: &'a dyn Fn(Duration),
}

impl std::fmt::Debug for ForgeClient<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForgeClient")
            .field("http", &self.http)
            .finish_non_exhaustive()
    }
}

impl<'a> ForgeClient<'a> {
    /// A client; `sleep` waits for rate limits to reset.
    pub fn new(
        http: &'a dyn Http,
        env: &'a dyn Env,
        runner: &'a dyn ProcessRunner,
        sleep: &'a dyn Fn(Duration),
    ) -> Self {
        Self {
            http,
            env,
            runner,
            sleep,
        }
    }

    /// GET with the forge's authentication, waiting out rate limits.
    /// The forge's authentication and API headers for `repo`.
    pub(crate) fn headers(&self, repo: &RepoUrl, kind: ForgeKind) -> Vec<(&'static str, String)> {
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some(token) = token(&repo.host, kind, self.env, self.runner) {
            headers.push((
                "authorization",
                match kind {
                    ForgeKind::Gitea => format!("token {token}"),
                    ForgeKind::Github | ForgeKind::Gitlab => format!("Bearer {token}"),
                },
            ));
        }
        if kind == ForgeKind::Github {
            headers.push(("accept", "application/vnd.github+json".to_owned()));
            headers.push(("x-github-api-version", "2022-11-28".to_owned()));
        }
        headers
    }

    /// POST `body` with the forge's authentication.
    pub(crate) fn post(
        &self,
        url: &str,
        repo: &RepoUrl,
        kind: ForgeKind,
        body: &Value,
    ) -> Result<HttpResponse, ForgeError> {
        self.http.post(url, &self.headers(repo, kind), body)
    }

    fn get(&self, url: &str, repo: &RepoUrl, kind: ForgeKind) -> Result<HttpResponse, ForgeError> {
        let headers = self.headers(repo, kind);
        for _ in 0..ATTEMPTS {
            let response = self.http.get(url, &headers)?;
            let retry_after = response.header("retry-after");
            let exhausted = response.header("x-ratelimit-remaining") == Some("0");
            let limited = response.status == 429
                || (response.status == 403 && (retry_after.is_some() || exhausted));
            if !limited {
                return Ok(response);
            }
            let wait = retry_after
                .and_then(|seconds| seconds.trim().parse::<u64>().ok())
                .map_or(MAX_WAIT, Duration::from_secs)
                .min(MAX_WAIT);
            (self.sleep)(wait);
        }
        Err(ForgeError::RateLimited(repo.host.clone()))
    }

    /// The JSON at `url`; `None` when the forge answers 404.
    pub(crate) fn json(
        &self,
        url: &str,
        repo: &RepoUrl,
        kind: ForgeKind,
    ) -> Result<Option<Value>, ForgeError> {
        let response = self.get(url, repo, kind)?;
        match response.status {
            200 => serde_json::from_slice(&response.body)
                .map(Some)
                .map_err(|e| ForgeError::Json(format!("{}: {e}", repo.host))),
            404 => Ok(None),
            status => Err(ForgeError::Status {
                host: repo.host.clone(),
                status,
            }),
        }
    }

    /// The API URL of `repo` itself, under which its pull requests,
    /// commits and settings live.
    pub(crate) fn repo_api(repo: &RepoUrl, kind: ForgeKind) -> String {
        let origin = repo.origin();
        match (kind, repo.host.as_str()) {
            (ForgeKind::Github, "github.com") => {
                format!("https://api.github.com/repos/{}", repo.path)
            }
            (ForgeKind::Github, _) => format!("{origin}/api/v3/repos/{}", repo.path),
            (ForgeKind::Gitlab, _) => {
                format!("{origin}/api/v4/projects/{}", repo.path.replace('/', "%2F"))
            }
            (ForgeKind::Gitea, _) => format!("{origin}/api/v1/repos/{}", repo.path),
        }
    }

    /// Which forge hosts `repo`: known hosts by name, others by probing
    /// their API (Gitea/Forgejo, then GitLab, then GitHub Enterprise).
    ///
    /// # Errors
    ///
    /// [`ForgeError::UnknownForge`] when no API answers like a forge, and
    /// network errors.
    pub fn kind(&self, repo: &RepoUrl) -> Result<ForgeKind, ForgeError> {
        match repo.host.as_str() {
            "github.com" => return Ok(ForgeKind::Github),
            "gitlab.com" => return Ok(ForgeKind::Gitlab),
            "codeberg.org" => return Ok(ForgeKind::Gitea),
            _ => {}
        }
        let origin = repo.origin();
        for (kind, path, statuses) in [
            (ForgeKind::Gitea, "/api/v1/version", &[200][..]),
            (ForgeKind::Gitlab, "/api/v4/version", &[200, 401][..]),
            (ForgeKind::Github, "/api/v3/meta", &[200][..]),
        ] {
            let response = self.http.get(&format!("{origin}{path}"), &[])?;
            if statuses.contains(&response.status) {
                return Ok(kind);
            }
        }
        Err(ForgeError::UnknownForge(repo.host.clone()))
    }

    /// Who can read `repo`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the
    /// repository (missing, or private and the token cannot read it),
    /// [`ForgeError::Json`] for an unexpected answer, and network, status
    /// and rate-limit errors.
    pub fn visibility(&self, repo: &RepoUrl, kind: ForgeKind) -> Result<Visibility, ForgeError> {
        let url = Self::repo_api(repo, kind);
        let value = self
            .json(&url, repo, kind)?
            .ok_or_else(|| ForgeError::NotVisible(format!("{}/{}", repo.host, repo.path)))?;
        let visibility = value.get("visibility").and_then(Value::as_str);
        let flag = |key: &str| value.get(key).and_then(Value::as_bool);
        match (visibility, flag("private"), flag("internal")) {
            (Some("public"), _, _) | (None, Some(false), Some(false) | None) => {
                Ok(Visibility::Public)
            }
            (Some("internal"), _, _) | (None, Some(false), Some(true)) => Ok(Visibility::Internal),
            (Some("private"), _, _) | (None, Some(true), _) => Ok(Visibility::Private),
            _ => Err(ForgeError::Json(format!(
                "{}: no visibility in the answer for {}",
                repo.host, repo.path
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};
    use odx_core::Output;
    use odx_core::fake::{MapEnv, ScriptedRunner};
    use std::sync::Mutex;

    fn limited(status: u16, headers: &[(&str, &str)]) -> HttpResponse {
        HttpResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            body: Vec::new(),
        }
    }

    fn repo(url: &str) -> RepoUrl {
        RepoUrl::parse(url).unwrap()
    }

    fn no_sleep(_: Duration) {}

    #[test]
    fn sleeping_is_injected() {
        no_sleep(Duration::ZERO);
    }

    #[test]
    fn tokens_from_env_and_gh() {
        let env = MapEnv::default()
            .with("GITHUB_TOKEN", "")
            .with("GH_TOKEN", "gh")
            .with("GITLAB_TOKEN", "gl")
            .with("FORGEJO_TOKEN", "fj")
            .with("GH_ENTERPRISE_TOKEN", "ghe");
        let none = ScriptedRunner::default();
        assert_eq!(
            token("github.com", ForgeKind::Github, &env, &none).as_deref(),
            Some("gh")
        );
        assert_eq!(
            token("ghe.corp", ForgeKind::Github, &env, &none).as_deref(),
            Some("ghe")
        );
        assert_eq!(
            token("gitlab.corp", ForgeKind::Gitlab, &env, &none).as_deref(),
            Some("gl")
        );
        assert_eq!(
            token("forge.corp", ForgeKind::Gitea, &env, &none).as_deref(),
            Some("fj")
        );

        let empty = MapEnv::default();
        let gh = ScriptedRunner::default();
        gh.push(Ok(Output {
            status: Some(0),
            stdout: b"from-gh\n".to_vec(),
            ..Output::default()
        }));
        assert_eq!(
            token("github.com", ForgeKind::Github, &empty, &gh).as_deref(),
            Some("from-gh")
        );
        assert_eq!(
            gh.invocations()[0].arguments(),
            ["auth", "token", "--hostname", "github.com"]
        );
        gh.push(Ok(Output {
            status: Some(0),
            stdout: b"\n".to_vec(),
            ..Output::default()
        }));
        assert_eq!(token("github.com", ForgeKind::Github, &empty, &gh), None);
        gh.push(Ok(Output {
            status: Some(1),
            ..Output::default()
        }));
        assert_eq!(token("github.com", ForgeKind::Github, &empty, &gh), None);
        assert_eq!(
            token("github.com", ForgeKind::Github, &empty, &gh),
            None,
            "gh missing"
        );
        assert_eq!(token("gitlab.com", ForgeKind::Gitlab, &empty, &gh), None);
        assert_eq!(gh.invocations().len(), 4, "gh is only asked for GitHub");
    }

    #[test]
    fn known_hosts_and_probes() {
        let http = Scripted::new(Vec::new());
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        for (url, kind) in [
            ("https://github.com/a/b", ForgeKind::Github),
            ("git@gitlab.com:a/b", ForgeKind::Gitlab),
            ("https://codeberg.org/a/b", ForgeKind::Gitea),
        ] {
            assert_eq!(client.kind(&repo(url)).unwrap(), kind);
        }
        assert_eq!(http.urls(), Vec::<String>::new());

        for (answers, kind) in [
            (vec![Ok(reply(200, "{}"))], ForgeKind::Gitea),
            (
                vec![Ok(reply(404, "")), Ok(reply(401, ""))],
                ForgeKind::Gitlab,
            ),
            (
                vec![Ok(reply(404, "")), Ok(reply(404, "")), Ok(reply(200, "{}"))],
                ForgeKind::Github,
            ),
        ] {
            let http = Scripted::new(answers);
            let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
            assert_eq!(
                client.kind(&repo("https://forge.corp:8443/a/b")).unwrap(),
                kind
            );
            assert!(http.urls()[0].starts_with("https://forge.corp:8443/api/v1/version"));
        }
        let http = Scripted::new(Vec::new());
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        assert_eq!(
            client.kind(&repo("https://forge.corp/a/b")),
            Err(ForgeError::UnknownForge("forge.corp".into()))
        );
        let http = Scripted::new(vec![Err(ForgeError::Http("down".into()))]);
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        assert_eq!(
            client.kind(&repo("https://forge.corp/a/b")),
            Err(ForgeError::Http("down".into()))
        );
        let names = [ForgeKind::Github, ForgeKind::Gitlab, ForgeKind::Gitea].map(ForgeKind::name);
        assert_eq!(names, ["GitHub", "GitLab", "Gitea/Forgejo"]);
    }

    #[test]
    fn visibility_per_forge() {
        let env = MapEnv::default()
            .with("GITHUB_TOKEN", "ght")
            .with("GH_ENTERPRISE_TOKEN", "ght")
            .with("GITLAB_TOKEN", "glt")
            .with("GITEA_TOKEN", "gtt");
        let runner = ScriptedRunner::default();
        for (url, kind, body, expected, api, auth) in [
            (
                "https://github.com/o/r",
                ForgeKind::Github,
                r#"{"private":true}"#,
                Visibility::Private,
                "https://api.github.com/repos/o/r",
                "Bearer ght",
            ),
            (
                "https://ghe.corp/o/r",
                ForgeKind::Github,
                r#"{"visibility":"internal"}"#,
                Visibility::Internal,
                "https://ghe.corp/api/v3/repos/o/r",
                "Bearer ght",
            ),
            (
                "git@gitlab.com:g/s/r.git",
                ForgeKind::Gitlab,
                r#"{"visibility":"public"}"#,
                Visibility::Public,
                "https://gitlab.com/api/v4/projects/g%2Fs%2Fr",
                "Bearer glt",
            ),
            (
                "https://forge.corp/o/r",
                ForgeKind::Gitea,
                r#"{"private":false,"internal":true}"#,
                Visibility::Internal,
                "https://forge.corp/api/v1/repos/o/r",
                "token gtt",
            ),
            (
                "https://forge.corp/o/r",
                ForgeKind::Gitea,
                r#"{"private":false,"internal":false}"#,
                Visibility::Public,
                "https://forge.corp/api/v1/repos/o/r",
                "token gtt",
            ),
            (
                "https://forge.corp/o/r",
                ForgeKind::Gitea,
                r#"{"private":true}"#,
                Visibility::Private,
                "https://forge.corp/api/v1/repos/o/r",
                "token gtt",
            ),
            (
                "https://github.com/o/r",
                ForgeKind::Github,
                r#"{"visibility":"private","private":true}"#,
                Visibility::Private,
                "https://api.github.com/repos/o/r",
                "Bearer ght",
            ),
        ] {
            let http = Scripted::new(vec![Ok(reply(200, body))]);
            let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
            assert_eq!(
                client.visibility(&repo(url), kind).unwrap(),
                expected,
                "{url} {body}"
            );
            let seen = http.seen.lock().unwrap();
            assert_eq!(seen[0].0, api);
            assert!(
                seen[0]
                    .1
                    .contains(&("authorization".to_owned(), auth.to_owned()))
            );
            let github = kind == ForgeKind::Github;
            assert_eq!(
                seen[0].1.iter().any(|(k, _)| k == "x-github-api-version"),
                github
            );
        }
    }

    #[test]
    fn visibility_failures() {
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let target = repo("https://forge.corp/o/r");
        let http = Scripted::new(vec![Err(ForgeError::Http("down".into()))]);
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        assert_eq!(
            client.visibility(&target, ForgeKind::Gitea),
            Err(ForgeError::Http("down".into()))
        );
        for (answer, expected) in [
            (
                reply(404, ""),
                ForgeError::NotVisible("forge.corp/o/r".into()),
            ),
            (
                reply(500, ""),
                ForgeError::Status {
                    host: "forge.corp".into(),
                    status: 500,
                },
            ),
            (
                reply(200, "not json"),
                ForgeError::Json("forge.corp: expected ident at line 1 column 2".into()),
            ),
            (
                reply(200, "{}"),
                ForgeError::Json("forge.corp: no visibility in the answer for o/r".into()),
            ),
        ] {
            let http = Scripted::new(vec![Ok(answer)]);
            let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
            assert_eq!(
                client.visibility(&target, ForgeKind::Gitea),
                Err(expected.clone())
            );
            assert!(
                http.seen.lock().unwrap()[0].1.is_empty(),
                "no token, no header"
            );
        }
    }

    #[test]
    fn rate_limits_are_waited_out() {
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let target = repo("https://github.com/o/r");
        let waits = Mutex::new(Vec::new());
        let sleep = |wait: Duration| waits.lock().unwrap().push(wait.as_secs());
        let http = Scripted::new(vec![
            Ok(limited(429, &[("retry-after", "2")])),
            Ok(limited(403, &[("x-ratelimit-remaining", "0")])),
            Ok(reply(200, r#"{"private":true}"#)),
        ]);
        let client = ForgeClient::new(&http, &env, &runner, &sleep);
        assert_eq!(
            client.visibility(&target, ForgeKind::Github),
            Ok(Visibility::Private)
        );
        assert_eq!(*waits.lock().unwrap(), [2, 60]);

        let http = Scripted::new(vec![
            Ok(limited(403, &[("retry-after", "600")])),
            Ok(limited(429, &[("retry-after", "x")])),
            Ok(limited(429, &[])),
        ]);
        let client = ForgeClient::new(&http, &env, &runner, &sleep);
        assert_eq!(
            client.visibility(&target, ForgeKind::Github),
            Err(ForgeError::RateLimited("github.com".into()))
        );
        // A plain 403 is an answer, not a rate limit.
        let http = Scripted::new(vec![Ok(reply(403, ""))]);
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        assert_eq!(
            client.visibility(&target, ForgeKind::Github),
            Err(ForgeError::Status {
                host: "github.com".into(),
                status: 403
            })
        );
        assert!(format!("{client:?}").starts_with("ForgeClient"));
    }
}
