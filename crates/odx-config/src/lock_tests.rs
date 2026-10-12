use proptest::prelude::*;

use super::*;

const SHA: &str = "a1b2c3d4e5f67890123456789012345678901234";
const TREE: &str = "0123456789abcdef0123456789abcdef01234567";

fn merge(source: LockSource) -> LockedMerge {
    LockedMerge {
        source,
        sha: SHA.to_owned(),
        tree: TREE.to_owned(),
        patch_id: SHA.to_owned(),
        mirror_ref: Some(format!("refs/odx/{SHA}")),
        status: MergeStatus::Pinned,
        upstream_head: Some(TREE.to_owned()),
        upstream_head_ci: Some(CiState::Red),
        upstream_head_classification: Some(RedClassification::BaseAlsoFailing),
        pinned_since: Some("2026-10-12".to_owned()),
    }
}

fn sample() -> Lockfile {
    let pr = LockSource {
        pr: Some(123),
        sha: None,
        branch: None,
        remote: None,
    };
    let branch = LockSource {
        pr: None,
        sha: None,
        branch: Some("17.0-fix".to_owned()),
        remote: Some("https://github.com/acme/pos.git".to_owned()),
    };
    let mut lock = Lockfile::new();
    lock.odoo = Some(OdooPin {
        url: "https://github.com/OCA/OCB.git".to_owned(),
        rev: SHA.to_owned(),
    });
    lock.repositories = vec![
        LockedRepository {
            name: "web".to_owned(),
            url: "https://github.com/OCA/web.git".to_owned(),
            base: "17.0".to_owned(),
            base_sha: SHA.to_owned(),
            result_tree: TREE.to_owned(),
            merges: vec![],
        },
        LockedRepository {
            name: "pos".to_owned(),
            url: "https://github.com/OCA/pos.git".to_owned(),
            base: "17.0".to_owned(),
            base_sha: SHA.to_owned(),
            result_tree: TREE.to_owned(),
            merges: vec![merge(pr.clone()), merge(branch)],
        },
    ];
    lock.packages.insert(
        "odoo-addon-pos-vouchers".to_owned(),
        LockedPackage {
            group: Some("oca_unmerged".to_owned()),
            url: "https://github.com/OCA/pos.git".to_owned(),
            source: LockSource { pr: Some(45), ..pr },
            subdirectory: Some("pos_vouchers".to_owned()),
            sha: SHA.to_owned(),
            mirror_ref: None,
        },
    );
    lock
}

#[test]
fn canonical_layout() {
    insta::assert_snapshot!(sample().render());
}

#[test]
fn round_trip_sorts_repositories() {
    let lock = sample();
    let parsed = Lockfile::parse(&lock.render()).unwrap();
    assert_eq!(parsed.repositories[0].name, "pos");
    assert_eq!(parsed.render(), lock.render());
    assert_eq!(Lockfile::new().render(), format!("{HEADER}version = 1\n"));
}

#[test]
fn versions() {
    assert_eq!(
        Lockfile::parse("version = 2\n"),
        Err(LockError::Newer { found: 2 })
    );
    assert_eq!(
        Lockfile::parse("version = 0\n"),
        Err(LockError::Older { found: 0 })
    );
    assert_eq!(
        Lockfile::parse("").unwrap_err().to_string(),
        "odx.lock has no `version`"
    );
    assert_eq!(
        LockError::Newer { found: 2 }.to_string(),
        "odx.lock has version 2; this odx reads version 1, upgrade odx"
    );
    assert_eq!(
        LockError::Older { found: 0 }.to_string(),
        "odx.lock has version 0; run `odx lock --migrate`"
    );
}

#[test]
fn format_errors() {
    for text in ["version = \"x\"\n", "version = 1\nbogus = 1\n"] {
        let err = Lockfile::parse(text).unwrap_err();
        assert_eq!(err, LockError::Format(err.to_string()));
    }
    let text = sample()
        .render()
        .replace("status = \"pinned\"", "status = \"lost\"");
    let err = Lockfile::parse(&text).unwrap_err();
    assert_eq!(err, LockError::Format(err.to_string()));
}

#[test]
fn object_ids_are_checked_everywhere() {
    let rendered = sample().render();
    for (from, key) in [
        (format!("\nrev = \"{SHA}\""), "odoo.rev"),
        (
            format!("\nbase_sha = \"{SHA}\""),
            "repositories.pos.base_sha",
        ),
        (
            format!("\nresult_tree = \"{TREE}\""),
            "repositories.pos.result_tree",
        ),
        (
            format!("\ntree = \"{TREE}\""),
            "repositories.pos.merges[0].tree",
        ),
        (
            format!("\npatch_id = \"{SHA}\""),
            "repositories.pos.merges[0].patch_id",
        ),
        (
            format!("\nupstream_head = \"{TREE}\""),
            "repositories.pos.merges[0].upstream_head",
        ),
    ] {
        let text = rendered.replacen(
            &from,
            &from.replace(&SHA[..4], "zzzz").replace(&TREE[..4], "zzzz"),
            1,
        );
        let err = Lockfile::parse(&text).unwrap_err();
        assert!(err.to_string().starts_with(key), "{key}: {err}");
    }
    let mut lock = sample();
    lock.repositories[1].merges[0].sha = "abc".to_owned();
    assert!(
        Lockfile::parse(&lock.render())
            .unwrap_err()
            .to_string()
            .starts_with("repositories.pos.merges[0].sha")
    );
    let mut lock = sample();
    lock.packages
        .values_mut()
        .for_each(|p| p.sha = "x".repeat(40));
    assert_eq!(
        Lockfile::parse(&lock.render()),
        Err(LockError::BadObjectId {
            key: "packages.odoo-addon-pos-vouchers.sha".to_owned(),
            value: "x".repeat(40)
        })
    );
    let mut sha256 = sample();
    sha256.odoo = Some(OdooPin {
        url: "u".to_owned(),
        rev: "a".repeat(64),
    });
    assert!(Lockfile::parse(&sha256.render()).is_ok());
}

#[test]
fn sources_match_configured_merges() {
    let merge = Merge {
        pr: Some(7),
        sha: None,
        branch: None,
        remote: None,
        on_conflict: None,
        on_head_change: None,
        track_upstream: Some(true),
    };
    let source = LockSource::of(&merge);
    assert!(source.matches(&merge));
    assert!(
        !LockSource {
            pr: Some(8),
            ..source
        }
        .matches(&merge)
    );
}

#[test]
fn enum_names() {
    assert_eq!(RedClassification::OtherAddon.as_str(), "other-addon");
    assert_eq!(CiState::Pending.as_str(), "pending");
    for (status, ci, red) in [
        (
            MergeStatus::Merged,
            Some(CiState::Green),
            Some(RedClassification::Red),
        ),
        (
            MergeStatus::Skipped,
            Some(CiState::Unknown),
            Some(RedClassification::OtherAddon),
        ),
        (MergeStatus::Merged, None, None),
    ] {
        let mut lock = sample();
        let merge = &mut lock.repositories[1].merges[0];
        merge.status = status;
        merge.upstream_head_ci = ci;
        merge.upstream_head_classification = red;
        let parsed = Lockfile::parse(&lock.render()).unwrap();
        let merge = &parsed.repositories[0].merges[0];
        assert_eq!(
            (
                merge.status,
                merge.upstream_head_ci,
                merge.upstream_head_classification
            ),
            (status, ci, red)
        );
    }
}

#[test]
fn removing_entries() {
    let mut lock = sample();
    let pr = LockSource {
        pr: Some(123),
        sha: None,
        branch: None,
        remote: None,
    };
    assert!(lock.remove_merge("git@github.com:oca/pos", &pr));
    assert!(!lock.remove_merge("https://github.com/OCA/pos.git", &pr));
    assert_eq!(lock.repositories[1].merges.len(), 1);
    assert!(lock.remove_package("Odoo_Addon_Pos.Vouchers"));
    assert!(!lock.remove_package("odoo-addon-pos-vouchers"));
}

#[test]
fn repositories_with_the_same_name_sort_by_url() {
    let mut lock = sample();
    let mut twin = lock.repositories[1].clone();
    twin.url = "https://github.com/AAA/pos.git".to_owned();
    lock.repositories.push(twin);
    let parsed = Lockfile::parse(&lock.render()).unwrap();
    assert_eq!(parsed.repositories[0].url, "https://github.com/AAA/pos.git");
}

fn hex(len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(b"0123456789abcdef".to_vec()), len)
        .prop_map(|bytes| bytes.into_iter().map(char::from).collect())
}

fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ._/:@\"'\\\\#=-]{0,24}"
}

fn source() -> impl Strategy<Value = LockSource> {
    (
        proptest::option::of(any::<u32>()),
        proptest::option::of(text()),
        proptest::option::of(text()),
        proptest::option::of(text()),
    )
        .prop_map(|(pr, sha, branch, remote)| LockSource {
            pr,
            sha,
            branch,
            remote,
        })
}

fn locked_merge() -> impl Strategy<Value = LockedMerge> {
    (
        source(),
        hex(40),
        hex(40),
        hex(40),
        proptest::option::of(text()),
        prop_oneof![
            Just(MergeStatus::Merged),
            Just(MergeStatus::Skipped),
            Just(MergeStatus::Pinned)
        ],
        proptest::option::of(hex(40)),
        proptest::option::of(prop_oneof![
            Just(CiState::Green),
            Just(CiState::Red),
            Just(CiState::Pending),
            Just(CiState::Unknown)
        ]),
        proptest::option::of(prop_oneof![
            Just(RedClassification::Red),
            Just(RedClassification::BaseAlsoFailing),
            Just(RedClassification::OtherAddon)
        ]),
        proptest::option::of(text()),
    )
        .prop_map(
            |(
                source,
                sha,
                tree,
                patch_id,
                mirror_ref,
                status,
                upstream_head,
                upstream_head_ci,
                upstream_head_classification,
                pinned_since,
            )| {
                LockedMerge {
                    source,
                    sha,
                    tree,
                    patch_id,
                    mirror_ref,
                    status,
                    upstream_head,
                    upstream_head_ci,
                    upstream_head_classification,
                    pinned_since,
                }
            },
        )
}

fn lockfile() -> impl Strategy<Value = Lockfile> {
    let repo = (
        text(),
        text(),
        text(),
        hex(40),
        hex(40),
        proptest::collection::vec(locked_merge(), 0..3),
    )
        .prop_map(
            |(name, url, base, base_sha, result_tree, merges)| LockedRepository {
                name,
                url,
                base,
                base_sha,
                result_tree,
                merges,
            },
        );
    let package = (
        proptest::option::of(text()),
        text(),
        source(),
        proptest::option::of(text()),
        hex(40),
        proptest::option::of(text()),
    )
        .prop_map(
            |(group, url, source, subdirectory, sha, mirror_ref)| LockedPackage {
                group,
                url,
                source,
                subdirectory,
                sha,
                mirror_ref,
            },
        );
    (
        proptest::option::of((text(), hex(40)).prop_map(|(url, rev)| OdooPin { url, rev })),
        proptest::collection::vec(repo, 0..3),
        proptest::collection::btree_map("[a-z0-9._-]{1,12}", package, 0..3),
    )
        .prop_map(|(odoo, repositories, packages)| Lockfile {
            version: LOCK_VERSION,
            odoo,
            repositories,
            packages,
        })
}

proptest! {
    #[test]
    fn render_is_a_fixed_point(lock in lockfile()) {
        let rendered = lock.render();
        let parsed = Lockfile::parse(&rendered).unwrap();
        prop_assert_eq!(parsed.render(), rendered);
        let mut expected = lock;
        expected.repositories.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.url.cmp(&b.url)));
        prop_assert_eq!(parsed, expected);
    }
}
