use serde::Deserialize;

use super::ordered::Ordered;
use crate::Problem;

/// A parsed git-aggregator `repos.yaml`, in file order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReposFile {
    /// One entry per target directory.
    pub repos: Vec<RepoSpec>,
}

/// One target directory of `repos.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    /// The target directory, as written (e.g. `./pos`).
    pub dir: String,
    /// Remote name → URL, in file order.
    pub remotes: Vec<(String, String)>,
    /// The first merge: the base branch.
    pub base: MergeSpec,
    /// The merges applied on top of the base, in order.
    pub merges: Vec<MergeSpec>,
    /// The branch the result is committed to, and its push remote.
    pub target: Option<TargetSpec>,
    /// Shell commands run after aggregation.
    pub shell_command_after: Vec<String>,
}

impl RepoSpec {
    /// The URL of remote `name`.
    pub fn remote_url(&self, name: &str) -> Option<&str> {
        self.remotes
            .iter()
            .find(|(remote, _)| remote == name)
            .map(|(_, url)| url.as_str())
    }
}

/// One merge: a ref (branch, tag, PR ref or SHA) on a remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeSpec {
    /// Remote name.
    pub remote: String,
    /// URL of the remote.
    pub url: String,
    /// Branch, tag, `refs/pull/N/head` or commit.
    pub reference: String,
}

/// `target: [<remote>] <branch>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    /// Push remote, when the result can be pushed.
    pub remote: Option<String>,
    /// Target branch.
    pub branch: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepo {
    #[serde(default)]
    remotes: Option<Ordered<String>>,
    merges: Vec<RawMerge>,
    target: Option<String>,
    shell_command_after: Option<OneOrMany>,
    // Accepted for compatibility; odx fetches into a shared object cache.
    #[expect(dead_code, reason = "accepted and ignored, as documented")]
    defaults: Option<RawDefaults>,
    #[expect(dead_code, reason = "accepted and ignored, as documented")]
    fetch_all: Option<FetchAll>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    #[expect(dead_code, reason = "accepted and ignored, as documented")]
    depth: Option<u64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FetchAll {
    Flag(#[expect(dead_code, reason = "accepted and ignored")] bool),
    Remotes(#[expect(dead_code, reason = "accepted and ignored")] Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawMerge {
    Text(String),
    Table {
        remote: String,
        #[serde(rename = "ref")]
        reference: String,
        #[serde(default)]
        #[expect(dead_code, reason = "accepted and ignored, as documented")]
        depth: Option<u64>,
    },
}

/// Parse `repos.yaml` text; problems are reported per repository key.
pub(crate) fn parse(text: &str) -> Result<(ReposFile, Vec<Problem>), String> {
    let raw: Ordered<RawRepo> = serde_saphyr::from_str(text).map_err(|e| e.to_string())?;
    let mut problems = Vec::new();
    let mut repos = Vec::new();
    for (dir, raw) in raw.0 {
        let mut problem = |message: String| {
            problems.push(Problem {
                key: dir.clone(),
                message,
            });
        };
        let remotes = raw.remotes.map(|r| r.0).unwrap_or_default();
        let url_of = |name: &str| {
            remotes
                .iter()
                .find(|(remote, _)| remote == name)
                .map(|(_, url)| url.clone())
        };
        if raw.merges.is_empty() {
            problem("needs at least one merge (the base branch)".to_owned());
        }
        let mut merges = Vec::new();
        for (i, merge) in raw.merges.into_iter().enumerate() {
            let parsed = match merge {
                RawMerge::Text(text) => split_pair(&text),
                RawMerge::Table {
                    remote, reference, ..
                } => Some((remote, reference)),
            };
            match parsed {
                None => problem(format!("merges[{i}]: expected \"<remote> <ref>\"")),
                Some((remote, reference)) => match url_of(&remote) {
                    None => problem(format!("merges[{i}]: unknown remote `{remote}`")),
                    Some(url) => merges.push(MergeSpec {
                        remote,
                        url,
                        reference,
                    }),
                },
            }
        }
        let target = match raw.target.as_deref().map(split_target) {
            None => None,
            Some(None) => {
                problem("target: expected \"[<remote>] <branch>\"".to_owned());
                None
            }
            Some(Some(target)) => {
                if let Some(remote) = &target.remote
                    && url_of(remote).is_none()
                {
                    problem(format!("target: unknown remote `{remote}`"));
                }
                Some(target)
            }
        };
        let shell_command_after = match raw.shell_command_after {
            None => Vec::new(),
            Some(OneOrMany::One(command)) => vec![command],
            Some(OneOrMany::Many(commands)) => commands,
        };
        let mut merges = merges.into_iter();
        if let Some(base) = merges.next() {
            repos.push(RepoSpec {
                dir,
                remotes,
                base,
                merges: merges.collect(),
                target,
                shell_command_after,
            });
        }
    }
    Ok((ReposFile { repos }, problems))
}

/// Split `"[<remote>] <branch>"`.
fn split_target(text: &str) -> Option<TargetSpec> {
    let mut words = text.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(branch), None, None) => Some(TargetSpec {
            remote: None,
            branch: branch.to_owned(),
        }),
        (Some(remote), Some(branch), None) => Some(TargetSpec {
            remote: Some(remote.to_owned()),
            branch: branch.to_owned(),
        }),
        _ => None,
    }
}

/// Split `"<a> <b>"` on whitespace into exactly two words.
fn split_pair(text: &str) -> Option<(String, String)> {
    let mut words = text.split_whitespace();
    match (words.next(), words.next(), words.next()) {
        (Some(a), Some(b), None) => Some((a.to_owned(), b.to_owned())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r"
./product_attribute:
  remotes:
    oca: https://github.com/OCA/product-attribute.git
    acsone: git+ssh://git@github.com/acsone/product-attribute.git
  merges:
    - oca 8.0
    - oca refs/pull/105/head
    - remote: acsone
      ref: 8.0-fix
      depth: 10
  target: acsone aggregated_branch_name
  shell_command_after: echo done
  defaults:
    depth: 20
  fetch_all: true
./pos:
  remotes:
    oca: https://github.com/OCA/pos.git
  merges: [oca 17.0]
  shell_command_after: [a, b]
  fetch_all: [oca]
";

    #[test]
    fn parses_both_merge_forms_in_order() {
        let (file, problems) = parse(SAMPLE).unwrap();
        assert_eq!(problems, []);
        let repo = &file.repos[0];
        assert_eq!(repo.dir, "./product_attribute");
        assert_eq!(
            repo.remote_url("oca"),
            Some("https://github.com/OCA/product-attribute.git")
        );
        assert_eq!(repo.remote_url("nope"), None);
        assert_eq!(repo.base.reference, "8.0");
        assert_eq!(
            repo.base.url,
            "https://github.com/OCA/product-attribute.git"
        );
        let merges: Vec<_> = repo
            .merges
            .iter()
            .map(|m| (m.remote.as_str(), m.reference.as_str()))
            .collect();
        assert_eq!(
            merges,
            [("oca", "refs/pull/105/head"), ("acsone", "8.0-fix")]
        );
        assert_eq!(
            repo.target,
            Some(TargetSpec {
                remote: Some("acsone".into()),
                branch: "aggregated_branch_name".into()
            })
        );
        assert_eq!(repo.shell_command_after, ["echo done"]);
        assert_eq!(file.repos[1].shell_command_after, ["a", "b"]);
        assert_eq!(file.repos[1].target, None);
    }

    #[test]
    fn target_without_remote() {
        let (file, problems) =
            parse("./x:\n  remotes:\n    oca: u\n  merges:\n    - oca 17.0\n  target: local\n")
                .unwrap();
        assert_eq!(problems, []);
        assert_eq!(
            file.repos[0].target,
            Some(TargetSpec {
                remote: None,
                branch: "local".into()
            })
        );
    }

    #[test]
    fn reports_malformed_entries() {
        let text = r"
./x:
  remotes:
    oca: u
  merges:
    - oca
    - ghost 17.0
  target: oca too many
./y:
  merges: []
  target: ghost branch
";
        let (_, problems) = parse(text).unwrap();
        let found: Vec<_> = problems.iter().map(ToString::to_string).collect();
        assert_eq!(
            found,
            [
                "./x: merges[0]: expected \"<remote> <ref>\"",
                "./x: merges[1]: unknown remote `ghost`",
                "./x: target: expected \"[<remote>] <branch>\"",
                "./y: needs at least one merge (the base branch)",
                "./y: target: unknown remote `ghost`",
            ]
        );
    }

    #[test]
    fn rejects_unknown_keys_and_bad_yaml() {
        assert!(
            parse("./x:\n  merges: []\n  bogus: 1\n")
                .unwrap_err()
                .contains("bogus")
        );
        assert_ne!(parse("./x: [\n").unwrap_err(), "");
    }
}
