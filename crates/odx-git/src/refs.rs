use std::fmt;

use crate::cache::url_hash;

/// How a forge exposes pull requests as git refs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Forge {
    /// GitHub: `refs/pull/<n>/head`.
    Github,
    /// GitLab: `refs/merge-requests/<n>/head`.
    Gitlab,
    /// Gitea and Forgejo: `refs/pull/<n>/head`.
    Gitea,
}

impl Forge {
    /// Guess the forge from a repository URL; anything that is not GitHub
    /// or GitLab is treated as Gitea/Forgejo.
    ///
    /// ```
    /// use odx_git::Forge;
    ///
    /// assert_eq!(Forge::detect("https://github.com/OCA/pos.git"), Forge::Github);
    /// assert_eq!(Forge::detect("git@gitlab.example.com:odoo/web.git"), Forge::Gitlab);
    /// assert_eq!(Forge::detect("https://forge.example/odoo/vault.git"), Forge::Gitea);
    /// ```
    pub fn detect(url: &str) -> Self {
        let url = url.to_ascii_lowercase();
        if url.contains("github") {
            Self::Github
        } else if url.contains("gitlab") {
            Self::Gitlab
        } else {
            Self::Gitea
        }
    }

    /// The forge's name in local ref names.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
        }
    }
}

/// Something to fetch from a remote.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// A branch.
    Branch(String),
    /// A pull or merge request.
    Pr(u32),
    /// A commit, abbreviated or full.
    Commit(String),
    /// A ref exactly as written in git-aggregator's `repos.yaml`: a branch
    /// or tag name, or a full ref such as `refs/pull/12/head`; the remote
    /// resolves short names.
    Ref(String),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Branch(branch) => write!(f, "branch {branch}"),
            Self::Pr(pr) => write!(f, "#{pr}"),
            Self::Commit(sha) => write!(f, "commit {sha}"),
            Self::Ref(reference) => write!(f, "{reference}"),
        }
    }
}

impl Source {
    /// Whether this is a full commit id (40 or 64 hex characters).
    pub fn is_full_commit(&self) -> bool {
        matches!(self, Self::Commit(sha) if matches!(sha.len(), 40 | 64))
    }

    /// Whether this is an abbreviated commit id, which git cannot fetch
    /// directly: the remote's branches are fetched and the commit is looked
    /// up among them.
    fn is_abbreviated(&self) -> bool {
        matches!(self, Self::Commit(_)) && !self.is_full_commit()
    }

    /// The ref (or object id) to ask the remote for.
    ///
    /// ```
    /// use odx_git::{Forge, Source};
    ///
    /// assert_eq!(Source::Pr(7).remote_ref(Forge::Gitlab), "refs/merge-requests/7/head");
    /// assert_eq!(Source::Branch("17.0".into()).remote_ref(Forge::Github), "refs/heads/17.0");
    /// ```
    pub fn remote_ref(&self, forge: Forge) -> String {
        match (self, forge) {
            (Self::Branch(branch), _) => format!("refs/heads/{branch}"),
            (Self::Pr(pr), Forge::Gitlab) => format!("refs/merge-requests/{pr}/head"),
            (Self::Pr(pr), Forge::Github | Forge::Gitea) => format!("refs/pull/{pr}/head"),
            (Self::Commit(_), _) if self.is_abbreviated() => "refs/heads/*".to_owned(),
            (Self::Commit(sha), _) => sha.clone(),
            (Self::Ref(reference), _) => reference.clone(),
        }
    }

    /// The ref in the cache repository that keeps the fetched commit.
    ///
    /// Branches from the repository's own URL live under `refs/odx/heads/`,
    /// branches from other remotes under `refs/odx/remotes/<hash>/`, pull
    /// requests under `refs/odx/<forge>/pull/<n>/head` and commits under
    /// `refs/odx/commits/<sha>`. For an abbreviated commit the remote's
    /// branches are kept under `refs/odx/branches/<hash>/` and the commit
    /// itself is the revision to resolve.
    ///
    /// ```
    /// use odx_git::{Forge, Source};
    ///
    /// assert_eq!(Source::Pr(7).local_ref(Forge::Github, None), "refs/odx/github/pull/7/head");
    /// assert_eq!(Source::Branch("17.0".into()).local_ref(Forge::Github, None), "refs/odx/heads/17.0");
    /// ```
    pub fn local_ref(&self, forge: Forge, remote: Option<&str>) -> String {
        match (self, remote) {
            (Self::Branch(branch), None) => format!("refs/odx/heads/{branch}"),
            (Self::Branch(branch), Some(url)) => {
                format!("refs/odx/remotes/{}/{branch}", url_hash(url))
            }
            (Self::Pr(pr), _) => format!("refs/odx/{}/pull/{pr}/head", forge.name()),
            (Self::Commit(sha), _) if self.is_abbreviated() => sha.clone(),
            (Self::Commit(sha), _) => format!("refs/odx/commits/{sha}"),
            (Self::Ref(reference), remote) => format!(
                "refs/odx/named/{}/{}",
                remote.map_or_else(|| "origin".to_owned(), url_hash),
                reference.strip_prefix("refs/").unwrap_or(reference)
            ),
        }
    }

    /// The fetch refspec: forced for moving refs, plain for full commits.
    pub fn refspec(&self, forge: Forge, remote: Option<&str>) -> String {
        if self.is_abbreviated() {
            let owner = remote.map_or_else(|| "origin".to_owned(), url_hash);
            return format!("+refs/heads/*:refs/odx/branches/{owner}/*");
        }
        let plus = if matches!(self, Self::Commit(_)) {
            ""
        } else {
            "+"
        };
        format!(
            "{plus}{}:{}",
            self.remote_ref(forge),
            self.local_ref(forge, remote)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refspecs() {
        assert_eq!(
            Source::Pr(3).refspec(Forge::Gitea, None),
            "+refs/pull/3/head:refs/odx/gitea/pull/3/head"
        );
        assert_eq!(
            Source::Commit("a".repeat(40)).refspec(Forge::Github, None),
            format!("{0}:refs/odx/commits/{0}", "a".repeat(40))
        );
        let other = Source::Branch("fix".into()).local_ref(Forge::Github, Some("https://x/y.git"));
        assert!(other.starts_with("refs/odx/remotes/") && other.ends_with("/fix"));
    }

    #[test]
    fn abbreviated_commits_fetch_branches() {
        let short = Source::Commit("abc1234".into());
        assert_eq!(short.remote_ref(Forge::Github), "refs/heads/*");
        assert_eq!(short.local_ref(Forge::Github, None), "abc1234");
        assert_eq!(
            short.refspec(Forge::Github, None),
            "+refs/heads/*:refs/odx/branches/origin/*"
        );
        let other = short.refspec(Forge::Github, Some("https://x/y.git"));
        assert!(other.starts_with("+refs/heads/*:refs/odx/branches/") && !other.contains("origin"));
        let full = Source::Commit("a".repeat(40));
        assert_eq!(full.remote_ref(Forge::Github), "a".repeat(40));
    }

    #[test]
    fn literal_refs() {
        let short = Source::Ref("17.0".into());
        assert_eq!(
            short.refspec(Forge::Github, None),
            "+17.0:refs/odx/named/origin/17.0"
        );
        let full = Source::Ref("refs/pull/12/head".into());
        let other = full.local_ref(Forge::Github, Some("https://x/y.git"));
        assert!(other.starts_with("refs/odx/named/") && other.ends_with("/pull/12/head"));
        assert_eq!(full.to_string(), "refs/pull/12/head");
    }

    #[test]
    fn display_and_names() {
        assert_eq!(Source::Branch("b".into()).to_string(), "branch b");
        assert_eq!(Source::Pr(1).to_string(), "#1");
        assert_eq!(Source::Commit("c".into()).to_string(), "commit c");
        assert_eq!(Forge::Gitlab.name(), "gitlab");
    }
}
