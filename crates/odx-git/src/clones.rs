use std::fs;
use std::path::{Path, PathBuf};

use odx_core::repo_key;

/// A local clone the cache can borrow objects from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalClone {
    /// The clone's git directory (`<clone>/.git`, or the clone itself when bare).
    pub git_dir: PathBuf,
    /// The clone's remote whose URL matches.
    pub remote: String,
    /// That remote's URL.
    pub url: String,
    /// Also fetch base branches into the clone's remote-tracking refs.
    pub update: bool,
}

/// The remote names and URLs in a git config file.
///
/// ```
/// use odx_git::remote_urls;
///
/// let config = "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:OCA/pos.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[remote \"me\"]\n\turl = https://github.com/me/pos.git\n";
/// assert_eq!(
///     remote_urls(config),
///     [("origin".to_owned(), "git@github.com:OCA/pos.git".to_owned()), ("me".to_owned(), "https://github.com/me/pos.git".to_owned())]
/// );
/// ```
pub fn remote_urls(config: &str) -> Vec<(String, String)> {
    let mut remote: Option<String> = None;
    let mut urls = Vec::new();
    for line in config.lines().map(str::trim) {
        if let Some(section) = line.strip_prefix('[') {
            remote = section
                .strip_prefix("remote \"")
                .and_then(|rest| rest.split_once('"'))
                .map(|(name, _)| name.to_owned());
        } else if let (Some(name), Some((key, value))) = (&remote, line.split_once('='))
            && key.trim() == "url"
        {
            urls.push((name.clone(), value.trim().to_owned()));
        }
    }
    urls
}

/// The git directory of a clone at `dir`, if `dir` is one.
fn git_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = dir.join(".git");
    if dot_git.join("config").is_file() {
        Some(dot_git)
    } else if dir.join("config").is_file() && dir.join("objects").is_dir() {
        Some(dir.to_path_buf())
    } else {
        None
    }
}

/// Local clones indexed by the comparison key of their remote URLs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Clones {
    entries: Vec<(String, LocalClone)>,
}

impl Clones {
    /// Index every direct subdirectory of `dirs` that is a git repository.
    /// Unreadable directories and non-repositories are skipped.
    pub fn scan(dirs: &[PathBuf], update: bool) -> Self {
        let mut entries = Vec::new();
        for dir in dirs {
            let Ok(children) = fs::read_dir(dir) else {
                continue;
            };
            let mut children: Vec<PathBuf> = children.flatten().map(|e| e.path()).collect();
            children.sort();
            for git_dir in children.iter().filter_map(|child| git_dir(child)) {
                let config = fs::read_to_string(git_dir.join("config")).unwrap_or_default();
                for (remote, url) in remote_urls(&config) {
                    entries.push((
                        repo_key(&url),
                        LocalClone {
                            git_dir: git_dir.clone(),
                            remote,
                            url,
                            update,
                        },
                    ));
                }
            }
        }
        Self { entries }
    }

    /// The clone of `url`, matched across URL spellings; the first one found
    /// wins.
    pub fn find(&self, url: &str) -> Option<&LocalClone> {
        let key = repo_key(url);
        self.entries
            .iter()
            .find(|(known, _)| *known == key)
            .map(|(_, clone)| clone)
    }

    /// Every indexed clone remote, in scan order.
    pub fn iter(&self) -> impl Iterator<Item = &LocalClone> {
        self.entries.iter().map(|(_, clone)| clone)
    }

    /// How many remotes are indexed.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is indexed.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parsing_edge_cases() {
        let config = "url = outside\n[remote \"a\"]\n  url=https://x/a.git\n  pushurl = https://x/p.git\n[branch \"main\"]\n  url = ignored\n[remote broken]\n  url = also-ignored\n";
        assert_eq!(
            remote_urls(config),
            [("a".to_owned(), "https://x/a.git".to_owned())]
        );
    }

    #[test]
    fn scans_working_clones_and_bare_repositories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("OCA");
        let pos = root.join("pos").join(".git");
        fs::create_dir_all(&pos).unwrap();
        fs::write(
            pos.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/OCA/pos.git\n",
        )
        .unwrap();
        let bare = root.join("web.git");
        fs::create_dir_all(bare.join("objects")).unwrap();
        fs::write(
            bare.join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:OCA/web\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("README"), "not a repo").unwrap();

        let clones = Clones::scan(&[root, tmp.path().join("missing")], true);
        assert_eq!(clones.len(), 2);
        assert!(!clones.is_empty());
        let found = clones.find("git@github.com:oca/pos").unwrap();
        assert_eq!(
            (found.git_dir.as_path(), found.remote.as_str(), found.update),
            (pos.as_path(), "origin", true)
        );
        assert_eq!(
            clones
                .find("https://github.com/OCA/web.git")
                .unwrap()
                .git_dir,
            bare
        );
        assert_eq!(clones.find("https://github.com/OCA/crm.git"), None);
        assert!(Clones::default().is_empty());
    }
}
