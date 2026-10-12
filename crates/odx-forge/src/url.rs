/// A repository on a forge, from its clone URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoUrl {
    /// `https`, or `http` when the clone URL uses plain HTTP; SSH URLs use
    /// `https` for the API.
    pub scheme: &'static str,
    /// Host name, lowercase.
    pub host: String,
    /// The HTTP port, when the clone URL names one.
    pub port: Option<u16>,
    /// `owner/name` (GitLab: `group/subgroup/name`), without `.git`.
    pub path: String,
}

impl RepoUrl {
    /// Parse a clone URL: `https://host/owner/name(.git)`,
    /// `ssh://[user@]host[:port]/owner/name`, or scp-like
    /// `user@host:owner/name`. Local paths, `file://` and `git://` URLs
    /// are not forge repositories.
    ///
    /// ```
    /// use odx_forge::RepoUrl;
    ///
    /// let repo = RepoUrl::parse("git@github.com:OCA/pos.git").unwrap();
    /// assert_eq!((repo.host.as_str(), repo.path.as_str()), ("github.com", "OCA/pos"));
    /// assert_eq!(RepoUrl::parse("file:///srv/vault.git"), None);
    /// ```
    pub fn parse(url: &str) -> Option<Self> {
        let (scheme, rest, http) = if let Some(rest) = url.strip_prefix("https://") {
            ("https", rest, true)
        } else if let Some(rest) = url.strip_prefix("http://") {
            ("http", rest, true)
        } else if let Some(rest) = url.strip_prefix("ssh://") {
            ("https", rest, false)
        } else if !url.contains("://") && url.contains('@') {
            // scp-like: user@host:path
            let (authority, path) = url.split_once(':')?;
            return Self::build("https", authority, None, path);
        } else {
            return None;
        };
        let (authority, path) = rest.split_once('/')?;
        let (authority, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().ok()),
            None => (authority, None),
        };
        Self::build(scheme, authority, port.filter(|_| http), path)
    }

    fn build(scheme: &'static str, authority: &str, port: Option<u16>, path: &str) -> Option<Self> {
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        (!host.is_empty() && path.contains('/')).then(|| Self {
            scheme,
            host: host.to_ascii_lowercase(),
            port,
            path: path.to_owned(),
        })
    }

    /// `scheme://host[:port]`, where the forge's API lives.
    pub fn origin(&self) -> String {
        let port = self.port.map(|port| format!(":{port}")).unwrap_or_default();
        format!("{}://{}{port}", self.scheme, self.host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings() {
        for (url, origin, path) in [
            (
                "https://github.com/OCA/pos.git",
                "https://github.com",
                "OCA/pos",
            ),
            (
                "https://GitHub.com/OCA/pos/",
                "https://github.com",
                "OCA/pos",
            ),
            (
                "http://forge.local:3000/odoo/vault",
                "http://forge.local:3000",
                "odoo/vault",
            ),
            (
                "ssh://git@forge.local:2222/odoo/vault.git",
                "https://forge.local",
                "odoo/vault",
            ),
            (
                "ssh://forge.local/a/b/c.git",
                "https://forge.local",
                "a/b/c",
            ),
            (
                "git@gitlab.com:group/sub/name.git",
                "https://gitlab.com",
                "group/sub/name",
            ),
            (
                "https://token@forge.local:x/odoo/vault",
                "https://forge.local",
                "odoo/vault",
            ),
        ] {
            let repo = RepoUrl::parse(url).unwrap();
            assert_eq!(
                (repo.origin().as_str(), repo.path.as_str()),
                (origin, path),
                "{url}"
            );
        }
        for url in [
            "/srv/vault.git",
            "file:///srv/vault.git",
            "git://host/a/b",
            "https://github.com",
            "https://github.com/OCA",
            "git@host",
        ] {
            assert_eq!(RepoUrl::parse(url), None, "{url}");
        }
    }
}
