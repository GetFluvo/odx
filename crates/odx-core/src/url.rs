/// A comparison key for repository URLs: host and path, lowercased,
/// without scheme, user, `.git` or trailing slashes.
///
/// ```
/// use odx_core::repo_key;
///
/// assert_eq!(repo_key("https://github.com/OCA/pos.git"), "github.com/oca/pos");
/// assert_eq!(repo_key("git@github.com:OCA/pos"), "github.com/oca/pos");
/// assert_eq!(repo_key("ssh://git@github.com/OCA/pos/"), "github.com/oca/pos");
/// assert_eq!(repo_key("HTTPS://GitHub.com/OCA/pos"), "github.com/oca/pos");
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
