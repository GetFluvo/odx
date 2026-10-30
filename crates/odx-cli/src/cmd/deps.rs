//! Whether the dependencies of an addon are available on a target series,
//! for `odx migrate plan` and `odx migrate addon`: part of Odoo, already
//! there or migrated earlier in the plan, released on the package index,
//! or still waiting on an open migration pull request upstream.

use std::collections::BTreeMap;

use odx_core::system::OsRunner;
use odx_forge::{ForgeClient, ForgeKind, Http, PullRequest, RepoUrl, ReqwestHttp};
use odx_index::sources::package_index;
use schemars::JsonSchema;
use serde::Serialize;

use crate::Context;

/// A dependency that is not available on the target series yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct Waiting {
    /// The dependency.
    pub(crate) addon: String,
    /// The repository it lives in on the source series, when known.
    pub(crate) repository: Option<String>,
    /// Its open migration pull request, when there is one.
    pub(crate) pull_request: Option<Migration>,
}

/// An open migration pull request of a dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(crate) struct Migration {
    /// `owner/repo` of its repository.
    pub(crate) repository: String,
    /// Where the repository is served: `scheme://host[:port]`.
    pub(crate) origin: String,
    /// Its number.
    pub(crate) number: u64,
    /// Its page.
    pub(crate) url: String,
    /// The ref a pip requirement points at for it.
    pub(crate) head: String,
}

impl Waiting {
    /// How it reads in a plan: `waits on x: open migration pull request
    /// OCA/y#12` or `waits on x: not on 19.0, and no open migration pull
    /// request found`.
    pub(crate) fn describe(&self, to: &str) -> String {
        self.pull_request.as_ref().map_or_else(
            || {
                format!(
                    "waits on {}: not on {to}, and no open migration pull request found",
                    self.addon
                )
            },
            |pr| {
                format!(
                    "waits on {}: open migration pull request {}#{}",
                    self.addon, pr.repository, pr.number
                )
            },
        )
    }
}

/// The Odoo addons of `series`, or of the newest series odx knows when it
/// has no list for it yet.
pub(crate) fn core(series: &str) -> Vec<&'static str> {
    odx_analyze::core::core_addons(series)
        .or_else(|| odx_analyze::core::core_addons("19.0"))
        .unwrap_or_default()
}

/// The package index's JSON API: `ODX_PYPI_URL`, else PyPI.
pub(crate) fn pypi(ctx: &Context<'_>) -> String {
    ctx.env
        .var("ODX_PYPI_URL")
        .as_deref()
        .unwrap_or("https://pypi.org/pypi")
        .to_owned()
}

/// A pull request's page and the ref of its head, per forge.
pub(crate) fn refs(kind: ForgeKind, base: &str, number: u64) -> (String, String) {
    match kind {
        ForgeKind::Gitlab => (
            format!("{base}/-/merge_requests/{number}"),
            format!("refs/merge-requests/{number}/head"),
        ),
        ForgeKind::Github => (
            format!("{base}/pull/{number}"),
            format!("refs/pull/{number}/head"),
        ),
        ForgeKind::Gitea => (
            format!("{base}/pulls/{number}"),
            format!("refs/pull/{number}/head"),
        ),
    }
}

/// Whether `title` names `addon` as a word: `pos_x` in `[17.0][FIX] pos_x:
/// ...`, but not `pos`.
pub(crate) fn names(title: &str, addon: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    !addon.is_empty()
        && title.match_indices(addon).any(|(at, _)| {
            let before = title.get(..at).and_then(|text| text.chars().next_back());
            let after = title
                .get(at + addon.len()..)
                .and_then(|text| text.chars().next());
            !before.is_some_and(word) && !after.is_some_and(word)
        })
}

/// Whether `pr` migrates `addon`: its title says `[MIG]`, or `[ADD]` for
/// an addon brought to the series anew (in any case), and it is about
/// `addon`. The `mod:<addon>` labels the OCA bot puts on a
/// pull request say which addons it touches, more reliably than a title
/// typed by hand; when it touches several, the title says which one it
/// migrates. Without such labels, the title must name `addon`.
pub(crate) fn migrates(pr: &PullRequest, addon: &str) -> bool {
    let title = pr.title.to_ascii_lowercase();
    if !title.contains("[mig]") && !title.contains("[add]") {
        return false;
    }
    let addon = addon.to_ascii_lowercase();
    let touched: Vec<String> = pr
        .labels
        .iter()
        .filter_map(|label| label.strip_prefix("mod:"))
        .map(str::to_ascii_lowercase)
        .collect();
    if touched.is_empty() {
        return names(&title, &addon);
    }
    touched.contains(&addon)
        && (names(&title, &addon) || !touched.iter().any(|other| names(&title, other)))
}

/// Looks dependencies up on the package index and the forges, remembering
/// what it asked.
pub(crate) struct Lookup<'a> {
    ctx: &'a Context<'a>,
    http: Option<ReqwestHttp>,
    pypi: String,
    /// Open pull requests per repository URL and target.
    open: BTreeMap<String, Vec<PullRequest>>,
}

impl<'a> Lookup<'a> {
    /// A lookup; `offline` asks nothing.
    pub(crate) fn new(ctx: &'a Context<'a>, offline: bool) -> Self {
        let pypi = pypi(ctx);
        Self {
            ctx,
            http: ReqwestHttp::new().ok().filter(|_| !offline),
            pypi,
            open: BTreeMap::new(),
        }
    }

    /// Whether it asks the package index and the forges at all.
    pub(crate) const fn online(&self) -> bool {
        self.http.is_some()
    }

    /// Which of `depends` are not available on `to`; `available` says what
    /// the caller knows to be there (the target branch, the plan). Nothing
    /// is reported offline, where odx cannot tell.
    pub(crate) fn waiting(
        &mut self,
        depends: &[String],
        from: &str,
        to: &str,
        available: &dyn Fn(&str) -> bool,
    ) -> Vec<Waiting> {
        let Some(http) = self.http.as_ref() else {
            return Vec::new();
        };
        let http = http as &dyn Http;
        let core = core(to);
        let mut waiting = Vec::new();
        for addon in depends {
            if core.contains(&addon.as_str())
                || available(addon)
                || matches!(package_index(http, &self.pypi, addon, to), Ok(Some(_)))
            {
                continue;
            }
            let repository = package_index(http, &self.pypi, addon, from).ok().flatten();
            let pull_request = repository
                .as_deref()
                .and_then(|url| migration(self.ctx, http, &mut self.open, url, addon, to));
            waiting.push(Waiting {
                addon: addon.clone(),
                repository,
                pull_request,
            });
        }
        waiting
    }
}

/// The open `[MIG]` pull request for `addon` on `to` in the repository at
/// `url`, the repository's open pull requests remembered in `open`.
fn migration(
    ctx: &Context<'_>,
    http: &dyn Http,
    open: &mut BTreeMap<String, Vec<PullRequest>>,
    url: &str,
    addon: &str,
    to: &str,
) -> Option<Migration> {
    let repo = RepoUrl::parse(url)?;
    let sleep = std::thread::sleep;
    let client = ForgeClient::new(http, ctx.env, &OsRunner, &sleep);
    let kind = client.kind(&repo).ok()?;
    let prs = open.entry(format!("{url} {to}")).or_insert_with(|| {
        client
            .open_pull_requests(&repo, kind, to)
            .unwrap_or_default()
    });
    let number = &prs.iter().find(|pr| migrates(pr, addon))?.number;
    let (page, head) = refs(kind, &format!("{}/{}", repo.origin(), repo.path), *number);
    Some(Migration {
        origin: repo.origin(),
        repository: repo.path,
        number: *number,
        url: page,
        head,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(title: &str, labels: &[&str]) -> PullRequest {
        PullRequest {
            number: 1,
            title: title.to_owned(),
            state: odx_forge::PrState::Open,
            head_sha: String::new(),
            base: "18.0".to_owned(),
            merged_at: None,
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            head_repository: None,
            head_branch: String::new(),
            maintainer_can_modify: false,
            author: String::new(),
            created_at: String::new(),
        }
    }

    #[test]
    fn titles_name_addons_as_words() {
        let titled = |title: &str| migrates(&pr(title, &[]), "contract");
        assert!(titled("[18.0][MIG] contract: Migration to 18.0"));
        assert!(titled("[MIG] contract_sale, contract"));
        assert!(titled("[18.0][mig]contract"));
        assert!(titled("[18.0][ADD] contract"));
        assert!(!titled("[18.0][MIG] contract_sale: Migration"));
        assert!(!titled("[18.0][MIG] sale_contract"));
        assert!(!titled("[18.0][FIX] contract"));
        assert!(!titled("Migration of contract to 18.0"));
        assert!(names("[17.0][FIX] pos_x", "pos_x"));
        assert!(!names("[17.0][FIX] pos_x", "pos"));
        assert!(!names("anything", ""));
    }

    #[test]
    fn labels_say_which_addons_a_pull_request_touches() {
        let labelled = |title: &str, labels: &[&str], addon: &str| {
            migrates(
                &pr(
                    title,
                    &["series:18.0", "approved"]
                        .iter()
                        .chain(labels)
                        .copied()
                        .collect::<Vec<_>>(),
                ),
                addon,
            )
        };
        // A title that misspells the addon, labelled by the bot.
        assert!(labelled(
            "[18.0][MIG] contrat",
            &["mod:contract"],
            "contract"
        ));
        // A dependency the migration touches is not what it migrates.
        let both = [
            "mod:dbfilter_from_header",
            "mod:base_view_inheritance_extension",
        ];
        assert!(labelled(
            "[18.0][MIG] dbfilter_from_header",
            &both,
            "dbfilter_from_header"
        ));
        assert!(!labelled(
            "[18.0][MIG] dbfilter_from_header",
            &both,
            "base_view_inheritance_extension"
        ));
        // A title naming none of them leaves every labelled addon.
        assert!(labelled(
            "[18.0][MIG] subscriptions",
            &["mod:contract_sale", "mod:contract"],
            "contract_sale"
        ));
        assert!(!labelled(
            "[18.0][MIG] contract",
            &["mod:contract_sale"],
            "contract"
        ));
        // Untitled, every labelled addon counts: a pull request adding two.
        assert!(labelled(
            "[18.0][ADD] Subscriptions",
            &["mod:contract_sale", "mod:contract"],
            "contract"
        ));
        // Labels and titles in another case.
        assert!(labelled(
            "[18.0][MIG] Contract",
            &["mod:Contract"],
            "contract"
        ));
        assert!(labelled("[18.0][MIG] Contract", &[], "Contract"));
        assert!(!labelled(
            "[18.0][IMP] contract",
            &["mod:contract"],
            "contract"
        ));
    }

    #[test]
    fn pages_and_heads_per_forge() {
        assert_eq!(
            refs(ForgeKind::Gitlab, "https://gitlab.com/g/r", 4),
            (
                "https://gitlab.com/g/r/-/merge_requests/4".to_owned(),
                "refs/merge-requests/4/head".to_owned()
            )
        );
        assert_eq!(
            refs(ForgeKind::Gitea, "https://codeberg.org/o/r", 2).0,
            "https://codeberg.org/o/r/pulls/2"
        );
        assert_eq!(
            refs(ForgeKind::Github, "https://github.com/o/r", 3).1,
            "refs/pull/3/head"
        );
        // A series odx has no list for yet uses the newest one.
        assert!(core("99.0").contains(&"base"));
    }
}
