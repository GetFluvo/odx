//! The sources of the index: configured overrides, the local object cache,
//! the package index, and OpenUpgrade's rename tables.

use std::collections::BTreeMap;
use std::path::PathBuf;

use odx_analyze::manifest::{Value, assignment};
use odx_forge::Http;
use odx_git::{Forge, Merger, Source};
use serde_json::Value as Json;

use crate::{Location, Origin, series_key};

/// The repository URL an override names: a URL as written, or
/// `owner/repo` on GitHub.
fn override_url(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with("git@") {
        repo.to_owned()
    } else {
        format!("https://github.com/{repo}")
    }
}

/// The overrides for `series`.
#[must_use]
pub fn overrides(
    overrides: &BTreeMap<String, BTreeMap<String, String>>,
    series: &str,
) -> BTreeMap<String, Location> {
    overrides
        .iter()
        .filter_map(|(addon, per_series)| {
            per_series.get(series).map(|repo| {
                (
                    addon.clone(),
                    Location {
                        url: override_url(repo),
                        path: addon.clone(),
                        origin: Origin::Override,
                    },
                )
            })
        })
        .collect()
}

/// The addons on the `series` branch of every cached repository that has
/// it; the first repository listing an addon wins.
#[must_use]
pub fn cached(
    merger: &Merger,
    repos: &[(String, PathBuf, Forge)],
    series: &str,
) -> BTreeMap<String, Location> {
    let mut found = BTreeMap::new();
    for (url, dir, forge) in repos {
        let branch = Source::Branch(series.to_owned()).local_ref(*forge, None);
        for addon in merger.addons(dir, &branch).unwrap_or_default() {
            found.entry(addon.clone()).or_insert_with(|| Location {
                url: url.clone(),
                path: addon,
                origin: Origin::Cache,
            });
        }
    }
    found
}

/// The distribution of `addon` on `series`: `odoo-addon-<name>` since
/// 15.0, `odoo<major>-addon-<name>` before.
///
/// ```
/// use odx_index::sources::distribution;
///
/// assert_eq!(distribution("pos_x", "17.0"), "odoo-addon-pos-x");
/// assert_eq!(distribution("pos_x", "14.0"), "odoo14-addon-pos-x");
/// ```
#[must_use]
pub fn distribution(addon: &str, series: &str) -> String {
    let name = addon.replace('_', "-");
    match series_key(series).0 {
        major if major >= 15 => format!("odoo-addon-{name}"),
        major => format!("odoo{major}-addon-{name}"),
    }
}

/// The JSON at `url`; `None` for a 404.
fn json(http: &dyn Http, url: &str) -> Result<Option<Json>, String> {
    let response = http.get(url, &[]).map_err(|e| e.to_string())?;
    match response.status {
        200 => serde_json::from_slice(&response.body)
            .map(Some)
            .map_err(|e| format!("{url}: {e}")),
        404 => Ok(None),
        status => Err(format!("{url} answered HTTP {status}")),
    }
}

/// A version as numbers, for picking the newest release.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// A repository URL from release metadata, without a trailing slash or a
/// `/tree/<branch>/...` suffix.
fn repository(url: &str) -> String {
    let url = url.trim_end_matches('/');
    url.split_once("/tree/")
        .map_or(url, |(repo, _)| repo)
        .to_owned()
}

/// The repository of `addon`'s newest release for `series`.
///
/// Read from the package index at `base` (a PyPI JSON API such as
/// `https://pypi.org/pypi`): the release's home page or first project URL;
/// `None` when there is no release for the series.
///
/// # Errors
///
/// When the index cannot be read or answers unexpectedly.
pub fn package_index(
    http: &dyn Http,
    base: &str,
    addon: &str,
    series: &str,
) -> Result<Option<String>, String> {
    let base = base.trim_end_matches('/');
    let dist = distribution(addon, series);
    let Some(project) = json(http, &format!("{base}/{dist}/json"))? else {
        return Ok(None);
    };
    let prefix = format!("{series}.");
    let newest = project
        .get("releases")
        .and_then(Json::as_object)
        .and_then(|releases| {
            releases
                .keys()
                .filter(|version| version.starts_with(&prefix))
                .max_by_key(|version| version_key(version))
        });
    let Some(version) = newest else {
        return Ok(None);
    };
    let release = json(http, &format!("{base}/{dist}/{version}/json"))?.unwrap_or_default();
    let info = release.get("info");
    let home = info
        .and_then(|info| info.get("home_page"))
        .and_then(Json::as_str)
        .filter(|url| !url.is_empty());
    let project_url = info
        .and_then(|info| info.get("project_urls"))
        .and_then(Json::as_object)
        .and_then(|urls| urls.values().find_map(Json::as_str));
    Ok(home.or(project_url).map(repository))
}

/// Old name → new name.
pub type Table = BTreeMap<String, String>;

/// A Python dict of strings as a map.
fn table(value: Option<Value>) -> BTreeMap<String, String> {
    match value {
        Some(Value::Dict(items)) => items
            .into_iter()
            .filter_map(|(key, value)| match (key, value) {
                (Value::Str(key), Value::Str(value)) => Some((key, value)),
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    }
}

/// The addons renamed and merged when migrating to `series`.
///
/// Read from OpenUpgrade's `apriori.py` on its `series` branch under `base`
/// (raw file access, such as
/// `https://raw.githubusercontent.com/OCA/OpenUpgrade`). A series
/// OpenUpgrade does not have has none.
///
/// # Errors
///
/// When the file cannot be read.
pub fn openupgrade(http: &dyn Http, base: &str, series: &str) -> Result<(Table, Table), String> {
    let url = format!(
        "{}/{series}/openupgrade_scripts/apriori.py",
        base.trim_end_matches('/')
    );
    let response = http.get(&url, &[]).map_err(|e| e.to_string())?;
    match response.status {
        200 => {
            let text = String::from_utf8_lossy(&response.body);
            Ok((
                table(assignment(&text, "renamed_modules")),
                table(assignment(&text, "merged_modules")),
            ))
        }
        404 => Ok((BTreeMap::new(), BTreeMap::new())),
        status => Err(format!("{url} answered HTTP {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fake;

    const BASE: &str = "https://pypi.test/pypi/";

    #[test]
    fn overrides_name_repositories() {
        let config = BTreeMap::from([
            (
                "a".to_owned(),
                BTreeMap::from([("18.0".to_owned(), "OCA/server-ux".to_owned())]),
            ),
            (
                "b".to_owned(),
                BTreeMap::from([("18.0".to_owned(), "https://gitlab.com/x/y".to_owned())]),
            ),
            (
                "c".to_owned(),
                BTreeMap::from([("17.0".to_owned(), "git@h:x/c".to_owned())]),
            ),
        ]);
        let found = overrides(&config, "18.0");
        assert_eq!(found["a"].url, "https://github.com/OCA/server-ux");
        assert_eq!(found["b"].url, "https://gitlab.com/x/y");
        assert!(!found.contains_key("c"));
        assert_eq!(overrides(&config, "17.0")["c"].url, "git@h:x/c");
    }

    #[test]
    fn releases_name_their_repository() {
        let fake = Fake::new(&[
            (
                "https://pypi.test/pypi/odoo-addon-pos-x/json",
                200,
                r#"{"releases":{"17.0.1.0.0":[],"18.0.1.0.9":[],"18.0.1.0.10":[],"19.0.1.0.0":[]}}"#,
            ),
            (
                "https://pypi.test/pypi/odoo-addon-pos-x/18.0.1.0.10/json",
                200,
                r#"{"info":{"home_page":"https://github.com/OCA/pos/tree/18.0/pos_x/","project_urls":{"Homepage":"x"}}}"#,
            ),
            (
                "https://pypi.test/pypi/odoo-addon-pos-x/17.0.1.0.0/json",
                200,
                r#"{"info":{"home_page":"","project_urls":{"Source":"https://github.com/OCA/old"}}}"#,
            ),
            (
                "https://pypi.test/pypi/odoo-addon-pos-x/19.0.1.0.0/json",
                200,
                r#"{"info":{}}"#,
            ),
            (
                "https://pypi.test/pypi/odoo-addon-bad/json",
                200,
                "not json",
            ),
            ("https://pypi.test/pypi/odoo-addon-down/json", 503, ""),
            (
                "https://pypi.test/pypi/odoo-addon-half/json",
                200,
                r#"{"releases":{"18.0.1.0.0":[]}}"#,
            ),
            (
                "https://pypi.test/pypi/odoo-addon-half/18.0.1.0.0/json",
                502,
                "",
            ),
        ]);
        let found = |addon: &str, series: &str| package_index(&fake, BASE, addon, series);
        assert_eq!(
            found("pos_x", "18.0").unwrap().as_deref(),
            Some("https://github.com/OCA/pos")
        );
        assert_eq!(
            found("pos_x", "17.0").unwrap().as_deref(),
            Some("https://github.com/OCA/old")
        );
        assert_eq!(found("pos_x", "19.0").unwrap(), None);
        assert_eq!(found("pos_x", "16.0").unwrap(), None);
        assert_eq!(found("gone", "18.0").unwrap(), None);
        assert!(
            found("bad", "18.0")
                .unwrap_err()
                .contains("odoo-addon-bad/json")
        );
        assert!(found("half", "18.0").unwrap_err().ends_with("HTTP 502"));
        assert!(
            found("down", "18.0")
                .unwrap_err()
                .ends_with("answered HTTP 503")
        );
        assert_eq!(
            package_index(&fake, "https://broken.test", "a", "18.0").unwrap_err(),
            "request failed: refused"
        );
        let empty = Fake::new(&[(
            "https://pypi.test/pypi/odoo-addon-e/json",
            200,
            r#"{"info":{}}"#,
        )]);
        assert_eq!(package_index(&empty, BASE, "e", "18.0").unwrap(), None);
    }

    #[test]
    fn openupgrade_rename_tables() {
        let apriori = "\"\"\"Apriori\"\"\"\nrenamed_modules = {\n    # OCA/x\n    \"old\": \"new\",\n    \"odd\": None,\n}\nmerged_modules = {\"a\": \"b\"}\n";
        let fake = Fake::new(&[
            (
                "https://raw.test/OpenUpgrade/18.0/openupgrade_scripts/apriori.py",
                200,
                apriori,
            ),
            (
                "https://raw.test/OpenUpgrade/17.0/openupgrade_scripts/apriori.py",
                200,
                "renamed_modules = []\n",
            ),
            (
                "https://raw.test/OpenUpgrade/16.0/openupgrade_scripts/apriori.py",
                500,
                "",
            ),
        ]);
        let base = "https://raw.test/OpenUpgrade/";
        let (renamed, merged) = openupgrade(&fake, base, "18.0").unwrap();
        assert_eq!(
            renamed,
            BTreeMap::from([("old".to_owned(), "new".to_owned())])
        );
        assert_eq!(merged, BTreeMap::from([("a".to_owned(), "b".to_owned())]));
        let (renamed, merged) = openupgrade(&fake, base, "17.0").unwrap();
        assert!(renamed.is_empty() && merged.is_empty());
        assert_eq!(openupgrade(&fake, base, "15.0").unwrap().0.len(), 0);
        assert!(
            openupgrade(&fake, base, "16.0")
                .unwrap_err()
                .ends_with("HTTP 500")
        );
        assert!(openupgrade(&fake, "https://broken.test", "18.0").is_err());
        assert_eq!(fake.post("x", &[], &Json::Null).unwrap().status, 404);
    }
}
