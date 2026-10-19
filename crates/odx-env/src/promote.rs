//! Promotion plans: what promoting a selection of addons from one
//! environment into another copies, and what stands in the way.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use odx_config::deps::normalize_name;
use odx_config::lock::{CiState, Lockfile};
use odx_config::uvlock::UvLock;

use crate::{Change, Kind, Unit, compare, packages};

/// A project addon as a promotion sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Addon {
    /// Technical name.
    pub name: String,
    /// Manifest version.
    pub version: Option<String>,
    /// The tree of its folder.
    pub tree: String,
    /// Manifest `depends`.
    pub depends: Vec<String>,
}

/// One environment's side of a promotion.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot<'a> {
    /// Environment name.
    pub env: &'a str,
    /// The series it runs.
    pub series: &'a str,
    /// Its own addons.
    pub addons: &'a [Addon],
    /// Its `uv.lock`.
    pub uv: Option<&'a UvLock>,
    /// Its `odx.lock`.
    pub lock: Option<&'a Lockfile>,
}

/// What stands in the way of copying one addon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// The target has a newer version of a package.
    Downgrade,
    /// The target has the addon from another source.
    SourceChange,
    /// The target has a newer version of a project addon: sync it back
    /// first.
    VersionConflict,
}

impl Conflict {
    /// Its name in output.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Downgrade => "downgrade",
            Self::SourceChange => "source change",
            Self::VersionConflict => "version conflict",
        }
    }
}

/// An addon in the closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Addon name.
    pub addon: String,
    /// On the source.
    pub from: Unit,
    /// On the target.
    pub to: Option<Unit>,
    /// How the target differs.
    pub change: Change,
    /// What stands in the way, if anything.
    pub conflict: Option<Conflict>,
    /// Whether a flag allows the conflict.
    pub allowed: bool,
}

/// Flags that change a plan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Copy an older package over a newer one.
    pub allow_downgrade: bool,
    /// Replace an addon from another source.
    pub allow_source_change: bool,
    /// Copy the Odoo core revision too.
    pub with_odoo: bool,
}

/// A promotion plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The selected addons.
    pub selection: Vec<String>,
    /// The closure, selection first, then in dependency order.
    pub items: Vec<Item>,
    /// Dependencies neither the project nor a package provides: Odoo's
    /// own addons, or addons of aggregated repositories.
    pub external: Vec<String>,
    /// The Odoo core on both sides, when it differs.
    pub core: Option<(Unit, Option<Unit>)>,
    /// Things reviewers should know.
    pub warnings: Vec<String>,
}

impl Plan {
    /// Whether a conflict no flag allows refuses the plan.
    #[must_use]
    pub fn refused(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.conflict.is_some() && !item.allowed)
    }

    /// Whether the plan changes anything on the target.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.core.is_none()
            && self
                .items
                .iter()
                .all(|item| item.change == Change::Identical)
    }
}

/// Why no plan could be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The environments run different series.
    SeriesMismatch,
    /// Selected addons the source does not have.
    Missing(Vec<String>),
}

/// The addon a package distributes: `odoo-addon-sale-x` and
/// `odoo19-addon-sale-x` are `sale_x`.
#[must_use]
pub fn addon_of_dist(dist: &str) -> Option<String> {
    let dist = normalize_name(dist);
    let rest = dist.strip_prefix("odoo-addon-").or_else(|| {
        let (series, rest) = dist.strip_prefix("odoo")?.split_once("-addon-")?;
        series.chars().all(|c| c.is_ascii_digit()).then_some(rest)
    })?;
    Some(rest.replace('-', "_"))
}

/// What a snapshot knows of addons: the project's, and the packaged ones
/// with their dependencies.
struct Known<'a> {
    project: BTreeMap<&'a str, &'a Addon>,
    /// Addon → its package's unit and the addons it depends on.
    packaged: BTreeMap<String, (Unit, Vec<String>)>,
}

impl<'a> Known<'a> {
    fn of(snapshot: &Snapshot<'a>) -> Self {
        let mut packaged = BTreeMap::new();
        if let Some(uv) = snapshot.uv {
            let units: BTreeMap<String, Unit> = packages(uv)
                .into_iter()
                .map(|unit| (unit.name.clone(), unit))
                .collect();
            for (dist, unit) in units {
                let Some(addon) = addon_of_dist(&dist) else {
                    continue;
                };
                let depends = uv
                    .packages
                    .iter()
                    .find(|package| package.name == dist)
                    .into_iter()
                    .flat_map(|package| &package.dependencies)
                    .filter_map(|dep| addon_of_dist(&dep.name))
                    .collect();
                packaged.entry(addon).or_insert((unit, depends));
            }
        }
        Self {
            project: snapshot
                .addons
                .iter()
                .map(|addon| (addon.name.as_str(), addon))
                .collect(),
            packaged,
        }
    }

    /// The unit of `addon` and what it depends on: the project's addon
    /// first.
    fn get(&self, addon: &str) -> Option<(Unit, Vec<String>)> {
        if let Some(own) = self.project.get(addon) {
            return Some((
                Unit {
                    kind: Kind::Addon,
                    name: own.name.clone(),
                    version: own.version.clone(),
                    source: "project".to_owned(),
                    pin: own.tree.clone(),
                },
                own.depends.clone(),
            ));
        }
        self.packaged.get(addon).cloned()
    }
}

/// The plan of promoting `selection`, with its dependency closure, from
/// `from` into `to`.
///
/// # Errors
///
/// When the series differ, or the source lacks a selected addon.
pub fn plan(
    from: &Snapshot<'_>,
    to: &Snapshot<'_>,
    selection: &[String],
    options: Options,
) -> Result<Plan, PlanError> {
    if from.series != to.series {
        return Err(PlanError::SeriesMismatch);
    }
    let source = Known::of(from);
    let target = Known::of(to);
    let missing: Vec<String> = selection
        .iter()
        .filter(|name| source.get(name).is_none())
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(PlanError::Missing(missing));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = selection.iter().cloned().collect();
    let mut items = Vec::new();
    let mut external = BTreeSet::new();
    let mut warnings = Vec::new();
    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some((unit, depends)) = source.get(&name) else {
            external.insert(name);
            continue;
        };
        queue.extend(depends);
        let theirs = target.get(&name).map(|(unit, _)| unit);
        let change = compare(Some(&unit), theirs.as_ref());
        let conflict = match (change, unit.kind) {
            (Change::SourceChanged, _) => Some(Conflict::SourceChange),
            (Change::Behind, Kind::Addon) => Some(Conflict::VersionConflict),
            (Change::Behind, _) => Some(Conflict::Downgrade),
            _ => None,
        };
        let allowed = match conflict {
            Some(Conflict::Downgrade) => options.allow_downgrade,
            Some(Conflict::SourceChange) => options.allow_source_change,
            _ => false,
        };
        if unit.kind == Kind::Addon && change == Change::Changed {
            warnings.push(format!(
                "{name} differs at the same version {}: bump it, or the target does not update it",
                unit.version.as_deref().unwrap_or("(none)")
            ));
        }
        items.push(Item {
            addon: name,
            from: unit,
            to: theirs,
            change,
            conflict,
            allowed,
        });
    }
    let core = options
        .with_odoo
        .then(|| core_drift(from.lock, to.lock))
        .flatten();
    for repo in from.lock.iter().flat_map(|lock| &lock.repositories) {
        for merge in &repo.merges {
            if merge.upstream_head_ci == Some(CiState::Red) {
                warnings.push(format!(
                    "{}: {} is pinned at {} while its new head is red; the pinned commit is what was tested",
                    repo.name,
                    merge.source.pr.map_or_else(|| merge.sha.clone(), |pr| format!("PR {pr}")),
                    merge.sha.chars().take(7).collect::<String>()
                ));
            }
        }
    }
    Ok(Plan {
        selection: selection.to_vec(),
        items,
        external: external.into_iter().collect(),
        core,
        warnings,
    })
}

/// The Odoo core of both sides, when they differ.
fn core_drift(from: Option<&Lockfile>, to: Option<&Lockfile>) -> Option<(Unit, Option<Unit>)> {
    let core = |lock: Option<&Lockfile>| {
        lock.and_then(|lock| lock.odoo.as_ref()).map(|odoo| Unit {
            kind: Kind::Odoo,
            name: "odoo".to_owned(),
            version: None,
            source: odoo.url.clone(),
            pin: odoo.rev.clone(),
        })
    };
    let ours = core(from)?;
    let theirs = core(to);
    (theirs.as_ref() != Some(&ours)).then_some((ours, theirs))
}

#[cfg(test)]
mod tests {
    use odx_config::lock::{LockSource, LockedMerge, LockedRepository, MergeStatus, OdooPin};

    use std::fmt::Write as _;

    use super::*;

    fn addon(name: &str, version: &str, tree: &str, depends: &[&str]) -> Addon {
        Addon {
            name: name.to_owned(),
            version: Some(version.to_owned()),
            tree: tree.to_owned(),
            depends: depends.iter().map(|dep| (*dep).to_owned()).collect(),
        }
    }

    fn uv(packages: &[(&str, &str, &[&str])]) -> UvLock {
        let mut text = String::new();
        for (name, version, deps) in packages {
            let deps: Vec<String> = deps
                .iter()
                .map(|dep| format!("{{ name = \"{dep}\" }}"))
                .collect();
            let _ = write!(
                text,
                "[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n\
                 source = {{ registry = \"https://pypi.org/simple\" }}\ndependencies = [{}]\n\n",
                deps.join(", ")
            );
        }
        UvLock::parse(&text).unwrap()
    }

    fn lock(rev: &str, red: bool) -> Lockfile {
        let mut lock = Lockfile::new();
        lock.odoo = Some(OdooPin {
            url: "https://github.com/OCA/OCB.git".to_owned(),
            rev: rev.to_owned(),
        });
        lock.repositories.push(LockedRepository {
            name: "pos".to_owned(),
            url: "u".to_owned(),
            base: "19.0".to_owned(),
            base_sha: "b".to_owned(),
            result_tree: "t".to_owned(),
            merges: vec![LockedMerge {
                source: LockSource {
                    pr: Some(7),
                    sha: None,
                    branch: None,
                    remote: None,
                    tag: None,
                },
                sha: "0123456789".to_owned(),
                tree: "t".to_owned(),
                patch_id: "p".to_owned(),
                mirror_ref: None,
                status: MergeStatus::Pinned,
                upstream_head: None,
                upstream_head_ci: red.then_some(CiState::Red),
                upstream_head_classification: None,
                pinned_since: None,
            }],
        });
        lock
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one scenario")]
    fn closures_and_conflicts() {
        let ours = [
            addon(
                "sale_x",
                "19.0.1.1.0",
                "t1",
                &["sale", "helper", "odoo_lib"],
            ),
            addon("helper", "19.0.1.0.0", "t2", &["base"]),
            addon("older", "19.0.1.0.0", "t3", &[]),
            addon("unbumped", "19.0.1.0.0", "t4", &[]),
            addon("moved", "19.0.1.0.0", "t5", &[]),
        ];
        let theirs = [
            addon("helper", "19.0.1.0.0", "t2", &["base"]),
            addon("older", "19.0.2.0.0", "t6", &[]),
            addon("unbumped", "19.0.1.0.0", "t7", &[]),
        ];
        let our_uv = uv(&[
            (
                "odoo-addon-odoo-lib",
                "19.0.1.2.0",
                &["odoo-addon-deep", "requests"],
            ),
            ("odoo19-addon-deep", "19.0.1.0.0", &[]),
            ("odoo-addon-pkg-old", "19.0.1.0.0", &[]),
            ("requests", "2.0", &[]),
        ]);
        let their_uv = uv(&[
            ("odoo-addon-odoo-lib", "19.0.1.0.0", &[]),
            ("odoo-addon-pkg-old", "19.0.3.0.0", &[]),
            ("odoo-addon-moved", "19.0.1.0.0", &[]),
        ]);
        let (our_lock, their_lock) = (lock("aaa", true), lock("bbb", false));
        let from = Snapshot {
            env: "test",
            series: "19.0",
            addons: &ours,
            uv: Some(&our_uv),
            lock: Some(&our_lock),
        };
        let to = Snapshot {
            env: "prod",
            series: "19.0",
            addons: &theirs,
            uv: Some(&their_uv),
            lock: Some(&their_lock),
        };
        let selection: Vec<String> = ["sale_x", "older", "unbumped", "moved", "pkg_old", "sale_x"]
            .map(str::to_owned)
            .to_vec();
        let options = Options {
            with_odoo: true,
            ..Options::default()
        };
        let plan = plan(&from, &to, &selection, options).unwrap();
        let found: Vec<(&str, &str, Option<&str>)> = plan
            .items
            .iter()
            .map(|item| {
                (
                    item.addon.as_str(),
                    item.change.name(),
                    item.conflict.map(Conflict::name),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("sale_x", "only-in-a", None),
                ("older", "behind", Some("version conflict")),
                ("unbumped", "changed", None),
                ("moved", "source-changed", Some("source change")),
                ("pkg_old", "behind", Some("downgrade")),
                ("helper", "identical", None),
                ("odoo_lib", "ahead", None),
                ("deep", "only-in-a", None),
            ]
        );
        assert_eq!(plan.external, ["base", "sale"]);
        assert!(plan.refused());
        assert!(!plan.is_noop());
        let (core, theirs_core) = plan.core.clone().unwrap();
        assert_eq!(
            (core.pin.as_str(), theirs_core.unwrap().pin.as_str()),
            ("aaa", "bbb")
        );
        assert_eq!(
            plan.warnings,
            [
                "unbumped differs at the same version 19.0.1.0.0: bump it, or the target does not update it",
                "pos: PR 7 is pinned at 0123456 while its new head is red; the pinned commit is what was tested",
            ]
        );
        // Flags allow downgrades and source changes, never version
        // conflicts.
        let allowing = Options {
            allow_downgrade: true,
            allow_source_change: true,
            with_odoo: false,
        };
        let allowed = super::plan(&from, &to, &selection[3..5], allowing).unwrap();
        assert!(!allowed.refused());
        assert!(allowed.items.iter().all(|item| item.allowed));
        assert_eq!(allowed.core, None);
        let version = super::plan(&from, &to, &selection[1..2], allowing).unwrap();
        assert!(version.refused());
    }

    #[test]
    fn what_refuses_a_plan() {
        let ours = [addon("a", "19.0.1.0.0", "t", &[])];
        let empty = Lockfile::new();
        let from = Snapshot {
            env: "test",
            series: "19.0",
            addons: &ours,
            uv: None,
            lock: Some(&empty),
        };
        let other = Snapshot {
            series: "20.0",
            ..from
        };
        assert_eq!(
            plan(&from, &other, &["a".to_owned()], Options::default()),
            Err(PlanError::SeriesMismatch)
        );
        assert_eq!(
            plan(&from, &from, &["b".to_owned()], Options::default()),
            Err(PlanError::Missing(vec!["b".to_owned()]))
        );
        let same = plan(
            &from,
            &from,
            &["a".to_owned()],
            Options {
                with_odoo: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(same.is_noop());
        // A core on one side only is drift; none at all is not.
        let mut core = Lockfile::new();
        core.odoo = Some(OdooPin {
            url: "u".to_owned(),
            rev: "r".to_owned(),
        });
        assert!(core_drift(Some(&core), None).is_some());
        assert!(core_drift(None, Some(&core)).is_none());
        // A red head of a merge of a commit rather than a pull request.
        let green = lock("r", false);
        let calm = Snapshot {
            lock: Some(&green),
            ..from
        };
        let quiet = plan(&calm, &from, &["a".to_owned()], Options::default()).unwrap();
        assert_eq!(quiet.warnings, Vec::<String>::new());
        let mut red = lock("r", true);
        red.repositories[0].merges[0].source.pr = None;
        let warned = Snapshot {
            lock: Some(&red),
            ..from
        };
        let found = plan(&warned, &from, &["a".to_owned()], Options::default()).unwrap();
        assert_eq!(
            found.warnings,
            [
                "pos: 0123456789 is pinned at 0123456 while its new head is red; the pinned commit is what was tested"
            ]
        );
        assert_eq!(Conflict::Downgrade.name(), "downgrade");
    }

    #[test]
    fn package_names() {
        assert_eq!(
            addon_of_dist("odoo-addon-sale_x"),
            Some("sale_x".to_owned())
        );
        assert_eq!(
            addon_of_dist("odoo19-addon-sale-x"),
            Some("sale_x".to_owned())
        );
        assert_eq!(addon_of_dist("odoox-addon-a"), None);
        assert_eq!(addon_of_dist("odoo"), None);
        assert_eq!(addon_of_dist("requests"), None);
    }
}
