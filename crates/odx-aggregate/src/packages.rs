use std::collections::BTreeMap;

use odx_config::deps::{
    add_to_group, normalize_name, read_group, set_direct_ref, set_uv_git_source,
};
use odx_config::lock::{LockSource, LockedPackage};
use odx_config::packages::{PackageOrigin, PackageSpec, uv_git_url};
use odx_core::repo_key;
use odx_git::{FetchPlan, Fetched, Forge, Source};
use toml_edit::DocumentMut;

/// What to fetch for a package source: a pull request, a commit, or a
/// branch or tag name the remote resolves.
///
/// ```
/// use odx_aggregate::package_source;
/// use odx_config::lock::LockSource;
/// use odx_git::Source;
///
/// let tag = LockSource { pr: None, sha: None, branch: None, remote: None, tag: Some("v1".into()) };
/// assert_eq!(package_source(&tag), Source::Ref("refs/tags/v1".into()));
/// ```
pub fn package_source(source: &LockSource) -> Source {
    match (source.pr, &source.sha, &source.tag) {
        (Some(pr), _, _) => Source::Pr(pr),
        (None, Some(sha), _) => Source::Commit(sha.clone()),
        (None, None, Some(tag)) => Source::Ref(format!("refs/tags/{tag}")),
        (None, None, None) => Source::Ref(source.branch.clone().unwrap_or_default()),
    }
}

/// The entry of `locked` for `spec`, when it was locked from the same
/// repository, source and subdirectory.
pub fn locked_package<'a>(
    spec: &PackageSpec,
    locked: &'a BTreeMap<String, LockedPackage>,
) -> Option<&'a LockedPackage> {
    locked.iter().find_map(|(name, entry)| {
        (normalize_name(name) == normalize_name(&spec.dist)
            && repo_key(&entry.url) == repo_key(&spec.url)
            && entry.source == spec.source
            && entry.subdirectory == spec.subdirectory)
            .then_some(entry)
    })
}

/// What to fetch for each package: its locked commit when `locked` pins
/// it, else its source; one plan per repository URL.
pub fn package_fetch_plans(
    specs: &[PackageSpec],
    locked: &BTreeMap<String, LockedPackage>,
    vault: Option<&str>,
) -> Vec<FetchPlan> {
    let mut plans: Vec<FetchPlan> = Vec::new();
    for spec in specs {
        let source = locked_package(spec, locked).map_or_else(
            || package_source(&spec.source),
            |entry| Source::Commit(entry.sha.clone()),
        );
        let key = repo_key(&spec.url);
        if !plans.iter().any(|plan| repo_key(&plan.url) == key) {
            plans.push(FetchPlan {
                url: spec.url.clone(),
                forge: Forge::detect(&spec.url),
                sources: Vec::new(),
                mirror: None,
                fallback: vault.map(str::to_owned),
                clone: None,
                max_age: None,
                offline: false,
            });
        }
        for plan in plans.iter_mut().filter(|plan| repo_key(&plan.url) == key) {
            if !plan.sources.contains(&(None, source.clone())) {
                plan.sources.push((None, source.clone()));
            }
        }
    }
    plans
}

/// The `odx.lock` entries of `specs` from the commits `fetched` for their
/// URLs; an entry keeps its vault ref when its commit did not change.
/// Packages that were not fetched are returned by name.
pub fn package_entries(
    specs: &[PackageSpec],
    fetched: &[(&str, &[Fetched])],
    locked: &BTreeMap<String, LockedPackage>,
) -> (BTreeMap<String, LockedPackage>, Vec<String>) {
    let mut entries = BTreeMap::new();
    let mut missing = Vec::new();
    for spec in specs {
        let pinned = locked_package(spec, locked);
        let wanted = pinned.map_or_else(
            || package_source(&spec.source),
            |entry| Source::Commit(entry.sha.clone()),
        );
        let key = repo_key(&spec.url);
        let sha = fetched
            .iter()
            .filter(|(url, _)| repo_key(url) == key)
            .flat_map(|(_, commits)| commits.iter())
            .find(|f| f.remote.is_none() && f.source == wanted)
            .map(|f| f.sha.clone());
        let Some(sha) = sha else {
            missing.push(spec.dist.clone());
            continue;
        };
        let mirror_ref = pinned
            .filter(|entry| entry.sha == sha)
            .and_then(|entry| entry.mirror_ref.clone());
        entries.insert(
            spec.dist.clone(),
            LockedPackage {
                group: spec.group.clone(),
                url: spec.url.clone(),
                source: spec.source.clone(),
                subdirectory: spec.subdirectory.clone(),
                sha,
                mirror_ref,
            },
        );
    }
    (entries, missing)
}

/// Point every vaulted package at its vault tag.
///
/// `[tool.odx.packages]` entries get a `[tool.uv.sources]` entry (the
/// distribution is added to its group when missing); direct references are
/// rewritten themselves. Packages without a vault ref are left alone.
/// Returns what could not be written.
pub fn rewrite_packages(
    doc: &mut DocumentMut,
    specs: &[PackageSpec],
    entries: &BTreeMap<String, LockedPackage>,
    vault: &str,
) -> Vec<String> {
    let git = uv_git_url(vault);
    let mut failures = Vec::new();
    let vaulted = specs.iter().filter_map(|spec| {
        entries
            .get(&spec.dist)
            .filter(|entry| entry.mirror_ref.is_some())
            .map(|entry| (spec, entry))
    });
    for (spec, entry) in vaulted {
        let tag = format!("odx/{}", entry.sha);
        let group = spec.group.as_deref().and_then(|name| read_group(doc, name));
        let written = match (&spec.origin, group) {
            (PackageOrigin::Table, group) => {
                let listed = group.as_ref().is_none_or(|group| {
                    group
                        .requirements
                        .iter()
                        .any(|req| normalize_name(&req.name) == normalize_name(&spec.dist))
                        || add_to_group(doc, group, &spec.dist)
                });
                listed
                    && set_uv_git_source(doc, &spec.dist, &git, &tag, spec.subdirectory.as_deref())
            }
            (PackageOrigin::DirectReference(_), Some(group)) => {
                let fragment = spec
                    .subdirectory
                    .as_ref()
                    .map(|dir| format!("#subdirectory={dir}"))
                    .unwrap_or_default();
                set_direct_ref(
                    doc,
                    &group,
                    &spec.dist,
                    &format!("git+{git}@{tag}{fragment}"),
                )
            }
            (PackageOrigin::DirectReference(_), None) => false,
        };
        if !written {
            failures.push(spec.dist.clone());
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_git::Via;

    fn spec(dist: &str, url: &str, source: LockSource, origin: PackageOrigin) -> PackageSpec {
        PackageSpec {
            dist: dist.to_owned(),
            group: Some("oca".to_owned()),
            url: url.to_owned(),
            source,
            subdirectory: Some(dist.replace("odoo-addon-", "")),
            origin,
        }
    }

    fn src(pr: Option<u32>, sha: Option<&str>, branch: Option<&str>) -> LockSource {
        LockSource {
            pr,
            sha: sha.map(str::to_owned),
            branch: branch.map(str::to_owned),
            remote: None,
            tag: None,
        }
    }

    fn fetched(source: Source, sha: &str) -> Fetched {
        Fetched {
            remote: None,
            source,
            sha: sha.to_owned(),
            via: Via::Upstream,
        }
    }

    const POS: &str = "https://github.com/OCA/pos.git";

    #[test]
    fn sources() {
        assert_eq!(package_source(&src(Some(1), None, None)), Source::Pr(1));
        assert_eq!(
            package_source(&src(None, Some("abc"), None)),
            Source::Commit("abc".into())
        );
        assert_eq!(
            package_source(&src(None, None, Some("17.0"))),
            Source::Ref("17.0".into())
        );
        assert_eq!(
            package_source(&src(None, None, None)),
            Source::Ref(String::new())
        );
    }

    #[test]
    fn plans_pins_and_entries() {
        let a = spec(
            "odoo-addon-a",
            POS,
            src(Some(1), None, None),
            PackageOrigin::Table,
        );
        let b = spec(
            "odoo-addon-b",
            "git@github.com:oca/pos",
            src(None, None, Some("17.0")),
            PackageOrigin::Table,
        );
        let c = spec(
            "odoo-addon-c",
            "https://github.com/OCA/web.git",
            src(Some(2), None, None),
            PackageOrigin::Table,
        );
        let again = spec(
            "odoo-addon-a2",
            POS,
            src(Some(1), None, None),
            PackageOrigin::Table,
        );
        let mut locked = BTreeMap::new();
        locked.insert(
            "odoo_addon_b".to_owned(),
            LockedPackage {
                group: Some("oca".into()),
                url: POS.into(),
                source: src(None, None, Some("17.0")),
                subdirectory: Some("b".into()),
                sha: "b".repeat(40),
                mirror_ref: Some("refs/tags/odx/bbb".into()),
            },
        );
        let specs = [a, b, c, again];
        let plans = package_fetch_plans(&specs, &locked, Some("vault"));
        assert_eq!(plans.len(), 2);
        assert_eq!(
            plans[0].sources,
            [
                (None, Source::Pr(1)),
                (None, Source::Commit("b".repeat(40)))
            ]
        );
        assert_eq!(plans[0].fallback.as_deref(), Some("vault"));
        assert_eq!(plans[1].forge, Forge::Github);

        let commits = vec![(
            POS.to_owned(),
            vec![
                fetched(Source::Pr(1), "a1"),
                fetched(Source::Commit("b".repeat(40)), &"b".repeat(40)),
            ],
        )];
        let (entries, missing) = package_entries(&specs[..3], &crate::borrowed(&commits), &locked);
        assert_eq!(missing, ["odoo-addon-c"]);
        assert_eq!(entries["odoo-addon-a"].sha, "a1");
        assert_eq!(entries["odoo-addon-a"].mirror_ref, None);
        assert_eq!(
            entries["odoo-addon-b"].mirror_ref.as_deref(),
            Some("refs/tags/odx/bbb")
        );
        // A changed source is not pinned.
        let moved = spec(
            "odoo-addon-b",
            POS,
            src(None, None, Some("16.0")),
            PackageOrigin::Table,
        );
        assert_eq!(locked_package(&moved, &locked), None);
    }

    const PYPROJECT: &str = r#"[project]
name = "client"

[project.optional-dependencies]
oca = [
  "odoo-addon-direct @ git+https://github.com/OCA/pos.git@refs/pull/4/head#subdirectory=direct",
  "odoo-addon-listed",
]
"#;

    fn vaulted(sha: &str) -> LockedPackage {
        LockedPackage {
            group: Some("oca".into()),
            url: POS.into(),
            source: src(Some(4), None, None),
            subdirectory: None,
            sha: sha.to_owned(),
            mirror_ref: Some(format!("refs/tags/odx/{sha}")),
        }
    }

    #[test]
    fn rewrites_point_at_the_vault() {
        let mut doc: DocumentMut = PYPROJECT.parse().unwrap();
        let direct = spec(
            "odoo-addon-direct",
            POS,
            src(Some(4), None, None),
            PackageOrigin::DirectReference("oca".into()),
        );
        let listed = spec(
            "odoo-addon-listed",
            POS,
            src(None, None, Some("17.0")),
            PackageOrigin::Table,
        );
        let added = PackageSpec {
            subdirectory: None,
            ..spec(
                "odoo-addon-added",
                POS,
                src(None, None, Some("17.0")),
                PackageOrigin::Table,
            )
        };
        let unvaulted = spec(
            "odoo-addon-unvaulted",
            POS,
            src(None, None, Some("17.0")),
            PackageOrigin::Table,
        );
        let orphan = PackageSpec {
            group: Some("gone".into()),
            ..spec(
                "odoo-addon-orphan",
                POS,
                src(None, None, Some("17.0")),
                PackageOrigin::DirectReference("gone".into()),
            )
        };
        let groupless = PackageSpec {
            group: None,
            ..spec(
                "odoo-addon-groupless",
                POS,
                src(None, None, Some("17.0")),
                PackageOrigin::Table,
            )
        };
        let mut entries = BTreeMap::new();
        for (name, sha) in [
            ("odoo-addon-direct", "d"),
            ("odoo-addon-listed", "l"),
            ("odoo-addon-added", "n"),
            ("odoo-addon-orphan", "o"),
            ("odoo-addon-groupless", "g"),
        ] {
            entries.insert(name.to_owned(), vaulted(&sha.repeat(40)));
        }
        entries.insert(
            "odoo-addon-unvaulted".to_owned(),
            LockedPackage {
                mirror_ref: None,
                ..vaulted(&"u".repeat(40))
            },
        );
        let specs = [direct, listed, added, unvaulted, orphan, groupless];
        let failures = rewrite_packages(&mut doc, &specs, &entries, "git@forge:odoo/vault.git");
        assert_eq!(failures, ["odoo-addon-orphan"]);
        let text = doc.to_string();
        let d = "d".repeat(40);
        assert!(text.contains(&format!("\"odoo-addon-direct @ git+ssh://git@forge/odoo/vault.git@odx/{d}#subdirectory=direct\"")), "{text}");
        assert!(text.contains("\"odoo-addon-added\""), "{text}");
        assert!(text.contains(&format!("odoo-addon-listed = {{ git = \"ssh://git@forge/odoo/vault.git\", tag = \"odx/{}\", subdirectory = \"listed\" }}", "l".repeat(40))), "{text}");
        assert!(text.contains("odoo-addon-groupless = {"), "{text}");
        assert!(!text.contains("unvaulted"), "{text}");

        // A pyproject whose tool table cannot take sources.
        let mut broken: DocumentMut = format!("tool = 1\n{PYPROJECT}").parse().unwrap();
        assert_eq!(
            rewrite_packages(&mut broken, &specs[1..2], &entries, "v"),
            ["odoo-addon-listed"]
        );
    }
}
