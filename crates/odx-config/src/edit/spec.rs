use std::fmt;

/// What a merge or package points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A pull or merge request.
    Pr(u32),
    /// A commit.
    Sha(String),
    /// A branch.
    Branch(String),
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pr(pr) => write!(f, "#{pr}"),
            Self::Sha(sha) | Self::Branch(sha) => write!(f, "@{sha}"),
        }
    }
}

/// A parsed `odx add` / `odx drop` argument: a repository and a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Repository URL.
    pub url: String,
    /// The pull request, commit or branch.
    pub target: Target,
}

/// Parse a change specification.
///
/// Accepted forms:
///
/// | Form | Meaning |
/// | --- | --- |
/// | `OCA/pos#123` | pull request 123 of `github.com/OCA/pos` |
/// | `OCA/pos@a1b2c3d` | commit (7 to 40 hex characters) |
/// | `OCA/pos@17.0-fix` | branch |
/// | `https://github.com/OCA/pos/pull/123` | pull request URL |
/// | `https://gitlab.com/g/p/-/merge_requests/7` | merge request URL |
/// | `<git url>#123`, `<git url>@<ref>` | the same on any URL |
///
/// ```
/// use odx_config::edit::{parse_spec, Target};
///
/// let spec = parse_spec("OCA/pos#123").unwrap();
/// assert_eq!(spec.url, "https://github.com/OCA/pos.git");
/// assert_eq!(spec.target, Target::Pr(123));
/// ```
///
/// # Errors
///
/// A message describing the accepted forms when `text` matches none.
pub fn parse_spec(text: &str) -> Result<Spec, String> {
    let text = text.trim();
    let invalid = || {
        format!(
            "cannot parse {text:?}: expected owner/repo#PR, owner/repo@REF, a pull request URL, or URL#PR / URL@REF"
        )
    };
    if let Some(spec) = parse_pr_url(text) {
        return Ok(spec);
    }
    let (repo, target) = if let Some((repo, pr)) = text.rsplit_once('#') {
        (repo, Target::Pr(pr.parse().map_err(|_| invalid())?))
    } else if let Some((repo, reference)) = split_ref(text) {
        let target = if is_commit(reference) {
            Target::Sha(reference.to_owned())
        } else {
            Target::Branch(reference.to_owned())
        };
        (repo, target)
    } else {
        return Err(invalid());
    };
    if repo.is_empty() {
        return Err(invalid());
    }
    let url = if repo.contains("://") || repo.contains('@') {
        repo.to_owned()
    } else {
        let mut parts = repo.split('/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
                format!(
                    "https://github.com/{owner}/{}.git",
                    name.trim_end_matches(".git")
                )
            }
            _ => return Err(invalid()),
        }
    };
    Ok(Spec { url, target })
}

/// Split `<repo>@<ref>`, ignoring an `@` in the URL's user part.
fn split_ref(text: &str) -> Option<(&str, &str)> {
    let path_start = text.find("://").map_or(0, |i| i + 3);
    let authority_end = text
        .split_at(path_start)
        .1
        .find('/')
        .map_or(text.len(), |i| path_start + i);
    let at = authority_end + text.split_at(authority_end).1.rfind('@')?;
    let (repo, reference) = text.split_at(at);
    let reference = reference.strip_prefix('@').unwrap_or(reference);
    (!reference.is_empty()).then_some((repo, reference))
}

fn parse_pr_url(text: &str) -> Option<Spec> {
    let rest = text.strip_prefix("https://")?;
    let (repo, number) = rest
        .split_once("/pull/")
        .or_else(|| rest.split_once("/-/merge_requests/"))?;
    let digits = number.split(['/', '#', '?']).next().unwrap_or_default();
    let number = digits.parse().ok()?;
    Some(Spec {
        url: format!("https://{}.git", repo.trim_end_matches(".git")),
        target: Target::Pr(number),
    })
}

/// Whether `reference` looks like an abbreviated or full commit id.
pub(crate) fn is_commit(reference: &str) -> bool {
    (7..=40).contains(&reference.len()) && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A comparison key for repository URLs: host and path, lowercased,
/// without scheme, user, `.git` or trailing slashes.
///
/// ```
/// use odx_config::edit::repo_key;
///
/// assert_eq!(repo_key("https://github.com/OCA/pos.git"), "github.com/oca/pos");
/// assert_eq!(repo_key("git@github.com:OCA/pos"), "github.com/oca/pos");
/// assert_eq!(repo_key("ssh://git@github.com/OCA/pos/"), "github.com/oca/pos");
/// ```
pub fn repo_key(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let without_user = without_scheme
        .split_once('@')
        .filter(|(user, _)| !user.contains('/'))
        .map_or(without_scheme, |(_, rest)| rest);
    without_user
        .replacen(':', "/", 1)
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> (String, Target) {
        let spec = parse_spec(text).unwrap();
        (spec.url, spec.target)
    }

    #[test]
    fn shorthand_forms() {
        let url = "https://github.com/OCA/pos.git".to_owned();
        assert_eq!(spec("OCA/pos#123"), (url.clone(), Target::Pr(123)));
        assert_eq!(spec("OCA/pos.git#1"), (url.clone(), Target::Pr(1)));
        assert_eq!(
            spec("OCA/pos@a1b2c3d"),
            (url.clone(), Target::Sha("a1b2c3d".into()))
        );
        assert_eq!(
            spec("OCA/pos@17.0-fix"),
            (url, Target::Branch("17.0-fix".into()))
        );
    }

    #[test]
    fn url_forms() {
        assert_eq!(
            spec("https://github.com/OCA/pos/pull/123/files"),
            ("https://github.com/OCA/pos.git".into(), Target::Pr(123))
        );
        assert_eq!(
            spec("https://gitlab.com/g/p/-/merge_requests/7"),
            ("https://gitlab.com/g/p.git".into(), Target::Pr(7))
        );
        assert_eq!(
            spec("git@github.com:acme/pos.git@17.0-x"),
            (
                "git@github.com:acme/pos.git".into(),
                Target::Branch("17.0-x".into())
            )
        );
        assert_eq!(
            spec("ssh://git@forge.example/odoo/pos.git#5"),
            ("ssh://git@forge.example/odoo/pos.git".into(), Target::Pr(5))
        );
        assert_eq!(
            spec("https://forge.example/odoo/pos.git@refs/heads/x"),
            (
                "https://forge.example/odoo/pos.git".into(),
                Target::Branch("refs/heads/x".into())
            )
        );
    }

    #[test]
    fn invalid_forms() {
        for text in [
            "OCA/pos",
            "pos#1",
            "OCA/pos#x",
            "#1",
            "a/b/c#1",
            "/pos#1",
            "OCA/#1",
            "OCA/pos@",
            "https://github.com/OCA/pos/pull/x",
        ] {
            assert!(
                parse_spec(text).unwrap_err().starts_with("cannot parse"),
                "{text}"
            );
        }
    }

    #[test]
    fn display_and_keys() {
        assert_eq!(Target::Pr(3).to_string(), "#3");
        assert_eq!(Target::Sha("abc1234".into()).to_string(), "@abc1234");
        assert_eq!(repo_key("HTTPS://GitHub.com/OCA/pos"), "github.com/oca/pos");
    }
}
