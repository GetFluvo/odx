//! `odx glue`: addons Odoo would install on its own (`auto_install`) with
//! what the project installs, that the project does not have. A project
//! that names its addons one by one misses them: nothing depends on them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use odx_analyze::core::core_addons;
use odx_analyze::glue::{Candidate, Offered, candidates, installed};
use odx_analyze::manifest::parse_manifest;
use odx_config::Problem;
use odx_config::edit::PyprojectEdit;
use odx_core::system::OsRunner;
use odx_core::{Exit, repo_key};
use odx_forge::RepoUrl;
use odx_git::{LocalClone, Merger};
use odx_index::sources::distribution;
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::{Prepared, prepare};
use super::check_deps::{addon_of, inventory};
use crate::Context;
use crate::cli::{GlobalArgs, GlueArgs};

/// A repository to look for glue in: its URL, git directory and the ref of
/// the series branch there.
struct Scanned {
    url: String,
    git_dir: PathBuf,
    rev: String,
}

/// The repositories of the series: the cache's, then the local clones' of
/// `[clones]` that the cache does not have, each once.
fn repositories(prepared: &Prepared, series: &str) -> Vec<Scanned> {
    let repos = prepared.cache.root().join("repos");
    let mut urls: Vec<(String, PathBuf)> = std::fs::read_dir(&repos)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let url = std::fs::read_to_string(path.with_extension("url")).ok()?;
            path.is_dir().then(|| (url.trim().to_owned(), path))
        })
        .collect();
    urls.sort();
    let mut seen = BTreeSet::new();
    let mut scanned = Vec::new();
    for (url, git_dir) in urls {
        if seen.insert(repo_key(&url)) {
            scanned.push(Scanned {
                url,
                git_dir,
                rev: format!("refs/odx/heads/{series}"),
            });
        }
    }
    // One remote per clone: `origin` when it has one.
    let mut clones: Vec<&LocalClone> = Vec::new();
    for clone in prepared.clones().iter() {
        match clones
            .iter_mut()
            .find(|known| known.git_dir == clone.git_dir)
        {
            Some(known) if clone.remote == "origin" => *known = clone,
            Some(_) => {}
            None => clones.push(clone),
        }
    }
    for clone in clones {
        if seen.insert(repo_key(&clone.url)) {
            scanned.push(Scanned {
                url: clone.url.clone(),
                git_dir: clone.git_dir.clone(),
                rev: format!("refs/remotes/{}/{series}", clone.remote),
            });
        }
    }
    scanned
}

/// Odoo's repositories, whose addons live under `addons/`.
const ODOO: [&str; 2] = ["https://github.com/odoo/odoo", "https://github.com/OCA/OCB"];

/// What the scan found: the `depends` of every addon seen, and the addons
/// with `auto_install`.
#[derive(Default)]
struct Catalogue {
    depends: BTreeMap<String, Vec<String>>,
    offered: Vec<Offered>,
}

impl Catalogue {
    /// Add the addons of `repo`; a repository without the series branch
    /// has none. Whether it has the branch.
    fn add(&mut self, merger: &Merger, repo: &Scanned, prefix: Option<&str>) -> bool {
        let Ok(manifests) = merger.manifests(&repo.git_dir, &repo.rev, prefix) else {
            return false;
        };
        for (path, text) in manifests {
            let Ok(manifest) = parse_manifest(&text) else {
                continue;
            };
            let addon = path.rsplit('/').next().unwrap_or(&path).to_owned();
            if !manifest.installable {
                continue;
            }
            if let Some(triggers) = manifest.auto_install.clone() {
                self.offered.push(Offered {
                    addon: addon.clone(),
                    depends: manifest.depends.clone(),
                    triggers,
                    url: repo.url.clone(),
                    path: path.clone(),
                });
            }
            self.depends.entry(addon).or_insert(manifest.depends);
        }
        true
    }
}

/// What `odx add` takes to add `candidate` as a package from its git
/// repository: the spec (the series branch), the distribution and the
/// subdirectory.
pub(crate) fn add_spec(candidate: &Candidate, series: &str) -> (String, String, String) {
    let offered = &candidate.offered;
    let repo = RepoUrl::parse(&offered.url)
        .filter(|repo| repo.host == "github.com")
        .map_or_else(|| offered.url.clone(), |repo| repo.path);
    (
        format!("{repo}@{series}"),
        distribution(&offered.addon, series),
        offered.path.clone(),
    )
}

/// How to add `candidate`.
fn add_command(candidate: &Candidate, series: &str) -> String {
    let (spec, dist, path) = add_spec(candidate, series);
    format!("odx add {spec} --package {dist} --subdirectory {path}")
}

/// `odx dashboard act glue:<addon> --mute`: the addon goes into
/// `[tool.odx.glue] ignore` with `reason`, which is required.
pub(crate) fn ignore(
    prepared: &Prepared,
    addon: &str,
    reason: Option<&str>,
) -> Result<Report, Report> {
    let reason =
        reason.ok_or_else(|| Report::error(Exit::Usage, "--mute needs --reason".to_owned()))?;
    let file = &prepared.project.file;
    PyprojectEdit::parse(&prepared.project.text)
        .and_then(|mut edit| edit.ignore_glue(addon, reason).map(|()| edit.to_string()))
        .as_ref()
        .map_err(ToString::to_string)
        .and_then(|text| {
            std::fs::write(file, text)
                .as_ref()
                .map_err(ToString::to_string)
                .copied()
        })
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
    Ok(Report {
        out: format!("{addon} is ignored in {}\n", file.display()),
        ..Report::default()
    })
}

/// The glue addons the project misses, the ignored ones with why, and how
/// many repositories were scanned.
pub(crate) struct Glue {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) ignored: Vec<(String, String)>,
    pub(crate) scanned: usize,
}

/// Look for glue addons in the cache and the local clones.
pub(crate) fn glue(prepared: &Prepared) -> Glue {
    let config = &prepared.project.config;
    let series = &config.odoo_version;
    let core: BTreeSet<&str> = core_addons(series)
        .unwrap_or_default()
        .into_iter()
        .collect();
    let inventory = inventory(prepared);
    let merger = Merger::new(Arc::new(OsRunner));
    let mut catalogue = Catalogue::default();
    for (addon, depends) in inventory.depends() {
        catalogue.depends.insert(addon.to_owned(), depends.to_vec());
    }
    let repositories = repositories(prepared, series);
    // Odoo's own addons, from the locked revision when Odoo is a git
    // source, else from a clone of Odoo or OCB on the series: their
    // dependencies count too.
    let locked = prepared.lock.as_ref().and_then(|lock| lock.odoo.as_ref());
    let odoo = locked
        .map(|odoo| Scanned {
            url: odoo.url.clone(),
            git_dir: prepared.cache.repo_dir(&odoo.url),
            rev: odoo.rev.clone(),
        })
        .into_iter()
        .chain(repositories.iter().filter_map(|repo| {
            let key = repo_key(&repo.url);
            ODOO.iter()
                .any(|url| repo_key(url) == key)
                .then(|| Scanned {
                    url: repo.url.clone(),
                    git_dir: repo.git_dir.clone(),
                    rev: repo.rev.clone(),
                })
        }));
    for repo in odoo.take(1) {
        catalogue.add(&merger, &repo, Some("addons"));
        catalogue.add(&merger, &repo, Some("odoo/addons"));
    }
    let mut scanned = 0;
    for repo in &repositories {
        scanned += usize::from(catalogue.add(&merger, repo, None));
    }
    // Packages of [tool.odx.packages] count as added, also before they are
    // declared as a requirement.
    let configured: Vec<String> = config.packages.keys().filter_map(|d| addon_of(d)).collect();
    let installed = installed(
        inventory
            .installs()
            .chain(configured.iter().map(String::as_str)),
        &catalogue.depends,
    );
    let present: BTreeSet<String> = inventory
        .sources
        .keys()
        .chain(&configured)
        .cloned()
        .collect();
    let ignore = config
        .glue
        .as_ref()
        .map(|glue| glue.ignore.clone())
        .unwrap_or_default();
    let (ignored, candidates): (Vec<Candidate>, Vec<Candidate>) =
        candidates(&catalogue.offered, &installed, &present, &core)
            .into_iter()
            .partition(|candidate| ignore.contains_key(&candidate.offered.addon));
    Glue {
        candidates,
        ignored: ignored
            .into_iter()
            .map(|candidate| {
                let why = ignore
                    .get(&candidate.offered.addon)
                    .cloned()
                    .unwrap_or_default();
                (candidate.offered.addon, why)
            })
            .collect(),
        scanned,
    }
}

/// The warning `odx lock` gives when glue addons are missing.
pub(crate) fn hint(prepared: &Prepared) -> Option<Problem> {
    let glue = glue(prepared);
    let names: Vec<&str> = glue
        .candidates
        .iter()
        .map(|candidate| candidate.offered.addon.as_str())
        .collect();
    (!names.is_empty()).then(|| Problem {
        key: "glue".to_owned(),
        message: format!(
            "Odoo would install {} with what the project has, but the project does not have them; see `odx glue`",
            names.join(", ")
        ),
    })
}

fn text(glue: &Glue, series: &str) -> String {
    let mut out = String::new();
    if glue.candidates.is_empty() {
        let _ = writeln!(
            out,
            "no glue addons missing ({} repositories with {series} scanned)",
            glue.scanned
        );
    } else {
        let _ = writeln!(
            out,
            "glue addons Odoo would install with what the project has ({series}):"
        );
    }
    for candidate in &glue.candidates {
        let offered = &candidate.offered;
        let _ = writeln!(
            out,
            "  {} ({}, with {})\n    {}",
            offered.addon,
            offered.url,
            offered.triggers.join(", "),
            add_command(candidate, series)
        );
        if !candidate.missing.is_empty() {
            let _ = writeln!(out, "    also needs: {}", candidate.missing.join(", "));
        }
    }
    for (addon, why) in &glue.ignored {
        let _ = writeln!(out, "ignored: {addon} ({why})");
    }
    out
}

/// What `odx --json glue` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx glue")]
pub(crate) struct GlueJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The project's Odoo series.
    series: &'a str,
    /// Repositories looked in.
    scanned: usize,
    /// Glue addons the project does not have.
    candidates: Vec<CandidateJson<'a>>,
    /// Glue addons left out, and why.
    ignored: Vec<IgnoredJson<'a>>,
}

/// A glue addon the project does not have.
#[derive(Debug, Serialize, JsonSchema)]
struct CandidateJson<'a> {
    /// The addon.
    addon: &'a str,
    /// The repository it lives in.
    url: &'a str,
    /// Its directory in the repository.
    path: &'a str,
    /// Its dependencies.
    depends: &'a [String],
    /// The dependencies the project installs that make Odoo install it.
    triggers: &'a [String],
    /// Its dependencies the project does not have.
    missing: &'a [String],
    /// The `odx add` command that adds it.
    add: String,
}

/// A glue addon left out.
#[derive(Debug, Serialize, JsonSchema)]
struct IgnoredJson<'a> {
    /// The addon.
    addon: &'a str,
    /// Why.
    reason: &'a str,
}

fn json(glue: &Glue, series: &str) -> String {
    super::to_json(&GlueJson {
        schema: 1,
        series,
        scanned: glue.scanned,
        candidates: glue
            .candidates
            .iter()
            .map(|candidate| {
                let offered = &candidate.offered;
                CandidateJson {
                    addon: &offered.addon,
                    url: &offered.url,
                    path: &offered.path,
                    depends: &offered.depends,
                    triggers: &offered.triggers,
                    missing: &candidate.missing,
                    add: add_command(candidate, series),
                }
            })
            .collect(),
        ignored: glue
            .ignored
            .iter()
            .map(|(addon, reason)| IgnoredJson { addon, reason })
            .collect(),
    })
}

/// `odx glue [--fail]`.
pub(crate) fn run_glue(args: &GlueArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    match prepare(global, ctx) {
        Ok(prepared) => {
            let series = &prepared.project.config.odoo_version;
            let glue = glue(&prepared);
            let failed = args.fail && !glue.candidates.is_empty();
            Report {
                exit: if failed { Exit::Failure } else { Exit::Success },
                out: if global.json {
                    json(&glue, series)
                } else {
                    text(&glue, series)
                },
                error: failed.then(|| format!("{} glue addons missing", glue.candidates.len())),
                ..Report::default()
            }
        }
        Err(report) => report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(url: &str) -> Candidate {
        Candidate {
            offered: Offered {
                addon: "pos_x_sale".to_owned(),
                depends: Vec::new(),
                triggers: Vec::new(),
                url: url.to_owned(),
                path: "pos_x_sale".to_owned(),
            },
            missing: Vec::new(),
        }
    }

    #[test]
    fn github_repositories_are_added_by_their_short_name() {
        let add = |url: &str| add_command(&candidate(url), "17.0");
        assert_eq!(
            add("git@github.com:OCA/pos.git"),
            "odx add OCA/pos@17.0 --package odoo-addon-pos-x-sale --subdirectory pos_x_sale"
        );
        assert!(
            add("https://gitlab.com/g/pos.git")
                .starts_with("odx add https://gitlab.com/g/pos.git@17.0 ")
        );
        assert!(add("/srv/git/pos").starts_with("odx add /srv/git/pos@17.0 "));
    }
}
