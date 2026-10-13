use serde_json::Value;

use crate::{ForgeClient, ForgeError, ForgeKind, RepoUrl};

/// The CI state of a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    /// Every check passed (neutral and skipped ones aside).
    Green,
    /// A check failed.
    Red,
    /// Checks are still running or waiting.
    Pending,
    /// The forge reports no checks.
    Unknown,
}

/// The CI state of a commit and the checks that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiReport {
    /// The overall state.
    pub state: CiState,
    /// Names of the failing checks, sorted.
    pub failing: Vec<String>,
}

/// One check, reduced to what decides the overall state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Passed,
    Failed,
    Running,
    Neutral,
}

/// The overall state of `checks`: red when any failed, else pending when
/// any runs, else green when any passed, else unknown.
fn report(checks: Vec<(String, Check)>) -> CiReport {
    let any = |wanted| checks.iter().any(|(_, check)| *check == wanted);
    let state = if any(Check::Failed) {
        CiState::Red
    } else if any(Check::Running) {
        CiState::Pending
    } else if any(Check::Passed) {
        CiState::Green
    } else {
        CiState::Unknown
    };
    let mut failing: Vec<String> = checks
        .into_iter()
        .filter(|(_, check)| *check == Check::Failed)
        .map(|(name, _)| name)
        .collect();
    failing.sort();
    failing.dedup();
    CiReport { state, failing }
}

fn text<'v>(value: &'v Value, key: &str) -> &'v str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn items<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// A GitHub check run.
fn github_run(run: &Value) -> (String, Check) {
    let check = match (text(run, "status"), text(run, "conclusion")) {
        ("completed", "success") => Check::Passed,
        ("completed", "neutral" | "skipped") => Check::Neutral,
        ("completed", _) => Check::Failed,
        _ => Check::Running,
    };
    (text(run, "name").to_owned(), check)
}

/// A commit status (GitHub `state`, Gitea `status`).
fn commit_status(name: &str, state: &str) -> (String, Check) {
    let check = match state {
        "success" => Check::Passed,
        "pending" => Check::Running,
        "warning" => Check::Neutral,
        _ => Check::Failed,
    };
    (name.to_owned(), check)
}

/// A GitLab commit status.
fn gitlab_status(status: &Value) -> (String, Check) {
    let allowed = status.get("allow_failure").and_then(Value::as_bool) == Some(true);
    let check = match text(status, "status") {
        "success" => Check::Passed,
        "skipped" | "manual" => Check::Neutral,
        "failed" if allowed => Check::Neutral,
        "failed" | "canceled" => Check::Failed,
        _ => Check::Running,
    };
    (text(status, "name").to_owned(), check)
}

impl ForgeClient<'_> {
    /// The CI state of commit `sha` in `repo`: GitHub check runs and
    /// commit statuses, GitLab commit statuses, Gitea/Forgejo commit
    /// statuses.
    ///
    /// # Errors
    ///
    /// [`ForgeError::NotVisible`] when the forge does not show the
    /// repository or commit, and network, status and rate-limit errors.
    pub fn ci(&self, repo: &RepoUrl, kind: ForgeKind, sha: &str) -> Result<CiReport, ForgeError> {
        let origin = repo.origin();
        let path = &repo.path;
        let not_visible = || ForgeError::NotVisible(format!("{}/{path}@{sha}", repo.host));
        let checks = match kind {
            ForgeKind::Github => {
                let api = if repo.host == "github.com" {
                    "https://api.github.com".to_owned()
                } else {
                    format!("{origin}/api/v3")
                };
                let runs = self
                    .json(
                        &format!("{api}/repos/{path}/commits/{sha}/check-runs?per_page=100"),
                        repo,
                        kind,
                    )?
                    .ok_or_else(not_visible)?;
                let status = self
                    .json(
                        &format!("{api}/repos/{path}/commits/{sha}/status"),
                        repo,
                        kind,
                    )?
                    .ok_or_else(not_visible)?;
                items(&runs, "check_runs")
                    .iter()
                    .map(github_run)
                    .chain(
                        items(&status, "statuses")
                            .iter()
                            .map(|s| commit_status(text(s, "context"), text(s, "state"))),
                    )
                    .collect()
            }
            ForgeKind::Gitlab => {
                let url = format!(
                    "{origin}/api/v4/projects/{}/repository/commits/{sha}/statuses",
                    path.replace('/', "%2F")
                );
                let statuses = self.json(&url, repo, kind)?.ok_or_else(not_visible)?;
                statuses
                    .as_array()
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .map(gitlab_status)
                    .collect()
            }
            ForgeKind::Gitea => {
                let url = format!("{origin}/api/v1/repos/{path}/commits/{sha}/status");
                let combined = self.json(&url, repo, kind)?.ok_or_else(not_visible)?;
                items(&combined, "statuses")
                    .iter()
                    .map(|s| commit_status(text(s, "context"), text(s, "status")))
                    .collect()
            }
        };
        Ok(report(checks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};
    use odx_core::fake::{MapEnv, ScriptedRunner};
    use std::time::Duration;

    fn no_sleep(_: Duration) {}

    fn ci(
        url: &str,
        kind: ForgeKind,
        answers: &[(u16, &str)],
    ) -> (Result<CiReport, ForgeError>, Vec<String>) {
        let http = Scripted::new(answers.iter().map(|(s, b)| Ok(reply(*s, b))).collect());
        let env = MapEnv::default();
        let runner = ScriptedRunner::default();
        let client = ForgeClient::new(&http, &env, &runner, &no_sleep);
        let result = client.ci(&RepoUrl::parse(url).unwrap(), kind, "abc");
        (result, http.urls())
    }

    fn state(report: Result<CiReport, ForgeError>) -> (CiState, Vec<String>) {
        let report = report.unwrap();
        (report.state, report.failing)
    }

    #[test]
    fn github_check_runs_and_statuses() {
        let runs = r#"{"check_runs":[
            {"name":"pre-commit","status":"completed","conclusion":"success"},
            {"name":"docs","status":"completed","conclusion":"skipped"},
            {"name":"neutral","status":"completed","conclusion":"neutral"}]}"#;
        let (report, urls) = ci(
            "https://github.com/OCA/pos",
            ForgeKind::Github,
            &[(200, runs), (200, r#"{"statuses":[]}"#)],
        );
        assert_eq!(state(report), (CiState::Green, vec![]));
        assert_eq!(
            urls,
            [
                "https://api.github.com/repos/OCA/pos/commits/abc/check-runs?per_page=100",
                "https://api.github.com/repos/OCA/pos/commits/abc/status"
            ]
        );
        let failing = r#"{"check_runs":[
            {"name":"test with Odoo","status":"completed","conclusion":"failure"},
            {"name":"test with OCB","status":"in_progress"}]}"#;
        let statuses = r#"{"statuses":[{"context":"codecov","state":"error"},{"context":"codecov","state":"error"}]}"#;
        let (report, urls) = ci(
            "https://ghe.corp/o/r",
            ForgeKind::Github,
            &[(200, failing), (200, statuses)],
        );
        assert_eq!(
            state(report),
            (
                CiState::Red,
                vec!["codecov".into(), "test with Odoo".into()]
            )
        );
        assert!(urls[0].starts_with("https://ghe.corp/api/v3/repos/o/r/"));
        let running = r#"{"check_runs":[{"name":"tests","status":"queued"}]}"#;
        let (report, _) = ci(
            "https://github.com/o/r",
            ForgeKind::Github,
            &[
                (200, running),
                (200, r#"{"statuses":[{"context":"ci","state":"success"}]}"#),
            ],
        );
        assert_eq!(state(report).0, CiState::Pending);
        let (report, _) = ci(
            "https://github.com/o/r",
            ForgeKind::Github,
            &[(200, "{}"), (200, "{}")],
        );
        assert_eq!(state(report).0, CiState::Unknown);
        let (report, _) = ci("https://github.com/o/r", ForgeKind::Github, &[(404, "")]);
        assert_eq!(
            report,
            Err(ForgeError::NotVisible("github.com/o/r@abc".into()))
        );
        let (report, _) = ci(
            "https://github.com/o/r",
            ForgeKind::Github,
            &[(200, "{}"), (404, "")],
        );
        assert!(report.is_err());
        for kind in [ForgeKind::Github, ForgeKind::Gitlab, ForgeKind::Gitea] {
            let (report, _) = ci("https://forge.corp/o/r", kind, &[(500, "")]);
            assert!(report.is_err(), "{kind:?}");
        }
        let (report, _) = ci(
            "https://github.com/o/r",
            ForgeKind::Github,
            &[(200, "{}"), (500, "")],
        );
        assert!(report.is_err());
    }

    #[test]
    fn gitlab_statuses() {
        let statuses = r#"[
            {"name":"lint","status":"success"},
            {"name":"flaky","status":"failed","allow_failure":true},
            {"name":"deploy","status":"manual"},
            {"name":"docs","status":"skipped"}]"#;
        let (report, urls) = ci(
            "git@gitlab.com:g/s/r.git",
            ForgeKind::Gitlab,
            &[(200, statuses)],
        );
        assert_eq!(state(report).0, CiState::Green);
        assert_eq!(
            urls,
            ["https://gitlab.com/api/v4/projects/g%2Fs%2Fr/repository/commits/abc/statuses"]
        );
        for (body, expected) in [
            (r#"[{"name":"t","status":"failed"}]"#, CiState::Red),
            (r#"[{"name":"t","status":"canceled"}]"#, CiState::Red),
            (r#"[{"name":"t","status":"running"}]"#, CiState::Pending),
            ("[]", CiState::Unknown),
            ("{}", CiState::Unknown),
        ] {
            let (report, _) = ci("https://gitlab.com/g/r", ForgeKind::Gitlab, &[(200, body)]);
            assert_eq!(state(report).0, expected, "{body}");
        }
        let (report, _) = ci("https://gitlab.com/g/r", ForgeKind::Gitlab, &[(404, "")]);
        assert!(report.is_err());
    }

    #[test]
    fn gitea_statuses() {
        for (body, expected) in [
            (
                r#"{"statuses":[{"context":"ci","status":"success"},{"context":"lint","status":"warning"}]}"#,
                CiState::Green,
            ),
            (
                r#"{"statuses":[{"context":"ci","status":"failure"}]}"#,
                CiState::Red,
            ),
            (
                r#"{"statuses":[{"context":"ci","status":"pending"}]}"#,
                CiState::Pending,
            ),
            (r#"{"statuses":[]}"#, CiState::Unknown),
        ] {
            let (report, urls) = ci("https://forge.corp/o/r", ForgeKind::Gitea, &[(200, body)]);
            assert_eq!(state(report).0, expected, "{body}");
            assert_eq!(
                urls,
                ["https://forge.corp/api/v1/repos/o/r/commits/abc/status"]
            );
        }
        let (report, _) = ci("https://forge.corp/o/r", ForgeKind::Gitea, &[(404, "")]);
        assert!(report.is_err());
        no_sleep(Duration::ZERO);
    }
}
