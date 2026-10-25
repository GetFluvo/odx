//! `odx changelog`: release notes between two states of `odx.lock`, per
//! repository and package, with the migration scripts they bring in.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use odx_config::Problem;
use odx_config::lock::{LockSource, LockedMerge, LockedPackage, LockedRepository, Lockfile};
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{ForgeKind, RepoUrl};
use odx_git::Merger;

use super::Report;
use super::aggregate::prepare;
use super::sniff::{describe, previous_lock, since, to_json};
use crate::Context;
use crate::cli::{ChangelogArgs, GlobalArgs};

/// Where a merge or package comes from, as configured, with its page on
/// the forge for a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Source {
    source: LockSource,
    link: Option<String>,
}

impl Source {
    fn of(source: &LockSource, url: &str) -> Self {
        let link = source.pr.and_then(|number| {
            let repo = RepoUrl::parse(url)?;
            let base = format!("{}/{}", repo.origin(), repo.path);
            Some(super::deps::refs(ForgeKind::guess(&repo.host), &base, number.into()).0)
        });
        Self {
            source: source.clone(),
            link,
        }
    }

    /// `#12` (linked), `branch x`, `branch x of <remote>`, `commit abc1234`
    /// or `tag v1`.
    fn markdown(&self) -> String {
        let source = &self.source;
        if let Some(number) = source.pr {
            return self.link.as_ref().map_or_else(
                || format!("#{number}"),
                |link| format!("[#{number}]({link})"),
            );
        }
        if let Some(branch) = &source.branch {
            return source.remote.as_ref().map_or_else(
                || format!("branch `{branch}`"),
                |remote| format!("branch `{branch}` of {remote}"),
            );
        }
        if let Some(tag) = &source.tag {
            return format!("tag `{tag}`");
        }
        format!("commit `{}`", short(source.sha.as_deref().unwrap_or("?")))
    }

    fn json(&self) -> serde_json::Value {
        let source = &self.source;
        serde_json::json!({
            "pr": source.pr,
            "sha": source.sha,
            "branch": source.branch,
            "remote": source.remote,
            "tag": source.tag,
            "link": self.link,
        })
    }
}

/// One change in a section.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// A merge or package that is new, at a commit.
    Added { source: Option<Source>, at: String },
    /// A merge or package that is gone.
    Removed { source: Option<Source> },
    /// A commit, base or URL that moved; `what` is `updated`, `base` or `url`.
    Moved {
        what: &'static str,
        source: Option<Source>,
        from: String,
        to: String,
        commits: Option<usize>,
    },
    /// A merge whose lock-time status changed (merged, skipped, pinned).
    Status {
        source: Source,
        from: &'static str,
        to: &'static str,
        since: Option<String>,
    },
}

impl Item {
    fn markdown(&self) -> String {
        let label = |source: &Option<Source>| {
            source
                .as_ref()
                .map(|source| format!(" {}", source.markdown()))
                .unwrap_or_default()
        };
        match self {
            Self::Added { source, at } => format!("- added{} at `{at}`", label(source)),
            Self::Removed { source } => format!("- removed{}", label(source)),
            Self::Moved {
                what,
                source,
                from,
                to,
                commits,
            } => {
                let commits = commits.map_or_else(String::new, |count| match count {
                    1 => " (1 commit)".to_owned(),
                    count => format!(" ({count} commits)"),
                });
                format!("- {what}{}: `{from}` → `{to}`{commits}", label(source))
            }
            Self::Status {
                source,
                from,
                to,
                since,
            } => {
                let since = since
                    .as_ref()
                    .map(|date| format!(" since {date}"))
                    .unwrap_or_default();
                format!("- {}: {to}{since} (was {from})", source.markdown())
            }
        }
    }

    fn json(&self) -> serde_json::Value {
        let source = |source: &Option<Source>| source.as_ref().map(Source::json);
        match self {
            Self::Added { source: s, at } => {
                serde_json::json!({"change": "added", "source": source(s), "to": at})
            }
            Self::Removed { source: s } => {
                serde_json::json!({"change": "removed", "source": source(s)})
            }
            Self::Moved {
                what,
                source: s,
                from,
                to,
                commits,
            } => serde_json::json!({
                "change": what,
                "source": source(s),
                "from": from,
                "to": to,
                "commits": commits,
            }),
            Self::Status {
                source: s,
                from,
                to,
                since,
            } => serde_json::json!({
                "change": "status",
                "source": s.json(),
                "from": from,
                "to": to,
                "since": since,
            }),
        }
    }
}

/// Odoo, a repository or a package, and what changed in it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Section {
    /// `odoo`, `repository` or `package`.
    kind: &'static str,
    name: String,
    /// `added`, `removed` or `changed`.
    state: &'static str,
    items: Vec<Item>,
}

/// Commits between two revisions of the repository at a URL, when known.
type Count<'a> = &'a dyn Fn(&str, &str, &str) -> Option<usize>;

/// `id` shortened to 7 characters.
fn short(id: &str) -> String {
    id.chars().take(7).collect()
}

/// The sections that differ between `old` and `new`: Odoo first, then
/// repositories, then packages, each by name.
fn compare(old: &Lockfile, new: &Lockfile, count: Count<'_>) -> Vec<Section> {
    let mut sections = Vec::new();
    let odoo = match (&old.odoo, &new.odoo) {
        (None, None) => None,
        (None, Some(pin)) => Some((
            "added",
            Item::Added {
                source: None,
                at: short(&pin.rev),
            },
        )),
        (Some(_), None) => Some(("removed", Item::Removed { source: None })),
        (Some(a), Some(b)) => (a.rev != b.rev).then(|| {
            (
                "changed",
                Item::Moved {
                    what: "updated",
                    source: None,
                    from: short(&a.rev),
                    to: short(&b.rev),
                    commits: count(&b.url, &a.rev, &b.rev),
                },
            )
        }),
    };
    sections.extend(odoo.map(|(state, item)| Section {
        kind: "odoo",
        name: "Odoo".to_owned(),
        state,
        items: vec![item],
    }));
    let names: BTreeSet<&str> = old
        .repositories
        .iter()
        .chain(&new.repositories)
        .map(|entry| entry.name.as_str())
        .collect();
    for name in names {
        let Some(entry) = repository(new, name) else {
            sections.push(Section {
                kind: "repository",
                name: name.to_owned(),
                state: "removed",
                items: Vec::new(),
            });
            continue;
        };
        let before = repository(old, name);
        let items = merges(before, entry, count);
        if before.is_some() && items.is_empty() {
            continue;
        }
        sections.push(Section {
            kind: "repository",
            name: name.to_owned(),
            state: if before.is_some() { "changed" } else { "added" },
            items,
        });
    }
    let dists: BTreeSet<&String> = old.packages.keys().chain(new.packages.keys()).collect();
    for dist in dists {
        let (state, items) = match (old.packages.get(dist), new.packages.get(dist)) {
            (_, None) => ("removed", Vec::new()),
            (None, Some(package)) => ("added", vec![added(package)]),
            (Some(a), Some(b)) => ("changed", package(a, b, count)),
        };
        if state == "changed" && items.is_empty() {
            continue;
        }
        sections.push(Section {
            kind: "package",
            name: dist.clone(),
            state,
            items,
        });
    }
    sections
}

fn repository<'a>(lock: &'a Lockfile, name: &str) -> Option<&'a LockedRepository> {
    lock.repositories.iter().find(|entry| entry.name == name)
}

/// What changed in a repository: its URL, its base and its merges, matched
/// by their configured source.
fn merges(old: Option<&LockedRepository>, new: &LockedRepository, count: Count<'_>) -> Vec<Item> {
    let mut items = Vec::new();
    let earlier: &[LockedMerge] = old.map_or(&[][..], |old| &old.merges);
    if let Some(old) = old {
        if old.url != new.url {
            items.push(Item::Moved {
                what: "url",
                source: None,
                from: old.url.clone(),
                to: new.url.clone(),
                commits: None,
            });
        }
        if (&old.base, &old.base_sha) != (&new.base, &new.base_sha) {
            items.push(Item::Moved {
                what: "base",
                source: None,
                from: format!("{} {}", old.base, short(&old.base_sha)),
                to: format!("{} {}", new.base, short(&new.base_sha)),
                commits: count(&new.url, &old.base_sha, &new.base_sha),
            });
        }
    }
    for merge in &new.merges {
        let source = Source::of(&merge.source, &new.url);
        let Some(before) = earlier.iter().find(|m| m.source == merge.source) else {
            items.push(Item::Added {
                source: Some(source),
                at: short(&merge.sha),
            });
            continue;
        };
        if before.sha != merge.sha {
            items.push(Item::Moved {
                what: "updated",
                source: Some(source.clone()),
                from: short(&before.sha),
                to: short(&merge.sha),
                commits: count(&new.url, &before.sha, &merge.sha),
            });
        }
        if before.status != merge.status {
            items.push(Item::Status {
                source,
                from: before.status.as_str(),
                to: merge.status.as_str(),
                since: merge.pinned_since.clone(),
            });
        }
    }
    let url = old.map_or(new.url.as_str(), |old| old.url.as_str());
    items.extend(
        earlier
            .iter()
            .filter(|before| new.merges.iter().all(|m| m.source != before.source))
            .map(|before| Item::Removed {
                source: Some(Source::of(&before.source, url)),
            }),
    );
    items
}

fn added(package: &LockedPackage) -> Item {
    Item::Added {
        source: Some(Source::of(&package.source, &package.url)),
        at: short(&package.sha),
    }
}

/// What changed in a package: another source is the old one removed and
/// the new one added; the same source at another commit is an update.
fn package(old: &LockedPackage, new: &LockedPackage, count: Count<'_>) -> Vec<Item> {
    if (&old.url, &old.source) != (&new.url, &new.source) {
        return vec![
            Item::Removed {
                source: Some(Source::of(&old.source, &old.url)),
            },
            added(new),
        ];
    }
    if old.sha == new.sha {
        return Vec::new();
    }
    vec![Item::Moved {
        what: "updated",
        source: Some(Source::of(&new.source, &new.url)),
        from: short(&old.sha),
        to: short(&new.sha),
        commits: count(&new.url, &old.sha, &new.sha),
    }]
}

fn markdown(title: &str, sections: &[Section], migrations: &str) -> String {
    let mut out = format!("# Changelog: {title}\n");
    if sections.is_empty() {
        out.push_str("\nNo changes.\n");
    }
    for section in sections {
        let state = match section.state {
            "added" => ", new",
            "removed" => ", removed",
            _ => "",
        };
        let kind = if section.kind == "odoo" {
            String::new()
        } else {
            format!(" ({}{state})", section.kind)
        };
        let _ = writeln!(out, "\n## {}{kind}", section.name);
        if !section.items.is_empty() {
            out.push('\n');
        }
        for item in &section.items {
            let _ = writeln!(out, "{}", item.markdown());
        }
    }
    if !migrations.is_empty() {
        let _ = write!(out, "\n## Migrations\n\n```text\n{migrations}```\n");
    }
    out
}

/// `odx changelog [--since REV] [--to REV]`.
pub(crate) fn run_changelog(
    args: &ChangelogArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    changelog(args, global, ctx).unwrap_or_else(|report| report)
}

fn changelog(
    args: &ChangelogArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let root = &prepared.project.root;
    let old = previous_lock(root, &prepared.lock_path, &args.since)?;
    let (new, to) = if let Some(rev) = &args.to {
        (previous_lock(root, &prepared.lock_path, rev)?, rev.clone())
    } else {
        let lock = prepared.lock.ok_or_else(|| {
            Report::error(
                Exit::Failure,
                format!(
                    "{} is missing; run `odx lock`",
                    prepared.lock_path.display()
                ),
            )
        })?;
        (lock, "working tree".to_owned())
    };
    let merger = Merger::new(Arc::new(OsRunner));
    let cache = &prepared.cache;
    let count = |url: &str, from: &str, to: &str| merger.count(&cache.repo_dir(url), from, to);
    let sections = compare(&old, &new, &count);
    let mut problems = Vec::new();
    let sniffed = since(&merger, cache, &old, &new, &mut problems);
    let title = format!("{} → {to}", args.since);
    let out = if global.json {
        let sections: Vec<serde_json::Value> = sections
            .iter()
            .map(|section| {
                serde_json::json!({
                    "kind": section.kind,
                    "name": section.name,
                    "state": section.state,
                    "items": section.items.iter().map(Item::json).collect::<Vec<_>>(),
                })
            })
            .collect();
        let value = serde_json::json!({
            "schema": 1,
            "since": args.since,
            "to": to,
            "sections": sections,
            "migrations": to_json(&sniffed),
            "errors": problems,
        });
        format!("{value:#}\n")
    } else {
        markdown(&title, &sections, &describe(&sniffed))
    };
    Ok(Report {
        out,
        warnings: problems
            .into_iter()
            .map(|message| Problem {
                key: "migrations".to_owned(),
                message,
            })
            .collect(),
        ..Report::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn lock(text: &str) -> Lockfile {
        Lockfile::parse(&format!("version = 1\n{text}")).unwrap()
    }

    fn repository(name: &str, base_sha: &str, merges: &str) -> String {
        format!(
            "[[repositories]]\nname = \"{name}\"\nurl = \"https://github.com/OCA/{name}.git\"\n\
             base = \"17.0\"\nbase_sha = \"{base_sha}\"\nresult_tree = \"{C}\"\n{merges}"
        )
    }

    fn merge(source: &str, sha: &str, status: &str) -> String {
        format!(
            "[[repositories.merges]]\nsource = {source}\nsha = \"{sha}\"\ntree = \"{C}\"\n\
             patch_id = \"{C}\"\nstatus = \"{status}\"\n"
        )
    }

    fn package(dist: &str, source: &str, sha: &str) -> String {
        format!(
            "[packages.{dist}]\nurl = \"https://codeberg.org/o/{dist}.git\"\nsource = {source}\nsha = \"{sha}\"\n"
        )
    }

    fn render(old: &Lockfile, new: &Lockfile) -> String {
        let count = |_: &str, from: &str, to: &str| (from != C && to != C).then_some(3);
        markdown("a → b", &compare(old, new, &count), "")
    }

    #[test]
    fn identical_states_have_no_changes() {
        let same = lock(&repository("pos", A, &merge("{ pr = 1 }", A, "merged")));
        assert_eq!(render(&same, &same), "# Changelog: a → b\n\nNo changes.\n");
    }

    #[test]
    fn merges_are_added_updated_removed_and_change_status() {
        let old = lock(&format!(
            "{}{}{}",
            repository("pos", A, &merge("{ pr = 1 }", A, "merged")),
            merge("{ pr = 2 }", A, "merged"),
            merge(
                "{ branch = \"fix\", remote = \"https://example.com/x.git\" }",
                A,
                "merged"
            ),
        ));
        let new = lock(&format!(
            "{}{}{}",
            repository("pos", B, &merge("{ pr = 1 }", B, "merged")),
            merge("{ pr = 2 }", A, "pinned"),
            merge("{ sha = \"abcdef0123\" }", C, "skipped"),
        ));
        assert_eq!(
            render(&old, &new),
            "# Changelog: a → b\n\n## pos (repository)\n\n\
             - base: `17.0 aaaaaaa` → `17.0 bbbbbbb` (3 commits)\n\
             - updated [#1](https://github.com/OCA/pos/pull/1): `aaaaaaa` → `bbbbbbb` (3 commits)\n\
             - [#2](https://github.com/OCA/pos/pull/2): pinned (was merged)\n\
             - added commit `abcdef0` at `ccccccc`\n\
             - removed branch `fix` of https://example.com/x.git\n"
        );
    }

    #[test]
    fn repositories_and_packages_come_and_go() {
        let old = lock(&format!(
            "{}{}{}{}",
            repository("gone", A, ""),
            package("odoo-addon-a", "{ tag = \"1.0\" }", A),
            package("odoo-addon-b", "{ pr = 4 }", A),
            package("odoo-addon-c", "{ pr = 5 }", A),
        ));
        let new = lock(&format!(
            "[odoo]\nurl = \"https://github.com/OCA/OCB.git\"\nrev = \"{A}\"\n{}{}{}{}{}",
            repository("web", A, &merge("{ pr = 9 }", A, "merged")),
            package("odoo-addon-a", "{ pr = 3 }", B),
            package("odoo-addon-b", "{ pr = 4 }", B),
            package("odoo-addon-c", "{ pr = 5 }", A),
            package("odoo-addon-d", "{ branch = \"main\" }", A),
        ));
        assert_eq!(
            render(&old, &new),
            "# Changelog: a → b\n\n## Odoo\n\n- added at `aaaaaaa`\n\n\
             ## gone (repository, removed)\n\n\
             ## web (repository, new)\n\n- added [#9](https://github.com/OCA/web/pull/9) at `aaaaaaa`\n\n\
             ## odoo-addon-a (package)\n\n- removed tag `1.0`\n\
             - added [#3](https://codeberg.org/o/odoo-addon-a/pulls/3) at `bbbbbbb`\n\n\
             ## odoo-addon-b (package)\n\n\
             - updated [#4](https://codeberg.org/o/odoo-addon-b/pulls/4): `aaaaaaa` → `bbbbbbb` (3 commits)\n\n\
             ## odoo-addon-d (package, new)\n\n- added branch `main` at `aaaaaaa`\n"
        );
        // And back: Odoo and the packages removed.
        let back = render(&new, &old);
        assert!(back.contains("## Odoo\n\n- removed\n"), "{back}");
        assert!(
            back.contains("## odoo-addon-d (package, removed)\n"),
            "{back}"
        );
        assert!(back.contains("## web (repository, removed)\n"), "{back}");
    }

    #[test]
    fn odoo_urls_and_unknown_counts() {
        let odoo = |rev: &str| {
            format!("[odoo]\nurl = \"https://github.com/OCA/OCB.git\"\nrev = \"{rev}\"\n")
        };
        let old = lock(&format!("{}{}", odoo(A), repository("pos", A, "")));
        let new = lock(&format!(
            "{}{}",
            odoo(C),
            repository("pos", A, "").replace("OCA/pos", "me/pos")
        ));
        assert_eq!(
            render(&old, &new),
            "# Changelog: a → b\n\n## Odoo\n\n- updated: `aaaaaaa` → `ccccccc`\n\n\
             ## pos (repository)\n\n\
             - url: `https://github.com/OCA/pos.git` → `https://github.com/me/pos.git`\n"
        );
        assert_eq!(
            render(&old, &old.clone()),
            "# Changelog: a → b\n\nNo changes.\n"
        );
    }

    #[test]
    fn single_commits_pins_since_and_pull_requests_off_forges() {
        let item = Item::Moved {
            what: "updated",
            source: None,
            from: "a".to_owned(),
            to: "b".to_owned(),
            commits: Some(1),
        };
        assert_eq!(item.markdown(), "- updated: `a` → `b` (1 commit)");
        let source = Source::of(
            &LockSource {
                pr: Some(7),
                sha: None,
                branch: None,
                remote: None,
                tag: None,
            },
            "/srv/git/pos",
        );
        assert_eq!(source.markdown(), "#7");
        let pinned = Item::Status {
            source,
            from: "merged",
            to: "pinned",
            since: Some("2026-10-01".to_owned()),
        };
        assert_eq!(
            pinned.markdown(),
            "- #7: pinned since 2026-10-01 (was merged)"
        );
        assert_eq!(pinned.json()["since"], "2026-10-01");
        let none = Source::of(
            &LockSource {
                pr: None,
                sha: None,
                branch: None,
                remote: None,
                tag: None,
            },
            "",
        );
        assert_eq!(none.markdown(), "commit `?`");
    }

    #[test]
    fn every_item_has_its_json() {
        let old = lock(&repository("pos", A, &merge("{ pr = 1 }", A, "merged")));
        let new = lock(&repository(
            "pos",
            B,
            &merge("{ tag = \"x\" }", A, "merged"),
        ));
        let items: Vec<serde_json::Value> = compare(&old, &new, &|_, _, _| None)[0]
            .items
            .iter()
            .map(Item::json)
            .collect();
        assert_eq!(
            serde_json::Value::Array(items),
            serde_json::json!([
                {"change": "base", "source": null, "from": "17.0 aaaaaaa", "to": "17.0 bbbbbbb", "commits": null},
                {"change": "added", "source": {"pr": null, "sha": null, "branch": null, "remote": null, "tag": "x", "link": null}, "to": "aaaaaaa"},
                {"change": "removed", "source": {"pr": 1, "sha": null, "branch": null, "remote": null, "tag": null, "link": "https://github.com/OCA/pos/pull/1"}},
            ])
        );
    }

    #[test]
    fn migrations_get_their_own_section() {
        let text = markdown("a → b", &[], "migrations in pos (repository):\n  pos_x\n");
        assert!(
            text.ends_with(
                "## Migrations\n\n```text\nmigrations in pos (repository):\n  pos_x\n```\n"
            ),
            "{text}"
        );
    }
}
