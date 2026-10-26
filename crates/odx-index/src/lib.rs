//! Addon to repository index per Odoo series for odx, including renamed and moved addons.
//!
//! Part of [odx](https://github.com/getfluvo/odx). Addons move between
//! repositories across series, get renamed or merged into others. The index
//! answers where an addon lives on a series ([`AddonIndex::locate`]), where
//! it lived on every series it knows ([`AddonIndex::history`]) and which
//! older names to look for ([`AddonIndex::aliases`]). [`build`] fills one
//! series from the sources in [`sources`], in order of trust: configured
//! overrides, the local object cache, the package index.
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use odx_forge::Http;
use odx_git::{Forge, Merger};
use serde::{Deserialize, Serialize};

pub mod sources;

/// Which source placed an addon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// `[tool.odx.index.overrides]`.
    Override,
    /// A series branch in the local object cache.
    Cache,
    /// The repository URL of the addon's release on the package index.
    PackageIndex,
    /// The series tree of a repository of an indexed organization.
    Forge,
}

impl Origin {
    /// How the origin is written: `override`, `cache`, `package-index`,
    /// `forge`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Cache => "cache",
            Self::PackageIndex => "package-index",
            Self::Forge => "forge",
        }
    }
}

/// Where an addon lives on a series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// The repository URL.
    pub url: String,
    /// The addon's directory in the repository.
    pub path: String,
    /// The source that said so.
    pub origin: Origin,
}

/// The index of one series, as cached in `<cache>/index/<series>.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesIndex {
    /// Format version.
    pub schema: u32,
    /// The series, e.g. `18.0`.
    pub series: String,
    /// Seconds since the epoch the package index was last asked.
    pub fetched_at: u64,
    /// Located addons.
    pub addons: BTreeMap<String, Location>,
    /// Addons the package index did not know for this series.
    #[serde(default)]
    pub unknown: BTreeSet<String>,
    /// Whether the organizations' listings were read into `addons`.
    #[serde(default)]
    pub listed: bool,
}

impl SeriesIndex {
    /// The cache file of `series` under `cache`.
    #[must_use]
    pub fn file(cache: &Path, series: &str) -> PathBuf {
        cache.join("index").join(format!("{series}.json"))
    }

    /// The cached index of `series`, if there is a readable one.
    #[must_use]
    pub fn load(cache: &Path, series: &str) -> Option<Self> {
        std::fs::read(Self::file(cache, series))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    /// Write it to the cache through a temporary file.
    ///
    /// # Errors
    ///
    /// When the file cannot be written.
    pub fn save(&self, cache: &Path) -> std::io::Result<()> {
        let file = Self::file(cache, &self.series);
        let temporary = file.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).unwrap_or_default();
        file.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&temporary, text))
            .and_then(|()| std::fs::rename(&temporary, &file))
    }
}

/// `"17.0"` as numbers, for ordering series; unknown parts count as zero.
///
/// ```
/// assert!(odx_index::series_key("9.0") < odx_index::series_key("17.0"));
/// ```
#[must_use]
pub fn series_key(series: &str) -> (u32, u32) {
    let mut parts = series.split('.').map(|part| part.parse().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Renames and merges, by the series in which they took effect (the
/// series migrated to): old name → new name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renames {
    /// Renamed addons.
    pub renamed: BTreeMap<String, BTreeMap<String, String>>,
    /// Addons merged into another.
    pub merged: BTreeMap<String, BTreeMap<String, String>>,
}

impl Renames {
    /// The older names of `addon` on `series`: addons renamed or merged
    /// into it on `series` or before, transitively, sorted.
    #[must_use]
    pub fn aliases(&self, addon: &str, series: &str) -> Vec<String> {
        let limit = series_key(series);
        let edges: Vec<(&String, &String)> = self
            .renamed
            .iter()
            .chain(&self.merged)
            .filter(|(effective, _)| series_key(effective) <= limit)
            .flat_map(|(_, table)| table)
            .collect();
        let mut names: BTreeSet<String> = BTreeSet::from([addon.to_owned()]);
        loop {
            let found: Vec<String> = edges
                .iter()
                .filter(|(old, new)| names.contains(*new) && !names.contains(*old))
                .map(|(old, _)| (*old).clone())
                .collect();
            if found.is_empty() {
                break;
            }
            names.extend(found);
        }
        names.remove(addon);
        names.into_iter().collect()
    }
}

/// The addon index: series indexes and the renames between them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddonIndex {
    /// Indexes by series.
    pub series: BTreeMap<String, SeriesIndex>,
    /// Renames and merges.
    pub renames: Renames,
}

impl AddonIndex {
    /// Where `addon` lives on `series`, under that name.
    #[must_use]
    pub fn locate(&self, addon: &str, series: &str) -> Option<&Location> {
        self.series
            .get(series)
            .and_then(|index| index.addons.get(addon))
    }

    /// Where `addon` (named as on the newest indexed series) lived on every
    /// indexed series, oldest first, under its name there or an older one.
    #[must_use]
    pub fn history(&self, addon: &str) -> Vec<(String, String, Location)> {
        let mut series: Vec<&String> = self.series.keys().collect();
        series.sort_by_key(|series| series_key(series));
        let newest = series
            .last()
            .map(|series| series.as_str())
            .unwrap_or_default();
        let names: Vec<String> = std::iter::once(addon.to_owned())
            .chain(self.renames.aliases(addon, newest))
            .collect();
        series
            .into_iter()
            .filter_map(|series| {
                names.iter().find_map(|name| {
                    self.locate(name, series)
                        .map(|location| (series.clone(), name.clone(), location.clone()))
                })
            })
            .collect()
    }

    /// The older names of `addon` on `series`.
    #[must_use]
    pub fn aliases(&self, addon: &str, series: &str) -> Vec<String> {
        self.renames.aliases(addon, series)
    }
}

/// What [`build`] reads.
pub struct Build<'a> {
    /// `[tool.odx.index.overrides]`: addon → series → `owner/repo` or URL.
    pub overrides: &'a BTreeMap<String, BTreeMap<String, String>>,
    /// Cached repositories: URL, bare repository, forge.
    pub cached: &'a [(String, PathBuf, Forge)],
    /// Git, to list the addons of cached series branches.
    pub merger: &'a Merger,
    /// The package index and its JSON API base URL; `None` offline.
    pub package_index: Option<(&'a dyn Http, &'a str)>,
    /// The indexed organizations' addons on a series, as (repository URL,
    /// addon), with what failed; `None` offline or when not configured.
    pub listing: Option<Listing<'a>>,
}

/// Lists the indexed organizations' addons on a series.
pub type Listing<'a> = &'a dyn Fn(&str) -> (Vec<(String, String)>, Vec<String>);

impl std::fmt::Debug for Build<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Build")
            .field("cached", &self.cached)
            .finish_non_exhaustive()
    }
}

/// The index of `series` with `wanted` addons looked up.
///
/// Overrides come first, then the cache's series branches, then what
/// `previous` learnt from the package index, then the package index for
/// addons still missing (and not known to be missing), then, when some
/// are still missing, the organizations' listings, once per series.
/// Lookups that fail are reported, not remembered.
pub fn build(
    spec: &Build<'_>,
    series: &str,
    wanted: &[String],
    previous: Option<SeriesIndex>,
    now: u64,
) -> (SeriesIndex, Vec<String>) {
    let previous = previous.unwrap_or_default();
    let mut index = SeriesIndex {
        schema: 1,
        series: series.to_owned(),
        fetched_at: previous.fetched_at,
        addons: sources::overrides(spec.overrides, series),
        unknown: previous.unknown,
        listed: previous.listed,
    };
    for (addon, location) in sources::cached(spec.merger, spec.cached, series) {
        index.addons.entry(addon).or_insert(location);
    }
    for (addon, location) in previous.addons {
        if matches!(location.origin, Origin::PackageIndex | Origin::Forge) {
            index.addons.entry(addon).or_insert(location);
        }
    }
    let mut problems = Vec::new();
    if let Some((http, base)) = spec.package_index {
        package_index(&mut index, http, base, wanted, now, &mut problems);
    }
    let missing = wanted.iter().any(|addon| !index.addons.contains_key(addon));
    if let Some(listing) = spec.listing.filter(|_| missing && !index.listed) {
        index.fetched_at = now;
        let (found, failures) = listing(series);
        index.listed = failures.is_empty();
        problems.extend(failures);
        for (url, addon) in found {
            index.unknown.remove(&addon);
            index.addons.entry(addon.clone()).or_insert(Location {
                url,
                path: addon,
                origin: Origin::Forge,
            });
        }
    }
    (index, problems)
}

/// Look up on the package index the `wanted` addons `index` misses.
fn package_index(
    index: &mut SeriesIndex,
    http: &dyn Http,
    base: &str,
    wanted: &[String],
    now: u64,
    problems: &mut Vec<String>,
) {
    let series = index.series.clone();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|addon| !index.addons.contains_key(*addon) && !index.unknown.contains(*addon))
        .collect();
    for addon in missing {
        index.fetched_at = now;
        match sources::package_index(http, base, addon, &series) {
            Ok(Some(url)) => {
                index.addons.insert(
                    addon.clone(),
                    Location {
                        url,
                        path: addon.clone(),
                        origin: Origin::PackageIndex,
                    },
                );
            }
            Ok(None) => {
                index.unknown.insert(addon.clone());
            }
            Err(e) => problems.push(format!("{addon} on {series}: {e}")),
        }
    }
}

/// Test doubles.
#[cfg(test)]
pub(crate) mod testing {
    use odx_forge::{ForgeError, Http, HttpResponse};
    use serde_json::Value as Json;
    use std::sync::Mutex;

    /// Answers from a list of (URL, status, body); others are 404.
    #[derive(Debug, Default)]
    pub(crate) struct Fake {
        routes: Vec<(String, u16, String)>,
        pub(crate) asked: Mutex<Vec<String>>,
    }

    impl Fake {
        pub(crate) fn new(routes: &[(&str, u16, &str)]) -> Self {
            Self {
                routes: routes
                    .iter()
                    .map(|(url, status, body)| ((*url).to_owned(), *status, (*body).to_owned()))
                    .collect(),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl Http for Fake {
        fn get(&self, url: &str, _: &[(&str, String)]) -> Result<HttpResponse, ForgeError> {
            self.asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(url.to_owned());
            if url.contains("broken") {
                return Err(ForgeError::Http("refused".to_owned()));
            }
            let (status, body) = self
                .routes
                .iter()
                .find(|(route, _, _)| route == url)
                .map_or((404, String::new()), |(_, status, body)| {
                    (*status, body.clone())
                });
            Ok(HttpResponse {
                status,
                headers: Vec::new(),
                body: body.into_bytes(),
            })
        }

        fn post(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _: &Json,
        ) -> Result<HttpResponse, ForgeError> {
            self.get(url, headers)
        }

        #[cfg_attr(coverage_nightly, coverage(off))]
        fn put(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _: &Json,
        ) -> Result<HttpResponse, ForgeError> {
            self.get(url, headers)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(url: &str) -> Location {
        Location {
            url: url.to_owned(),
            path: "x".to_owned(),
            origin: Origin::Cache,
        }
    }

    fn renames() -> Renames {
        Renames {
            renamed: BTreeMap::from([
                (
                    "17.0".to_owned(),
                    BTreeMap::from([("a".to_owned(), "b".to_owned())]),
                ),
                (
                    "18.0".to_owned(),
                    BTreeMap::from([("b".to_owned(), "c".to_owned())]),
                ),
            ]),
            merged: BTreeMap::from([(
                "18.0".to_owned(),
                BTreeMap::from([("m".to_owned(), "c".to_owned())]),
            )]),
        }
    }

    #[test]
    fn aliases_follow_renames_and_merges_up_to_the_series() {
        let renames = renames();
        assert_eq!(renames.aliases("c", "18.0"), ["a", "b", "m"]);
        assert_eq!(renames.aliases("c", "17.0"), Vec::<String>::new());
        assert_eq!(renames.aliases("b", "17.0"), ["a"]);
        assert_eq!(renames.aliases("z", "19.0"), Vec::<String>::new());
    }

    #[test]
    fn locate_and_history() {
        let series = |name: &str, addon: &str, url: &str| SeriesIndex {
            schema: 1,
            series: name.to_owned(),
            fetched_at: 0,
            addons: BTreeMap::from([(addon.to_owned(), location(url))]),
            unknown: BTreeSet::new(),
            listed: false,
        };
        let index = AddonIndex {
            series: BTreeMap::from([
                ("16.0".to_owned(), series("16.0", "a", "https://x/old")),
                ("9.0".to_owned(), series("9.0", "zz", "https://x/none")),
                ("18.0".to_owned(), series("18.0", "c", "https://x/new")),
            ]),
            renames: renames(),
        };
        assert_eq!(index.locate("c", "18.0").unwrap().url, "https://x/new");
        assert_eq!(index.locate("c", "16.0"), None);
        assert_eq!(index.locate("c", "15.0"), None);
        assert_eq!(
            index
                .history("c")
                .into_iter()
                .map(|(series, name, location)| format!("{series} {name} {}", location.url))
                .collect::<Vec<_>>(),
            ["16.0 a https://x/old", "18.0 c https://x/new"]
        );
        assert_eq!(index.aliases("c", "18.0"), ["a", "b", "m"]);
        assert_eq!(AddonIndex::default().history("c"), Vec::new());
        assert_eq!(series_key("x"), (0, 0));
        assert_eq!(
            [
                Origin::Override,
                Origin::Cache,
                Origin::PackageIndex,
                Origin::Forge
            ]
            .map(Origin::name),
            ["override", "cache", "package-index", "forge"]
        );
    }

    #[test]
    fn builds_from_overrides_cache_and_package_index() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "odx")
                .env("GIT_AUTHOR_EMAIL", "odx@example.com")
                .env("GIT_COMMITTER_NAME", "odx")
                .env("GIT_COMMITTER_EMAIL", "odx@example.com")
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        let up = tmp.path().join("up");
        for addon in ["pos_a", "pos_b"] {
            std::fs::create_dir_all(up.join(addon)).unwrap();
            std::fs::write(up.join(addon).join("__manifest__.py"), "{}").unwrap();
        }
        git(&up, &["init", "--quiet", "--initial-branch=18.0"]);
        git(&up, &["add", "-A"]);
        git(&up, &["commit", "--quiet", "-m", "addons"]);
        let cache = tmp.path().join("cache.git");
        git(
            tmp.path(),
            &["init", "--quiet", "--bare", cache.to_str().unwrap()],
        );
        git(
            &cache,
            &[
                "fetch",
                "--quiet",
                up.to_str().unwrap(),
                "18.0:refs/odx/heads/18.0",
            ],
        );
        let cached = vec![
            (
                "https://x/missing".to_owned(),
                tmp.path().join("nope"),
                Forge::Github,
            ),
            ("https://x/pos".to_owned(), cache, Forge::Github),
        ];
        let overrides = BTreeMap::from([(
            "pos_b".to_owned(),
            BTreeMap::from([("18.0".to_owned(), "OCA/b".to_owned())]),
        )]);
        let merger = Merger::new(std::sync::Arc::new(odx_core::system::OsRunner));
        let fake = testing::Fake::new(&[
            (
                "https://pypi.test/odoo-addon-web-x/json",
                200,
                r#"{"releases":{"18.0.1.0.0":[]}}"#,
            ),
            (
                "https://pypi.test/odoo-addon-web-x/18.0.1.0.0/json",
                200,
                r#"{"info":{"home_page":"https://github.com/OCA/web"}}"#,
            ),
            ("https://pypi.test/odoo-addon-down/json", 500, ""),
        ]);
        let spec = Build {
            overrides: &overrides,
            cached: &cached,
            merger: &merger,
            package_index: Some((&fake, "https://pypi.test")),
            listing: None,
        };
        let wanted: Vec<String> = ["pos_a", "web_x", "gone", "down"]
            .map(str::to_owned)
            .to_vec();
        let (index, problems) = build(&spec, "18.0", &wanted, None, 7);
        assert_eq!(index.addons["pos_a"].origin, Origin::Cache);
        assert_eq!(index.addons["pos_b"].url, "https://github.com/OCA/b");
        assert_eq!(index.addons["web_x"].url, "https://github.com/OCA/web");
        assert_eq!(index.unknown, BTreeSet::from(["gone".to_owned()]));
        assert_eq!(index.fetched_at, 7);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("down on 18.0: "), "{problems:?}");
        assert!(format!("{spec:?}").starts_with("Build"));

        // Known answers are not asked again; offline, nothing is asked.
        let asked = fake.asked.lock().unwrap().len();
        let (again, _) = build(&spec, "18.0", &wanted[..3], Some(index), 9);
        assert_eq!(fake.asked.lock().unwrap().len(), asked);
        assert_eq!((again.fetched_at, again.addons.len()), (7, 3));
        let offline = Build {
            package_index: None,
            ..spec
        };
        let (cached_only, _) = build(&offline, "18.0", &wanted, None, 9);
        assert_eq!(cached_only.addons.len(), 2);
    }

    #[test]
    fn listings_fill_what_is_still_missing_once() {
        let calls = std::sync::Mutex::new(Vec::new());
        let listing = |series: &str| {
            calls.lock().unwrap().push(series.to_owned());
            (
                vec![
                    ("https://github.com/OCA/web".to_owned(), "web_x".to_owned()),
                    ("https://github.com/OCA/b".to_owned(), "pos_b".to_owned()),
                ],
                Vec::new(),
            )
        };
        let overrides = BTreeMap::from([(
            "pos_b".to_owned(),
            BTreeMap::from([("18.0".to_owned(), "OCA/pos".to_owned())]),
        )]);
        let merger = Merger::new(std::sync::Arc::new(odx_core::system::OsRunner));
        let spec = Build {
            overrides: &overrides,
            cached: &[],
            merger: &merger,
            package_index: None,
            listing: Some(&listing),
        };
        let previous = SeriesIndex {
            unknown: BTreeSet::from(["web_x".to_owned()]),
            ..SeriesIndex::default()
        };
        let wanted = vec!["web_x".to_owned(), "pos_b".to_owned()];
        let (index, problems) = build(&spec, "18.0", &wanted, Some(previous), 3);
        assert_eq!(index.addons["web_x"].origin, Origin::Forge);
        assert_eq!(index.addons["pos_b"].origin, Origin::Override);
        assert_eq!((index.listed, index.fetched_at), (true, 3));
        assert!(index.unknown.is_empty() && problems.is_empty());
        // Found addons are kept, and the listing is not read again.
        let (again, _) = build(&spec, "18.0", &wanted, Some(index), 4);
        assert_eq!(again.addons["web_x"].origin, Origin::Forge);
        let missing = ["gone".to_owned()];
        let (again, _) = build(&spec, "18.0", &missing, Some(again), 5);
        assert_eq!(again.fetched_at, 3);
        assert_eq!(*calls.lock().unwrap(), ["18.0"]);

        // Failures are reported and the listing is read again next time.
        let failing = |_: &str| (Vec::new(), vec!["OCA: down".to_owned()]);
        let spec = Build {
            listing: Some(&failing),
            ..spec
        };
        let (index, problems) = build(&spec, "18.0", &missing, None, 6);
        assert_eq!(
            (index.listed, problems),
            (false, vec!["OCA: down".to_owned()])
        );
    }

    #[test]
    fn series_indexes_are_cached() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(SeriesIndex::load(tmp.path(), "18.0"), None);
        let index = SeriesIndex {
            schema: 1,
            series: "18.0".to_owned(),
            fetched_at: 5,
            addons: BTreeMap::from([("a".to_owned(), location("u"))]),
            unknown: BTreeSet::from(["b".to_owned()]),
            listed: true,
        };
        index.save(tmp.path()).unwrap();
        assert_eq!(SeriesIndex::load(tmp.path(), "18.0"), Some(index.clone()));
        let blocked = tmp.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        assert!(index.save(&blocked).is_err());
    }
}
