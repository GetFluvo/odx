//! `odx diff`: changes between two states of `odx.lock` that are likely to
//! affect the database schema, per addon: models, fields, parents and access
//! rights from odoo-lint's index, with the version bump and migration
//! scripts that come with them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use odx_analyze::sniff::{AddonChange, Change, Script};
use odx_config::Problem;
use odx_config::lock::Lockfile;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_git::Merger;
use odx_lint::schema::{Schema, SchemaChange, changes};
use odx_vendor::archive::{extract, subdirectory_entry};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::prepare;
use super::changelog::states;
use super::sniff::sniff_files;
use crate::Context;
use crate::cli::{DiffArgs, GlobalArgs};

/// What happened to one addon.
#[derive(Debug)]
struct AddonDiff {
    addon: String,
    /// `added`, `removed` or `changed`.
    state: &'static str,
    /// Models created and fields declared, for an added addon.
    size: (usize, usize),
    changes: Vec<SchemaChange>,
    /// Its version change and migration scripts, if any.
    migration: Option<AddonChange>,
}

/// The addons one repository or package changes.
#[derive(Debug)]
struct SourceDiff {
    name: String,
    /// `repository` or `package`.
    kind: &'static str,
    addons: Vec<AddonDiff>,
}

/// One repository or package to compare, between two revisions, limited
/// to the subdirectory of a package.
struct Range<'a> {
    name: &'a str,
    kind: &'static str,
    url: &'a str,
    old: &'a str,
    new: &'a str,
    path: Option<&'a str>,
}

/// The repositories and packages whose content differs between `old` and
/// `new`: a repository from its previous result (its base when it is
/// new), a package from its previous commit.
fn ranges<'a>(old: &'a Lockfile, new: &'a Lockfile) -> Vec<Range<'a>> {
    let mut ranges = Vec::new();
    for entry in &new.repositories {
        let from = old
            .repositories
            .iter()
            .find(|before| before.name == entry.name)
            .map_or(entry.base_sha.as_str(), |before| {
                before.result_tree.as_str()
            });
        if from != entry.result_tree {
            ranges.push(Range {
                name: &entry.name,
                kind: "repository",
                url: &entry.url,
                old: from,
                new: &entry.result_tree,
                path: None,
            });
        }
    }
    for (dist, package) in &new.packages {
        if let Some(before) = old.packages.get(dist).filter(|b| b.sha != package.sha) {
            ranges.push(Range {
                name: dist,
                kind: "package",
                url: &package.url,
                old: &before.sha,
                new: &package.sha,
                path: package.subdirectory.as_deref(),
            });
        }
    }
    ranges
}

/// The schema of directory `dir` at `rev`, extracted under `dest`; `None`
/// when `rev` has no addon there.
fn schema_at(
    merger: &Merger,
    repo: &Path,
    rev: &str,
    dir: &str,
    dest: &Path,
) -> Result<Option<Schema>, String> {
    // Both revisions were compared already, so a failing archive means the
    // directory is not there.
    let Ok(zip) = merger.archive(repo, rev, Some(dir)) else {
        return Ok(None);
    };
    let select = |name: &str| subdirectory_entry(dir, name);
    extract(&zip, dir, &select, &|_| false, dest)
        .map(|written| {
            written
                .first()
                .and_then(|addon| Schema::of(&dest.join(addon)))
        })
        .map_err(|e| e.to_string())
}

/// The addons `range` changes, with their schema changes and migrations.
fn compare(
    merger: &Merger,
    repo: &Path,
    range: &Range<'_>,
    scratch: &Path,
) -> Result<Vec<AddonDiff>, String> {
    let files = merger
        .changed_files(repo, range.old, range.new, range.path)
        .map_err(|e| e.to_string())?;
    let dirs: BTreeSet<String> = range.path.map_or_else(
        || {
            files
                .iter()
                .filter_map(|(_, file)| file.split_once('/').map(|(dir, _)| dir.to_owned()))
                .collect()
        },
        |path| {
            files
                .first()
                .map(|_| path.trim_matches('/').to_owned())
                .into_iter()
                .collect()
        },
    );
    let mut migrations: BTreeMap<String, AddonChange> =
        sniff_files(merger, repo, range.old, range.new, &files)
            .into_iter()
            .map(|change| (change.addon.clone(), change))
            .collect();
    let mut addons = Vec::new();
    for dir in &dirs {
        let addon = dir.rsplit('/').next().unwrap_or(dir).to_owned();
        let before = schema_at(merger, repo, range.old, dir, &scratch.join("old"))?;
        let after = schema_at(merger, repo, range.new, dir, &scratch.join("new"))?;
        let (state, size, found) = match (before, after) {
            (None, None) => continue,
            (None, Some(after)) => ("added", after.size(), Vec::new()),
            (Some(_), None) => ("removed", (0, 0), Vec::new()),
            (Some(before), Some(after)) => ("changed", (0, 0), changes(&before, &after)),
        };
        let migration = migrations.remove(&addon);
        if state == "changed" && found.is_empty() && migration.is_none() {
            continue;
        }
        addons.push(AddonDiff {
            addon,
            state,
            size,
            changes: found,
            migration,
        });
    }
    // Scripts of addons that are not top-level folders (OpenUpgrade's).
    addons.extend(migrations.into_values().map(|migration| AddonDiff {
        addon: migration.addon.clone(),
        state: "changed",
        size: (0, 0),
        changes: Vec::new(),
        migration: Some(migration),
    }));
    addons.sort_by(|a, b| a.addon.cmp(&b.addon));
    Ok(addons)
}

fn version(migration: Option<&AddonChange>) -> String {
    match migration.map(|m| (&m.previous_version, &m.version)) {
        Some((Some(old), Some(new))) if old != new => format!(" {new} (was {old})"),
        Some((_, Some(new))) => format!(" {new}"),
        _ => String::new(),
    }
}

fn text(sources: &[SourceDiff]) -> String {
    let mut out = String::new();
    for source in sources.iter().filter(|source| !source.addons.is_empty()) {
        let _ = writeln!(out, "schema changes in {} ({}):", source.name, source.kind);
        for addon in &source.addons {
            let version = version(addon.migration.as_ref());
            let _ = match addon.state {
                "added" => writeln!(
                    out,
                    "  {}{version}: new addon ({} models, {} fields)",
                    addon.addon, addon.size.0, addon.size.1
                ),
                "removed" => writeln!(out, "  {}: removed addon", addon.addon),
                _ => writeln!(out, "  {}{version}", addon.addon),
            };
            for change in &addon.changes {
                let _ = writeln!(out, "    {change}");
            }
            for script in addon.migration.iter().flat_map(|m| &m.scripts) {
                let change = match script.change {
                    Change::Added => "added",
                    Change::Modified => "modified",
                };
                let _ = writeln!(
                    out,
                    "    {:<4} {change:<8} {}",
                    script.phase.as_str(),
                    script.path
                );
            }
        }
    }
    if out.is_empty() {
        out.push_str("no schema changes\n");
    }
    out
}

/// What `odx --json diff` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx diff")]
pub(crate) struct DiffJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The revision of `odx.lock` compared from.
    since: &'a str,
    /// The revision compared to, or `working tree`.
    to: &'a str,
    /// The repositories and packages with addons that changed.
    sources: Vec<SourceDiffJson<'a>>,
    /// Repositories or packages that could not be compared.
    errors: &'a [String],
}

/// The addons one repository or package changes.
#[derive(Debug, Serialize, JsonSchema)]
struct SourceDiffJson<'a> {
    /// Repository name or distribution.
    name: &'a str,
    /// `repository` or `package`.
    kind: &'a str,
    /// The addons that changed.
    addons: Vec<AddonDiffJson<'a>>,
}

/// What happened to one addon.
#[derive(Debug, Serialize, JsonSchema)]
struct AddonDiffJson<'a> {
    /// The addon.
    addon: &'a str,
    /// `added`, `removed` or `changed`.
    state: &'a str,
    /// Models created, for an added addon.
    models: usize,
    /// Fields declared, for an added addon.
    fields: usize,
    /// The manifest version before, when it changed.
    previous_version: Option<&'a str>,
    /// The manifest version after, when it changed.
    version: Option<&'a str>,
    /// Changes to models, fields, parents and access rights.
    changes: Vec<SchemaChangeJson<'a>>,
    /// Migration scripts added or modified, sorted by path.
    scripts: &'a [Script],
}

/// A change likely to affect the database schema.
#[derive(Debug, Serialize, JsonSchema)]
struct SchemaChangeJson<'a> {
    /// What kind of change, e.g. `field-added`.
    change: &'a str,
    /// The change as `odx diff` writes it.
    text: String,
}

impl AddonDiff {
    fn json(&self) -> AddonDiffJson<'_> {
        let migration = self.migration.as_ref();
        AddonDiffJson {
            addon: &self.addon,
            state: self.state,
            models: self.size.0,
            fields: self.size.1,
            previous_version: migration.and_then(|m| m.previous_version.as_deref()),
            version: migration.and_then(|m| m.version.as_deref()),
            changes: self
                .changes
                .iter()
                .map(|change| SchemaChangeJson {
                    change: change.name(),
                    text: change.to_string(),
                })
                .collect(),
            scripts: migration.map_or(&[][..], |m| m.scripts.as_slice()),
        }
    }
}

fn json(since: &str, to: &str, sources: &[SourceDiff], problems: &[String]) -> String {
    super::to_json(&DiffJson {
        schema: 1,
        since,
        to,
        sources: sources
            .iter()
            .map(|source| SourceDiffJson {
                name: &source.name,
                kind: source.kind,
                addons: source.addons.iter().map(AddonDiff::json).collect(),
            })
            .collect(),
        errors: problems,
    })
}

/// `odx diff [--since REV] [--to REV]`.
pub(crate) fn run_diff(args: &DiffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    diff(args, global, ctx).unwrap_or_else(|report| report)
}

fn diff(args: &DiffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let mut prepared = prepare(global, ctx)?;
    let (old, new, to) = states(&mut prepared, &args.since, args.to.as_deref())?;
    let base = prepared.cache.root().join("scratch");
    let scratch = std::fs::create_dir_all(&base)
        .and_then(|()| tempfile::tempdir_in(&base))
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", base.display())))?;
    let merger = Merger::new(Arc::new(OsRunner));
    let mut problems = Vec::new();
    let mut sources = Vec::new();
    for (index, range) in ranges(&old, &new).iter().enumerate() {
        let dir = scratch.path().join(index.to_string());
        let repo = prepared.cache.repo_dir(range.url);
        match compare(&merger, &repo, range, &dir) {
            Ok(addons) => sources.push(SourceDiff {
                name: range.name.to_owned(),
                kind: range.kind,
                addons,
            }),
            Err(e) => problems.push(format!("{}: cannot compare: {e}", range.name)),
        }
    }
    let out = if global.json {
        json(&args.since, &to, &sources, &problems)
    } else {
        text(&sources)
    };
    Ok(Report {
        out,
        warnings: problems
            .into_iter()
            .map(|message| Problem {
                key: "diff".to_owned(),
                message,
            })
            .collect(),
        ..Report::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "odx")
            .env("GIT_AUTHOR_EMAIL", "odx@example.com")
            .env("GIT_COMMITTER_NAME", "odx")
            .env("GIT_COMMITTER_EMAIL", "odx@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn addon(repo: &Path, name: &str, version: &str, python: &str) {
        write(
            &repo.join(name).join("__manifest__.py"),
            &format!("{{\"name\": \"X\", \"version\": \"{version}\"}}\n"),
        );
        write(
            &repo.join(name).join("__init__.py"),
            "from . import models\n",
        );
        write(
            &repo.join(name).join("models/__init__.py"),
            "from . import x\n",
        );
        write(
            &repo.join(name).join("models/x.py"),
            &format!("from odoo import fields, models\n\n{python}"),
        );
    }

    const ORDER: &str =
        "class Order(models.Model):\n    _inherit = \"pos.order\"\n\n    note = fields.Char()\n";

    /// Two commits: `pos_x` gains a field with a version bump and a
    /// migration script, `pos_new` is added, `pos_gone` removed, `pos_same`
    /// changes a script without a version bump, `pos_doc` only a README, `docs` is no addon, and OpenUpgrade gets a
    /// script.
    fn repository(dir: &Path) -> (String, String) {
        git(dir, &["init", "--quiet"]);
        addon(dir, "pos_x", "17.0.1.0.0", ORDER);
        addon(dir, "pos_gone", "17.0.1.0.0", ORDER);
        addon(dir, "pos_same", "17.0.1.0.0", ORDER);
        addon(dir, "pos_doc", "17.0.1.0.0", ORDER);
        write(
            &dir.join("pos_same/migrations/17.0.1.0.0/post-x.py"),
            "post\n",
        );
        write(&dir.join("docs/index.md"), "docs\n");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", "old"]);
        let old = git(dir, &["rev-parse", "HEAD"]);
        addon(
            dir,
            "pos_x",
            "17.0.1.1.0",
            &format!("{ORDER}    total = fields.Float()\n"),
        );
        write(
            &dir.join("pos_x/migrations/17.0.1.1.0/pre-migrate.py"),
            "pre\n",
        );
        addon(
            dir,
            "pos_new",
            "17.0.1.0.0",
            "class New(models.Model):\n    _name = \"pos.new\"\n\n    name = fields.Char()\n",
        );
        std::fs::remove_dir_all(dir.join("pos_gone")).unwrap();
        write(&dir.join("pos_doc/README.rst"), "readme\n");
        write(
            &dir.join("pos_same/migrations/17.0.1.0.0/post-x.py"),
            "post, fixed\n",
        );
        write(&dir.join("docs/index.md"), "more docs\n");
        write(
            &dir.join("openupgrade_scripts/scripts/sale/17.0.1.0/end-migration.py"),
            "end\n",
        );
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", "new"]);
        (old, git(dir, &["rev-parse", "HEAD"]))
    }

    fn range<'a>(old: &'a str, new: &'a str, path: Option<&'a str>) -> Range<'a> {
        Range {
            name: "pos",
            kind: "repository",
            url: "unused",
            old,
            new,
            path,
        }
    }

    #[test]
    fn addons_are_compared_between_two_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (old, new) = repository(&repo);
        let merger = Merger::new(Arc::new(OsRunner));
        let addons = compare(
            &merger,
            &repo,
            &range(&old, &new, None),
            &tmp.path().join("a"),
        )
        .unwrap();
        let sources = [SourceDiff {
            name: "pos".to_owned(),
            kind: "repository",
            addons,
        }];
        assert_eq!(
            text(&sources),
            "schema changes in pos (repository):
  pos_gone: removed addon
  pos_new: new addon (1 models, 1 fields)
  pos_same 17.0.1.0.0
    post modified pos_same/migrations/17.0.1.0.0/post-x.py
  pos_x 17.0.1.1.0 (was 17.0.1.0.0)
    + field pos.order.total: Float
    pre  added    pos_x/migrations/17.0.1.1.0/pre-migrate.py
  sale
    end  added    openupgrade_scripts/scripts/sale/17.0.1.0/end-migration.py
"
        );
        let value: serde_json::Value =
            serde_json::from_str(&json("a", "b", &sources, &["x".to_owned()])).unwrap();
        let x = &value["sources"][0]["addons"][3];
        assert_eq!(x["changes"][0]["change"], "field-added");
        assert_eq!(x["previous_version"], "17.0.1.0.0");
        assert_eq!(x["scripts"][0]["phase"], "pre");
        assert_eq!(value["sources"][0]["addons"][1]["models"], 1);
        assert_eq!(value["errors"][0], "x");

        // A package: only its subdirectory.
        let addons = compare(
            &merger,
            &repo,
            &range(&old, &new, Some("pos_x/")),
            &tmp.path().join("b"),
        )
        .unwrap();
        assert_eq!(
            addons.iter().map(|a| a.addon.as_str()).collect::<Vec<_>>(),
            ["pos_x"]
        );
        // Unchanged: nothing.
        let none = compare(
            &merger,
            &repo,
            &range(&new, &new, Some("pos_x")),
            tmp.path(),
        )
        .unwrap();
        assert!(none.is_empty());
        assert_eq!(text(&[]), "no schema changes\n");
    }

    #[test]
    fn failures_are_said() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let (old, new) = repository(&repo);
        let merger = Merger::new(Arc::new(OsRunner));
        let missing = "0".repeat(40);
        assert!(compare(&merger, &repo, &range(&missing, &new, None), tmp.path()).is_err());
        // Extracting into a file fails.
        let file = tmp.path().join("file");
        std::fs::write(&file, "").unwrap();
        let error = compare(&merger, &repo, &range(&old, &new, None), &file).unwrap_err();
        assert!(error.contains("file"), "{error}");
        // Also for an addon that is new.
        let new_addon = range(&old, &new, Some("pos_new"));
        assert!(compare(&merger, &repo, &new_addon, &file).is_err());
    }

    #[test]
    fn ranges_are_the_repositories_and_packages_that_moved() {
        let tree = |c: char| c.to_string().repeat(40);
        let lock = |result: char, package: char, extra: &str| {
            Lockfile::parse(&format!(
                "version = 1\n[[repositories]]\nname = \"pos\"\nurl = \"u\"\nbase = \"17.0\"\n\
                 base_sha = \"{}\"\nresult_tree = \"{}\"\n{extra}\
                 [packages.p]\nurl = \"u\"\nsource = {{ pr = 1 }}\nsubdirectory = \"p\"\nsha = \"{}\"\n",
                tree('a'),
                tree(result),
                tree(package)
            ))
            .unwrap()
        };
        let web = format!(
            "[[repositories]]\nname = \"web\"\nurl = \"w\"\nbase = \"17.0\"\nbase_sha = \"{}\"\nresult_tree = \"{}\"\n",
            tree('a'),
            tree('b')
        );
        let old = lock('b', 'c', "");
        let new = lock('d', 'e', &web);
        let found: Vec<String> = ranges(&old, &new)
            .iter()
            .map(|r| format!("{} {}..{} {:?}", r.name, &r.old[..1], &r.new[..1], r.path))
            .collect();
        assert_eq!(
            found,
            ["pos b..d None", "web a..b None", "p c..e Some(\"p\")"]
        );
        assert!(ranges(&new, &new).is_empty());
    }
}
