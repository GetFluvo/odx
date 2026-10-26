//! `odx env diff`: what two environments run, unit by unit, read from
//! their branches through git without checking anything out.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use odx_config::environments::Environment;
use odx_core::Exit;
use odx_env::{Change, Entry, Kind, Unit, diff, locked, packages};
use odx_platform::installed::{record_hash, scan, site_packages};
use odx_vendor::record::VendorRecord;
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::prepare;
use super::checkout::{Checkout, environment, git, read};
use super::env::model;
use crate::Context;
use crate::cli::{EnvDiffArgs, GlobalArgs};

/// One side of the comparison.
#[derive(Debug)]
struct Side {
    env: Environment,
    /// The git ref or file it was read from.
    from: String,
    units: Vec<Unit>,
}

/// `odx env diff`.
pub(crate) fn run_diff(args: &EnvDiffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    compare(args, global, ctx).unwrap_or_else(|report| report)
}

fn compare(args: &EnvDiffArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let root = &prepared.project.root;
    let model = model(&prepared.project.config)?;
    let envs = [environment(&model, &args.a)?, environment(&model, &args.b)?];
    // clap passes exactly two paths to --vendored and --installed.
    let (sides, branch_specific) = match (args.vendored.as_deref(), args.installed.as_deref()) {
        (Some([a, b]), _) => (
            sides_of(envs, [a, b], &|path| vendored(ctx, path))?,
            Vec::new(),
        ),
        (_, Some([a, b])) => (
            sides_of(envs, [a, b], &|path| installed(ctx, path))?,
            Vec::new(),
        ),
        _ => {
            let [a, b] = envs;
            let a = side(read(root, a, &args.remote)?);
            let b = side(read(root, b, &args.remote)?);
            let specific = differing(root, &model.branch_specific, &a.from, &b.from);
            ([a, b], specific)
        }
    };
    let entries = diff(&sides[0].units, &sides[1].units);
    let out = if global.json {
        json(&sides, &entries, &branch_specific)
    } else {
        text(&sides, &entries, &branch_specific)
    };
    Ok(Report {
        out,
        ..Report::default()
    })
}

/// `odx env diff` of two checkouts, as text.
pub(crate) fn between(root: &Path, patterns: &[String], a: Checkout, b: Checkout) -> String {
    let specific = differing(root, patterns, &a.from, &b.from);
    let sides = [side(a), side(b)];
    let entries = diff(&sides[0].units, &sides[1].units);
    text(&sides, &entries, &specific)
}

/// The units of a checkout.
fn side(checkout: Checkout) -> Side {
    Side {
        units: units(&checkout),
        env: checkout.env,
        from: checkout.from,
    }
}

/// What a checkout runs: its own addons, its packages, its repositories
/// and Odoo core.
pub(crate) fn units(checkout: &Checkout) -> Vec<Unit> {
    let mut units: Vec<Unit> = checkout
        .addons
        .iter()
        .map(|addon| Unit {
            kind: Kind::Addon,
            name: addon.name.clone(),
            version: odx_analyze::sniff::manifest_version(&addon.manifest),
            source: "project".to_owned(),
            pin: addon.tree.clone(),
        })
        .collect();
    units.extend(checkout.uv.iter().flat_map(|(_, uv)| packages(uv)));
    units.extend(checkout.lock.iter().flat_map(|(_, lock)| locked(lock)));
    units
}

/// Sides read from a file each, by `read`.
fn sides_of(
    envs: [Environment; 2],
    paths: [&PathBuf; 2],
    read: &dyn Fn(&Path) -> Result<Vec<Unit>, Report>,
) -> Result<[Side; 2], Report> {
    let [a, b] = envs;
    let side = |env: Environment, path: &Path| -> Result<Side, Report> {
        Ok(Side {
            units: read(path)?,
            from: path.display().to_string(),
            env,
        })
    };
    Ok([side(a, paths[0])?, side(b, paths[1])?])
}

fn vendored(ctx: &Context<'_>, path: &Path) -> Result<Vec<Unit>, Report> {
    let path = ctx.dir.join(path);
    let failed = |e: String| Report::error(Exit::Failure, format!("{}: {e}", path.display()));
    let bytes = ctx.fs.read(&path).map_err(|e| failed(e.to_string()))?;
    let record = VendorRecord::parse(&String::from_utf8_lossy(&bytes)).map_err(failed)?;
    Ok(record
        .addons
        .into_iter()
        .map(|addon| Unit {
            kind: Kind::Addon,
            name: addon.name,
            version: Some(addon.written_version),
            source: addon.package,
            pin: addon.tree,
        })
        .collect())
}

fn installed(ctx: &Context<'_>, path: &Path) -> Result<Vec<Unit>, Report> {
    let dir = ctx.dir.join(path);
    let site = site_packages(&dir).ok_or_else(|| {
        Report::error(
            Exit::Failure,
            format!(
                "{}: no site-packages with installed distributions",
                dir.display()
            ),
        )
    })?;
    Ok(scan(&site)
        .into_iter()
        .map(|distribution| {
            let files: String = distribution
                .record
                .iter()
                .filter_map(|entry| Some(format!("{} {}\n", entry.path, entry.hash.as_ref()?)))
                .collect();
            Unit {
                kind: Kind::Package,
                name: distribution.name,
                version: Some(distribution.version),
                source: if distribution.commit.is_some() {
                    "git".to_owned()
                } else {
                    "installed".to_owned()
                },
                pin: distribution
                    .commit
                    .unwrap_or_else(|| record_hash(files.as_bytes())),
            }
        })
        .collect())
}

/// The files matching `patterns` that differ between `a` and `b`.
fn differing(root: &Path, patterns: &[String], a: &str, b: &str) -> Vec<String> {
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["diff", "--name-only", a, b, "--"];
    let specs: Vec<String> = patterns
        .iter()
        .map(|pattern| format!(":(glob){pattern}"))
        .collect();
    args.extend(specs.iter().map(String::as_str));
    // Both refs were resolved already.
    git(root, &args)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// `pin` shortened for reading: commits and trees to 7 characters, hashes
/// to their algorithm and 7 characters.
fn short(pin: &str) -> String {
    let (prefix, digest) = pin
        .split_once([':', '='])
        .filter(|(algorithm, _)| algorithm.starts_with("sha"))
        .map_or(("", pin), |(algorithm, digest)| (algorithm, digest));
    if digest.len() > 12
        && digest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        let cut: String = digest.chars().take(7).collect();
        if prefix.is_empty() {
            cut
        } else {
            format!("{prefix}:{cut}")
        }
    } else {
        pin.to_owned()
    }
}

fn describe(unit: &Unit, with_source: bool) -> String {
    let mut parts = Vec::new();
    if with_source {
        parts.push(unit.source.clone());
    }
    parts.extend(unit.version.clone());
    let pin = short(&unit.pin);
    match unit.kind {
        Kind::Addon | Kind::Repository => parts.push(format!("tree {pin}")),
        Kind::Odoo => parts.push(format!("rev {pin}")),
        Kind::Package if unit.version.as_deref() != Some(unit.pin.as_str()) => parts.push(pin),
        Kind::Package => {}
    }
    parts.join(" ")
}

fn label(entry: &Entry) -> String {
    match entry.kind {
        Kind::Repository => format!("repository {}", entry.name),
        _ => entry.name.clone(),
    }
}

fn text(sides: &[Side; 2], entries: &[Entry], branch_specific: &[String]) -> String {
    let [a, b] = sides;
    let mut out = format!(
        "{} ({}) and {} ({})\n",
        a.env.name, a.from, b.env.name, b.from
    );
    if a.env.series != b.env.series {
        let _ = writeln!(
            out,
            "series differ: {} runs {}, {} runs {}",
            a.env.name, a.env.series, b.env.name, b.env.series
        );
    }
    let sections = [
        (Change::Ahead, format!("newer in {}", a.env.name)),
        (Change::Behind, format!("older in {}", a.env.name)),
        (Change::Changed, "changed".to_owned()),
        (Change::SourceChanged, "from another source".to_owned()),
        (Change::OnlyInA, format!("only in {}", a.env.name)),
        (Change::OnlyInB, format!("only in {}", b.env.name)),
    ];
    let width = entries
        .iter()
        .filter(|entry| entry.change != Change::Identical)
        .map(|entry| label(entry).chars().count())
        .max()
        .unwrap_or(0);
    for (change, title) in sections {
        let listed: Vec<&Entry> = entries.iter().filter(|e| e.change == change).collect();
        if listed.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{title}:");
        for entry in listed {
            let with_source = change == Change::SourceChanged;
            let side = |unit: &Option<Unit>| {
                unit.as_ref()
                    .map_or_else(|| "-".to_owned(), |unit| describe(unit, with_source))
            };
            let _ = writeln!(
                out,
                "  {:width$}  {} | {}",
                label(entry),
                side(&entry.a),
                side(&entry.b)
            );
        }
    }
    if !branch_specific.is_empty() {
        let _ = writeln!(out, "branch-specific files that differ (never promoted):");
        for file in branch_specific {
            let _ = writeln!(out, "  {file}");
        }
    }
    let identical = entries
        .iter()
        .filter(|entry| entry.change == Change::Identical)
        .count();
    let _ = if identical == entries.len() {
        writeln!(out, "identical ({identical} pinned)")
    } else {
        writeln!(
            out,
            "{} differ, {identical} identical",
            entries.len() - identical
        )
    };
    out
}

/// What `odx --json env diff` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx env diff")]
pub(crate) struct EnvDiffJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The first environment.
    a: SideJson<'a>,
    /// The second environment.
    b: SideJson<'a>,
    /// Whether they run different series.
    series_differ: bool,
    /// Every unit either side runs.
    entries: Vec<EntryJson<'a>>,
    /// Branch-specific files that differ; they are never promoted.
    branch_specific: &'a [String],
}

/// One side of the comparison.
#[derive(Debug, Serialize, JsonSchema)]
struct SideJson<'a> {
    /// The environment.
    env: &'a str,
    /// Its branch or branch pattern.
    branch: &'a str,
    /// The series it runs.
    series: &'a str,
    /// The git ref or file it was read from.
    from: &'a str,
}

/// One unit on both sides.
#[derive(Debug, Serialize, JsonSchema)]
struct EntryJson<'a> {
    /// `odoo`, `addon`, `package` or `repository`.
    kind: &'static str,
    /// Its name.
    name: &'a str,
    /// `identical`, `ahead`, `behind`, `changed`, `source-changed`,
    /// `only-in-a` or `only-in-b`.
    change: &'static str,
    /// It on the first side; null when that side does not run it.
    a: Option<UnitJson<'a>>,
    /// It on the second side; null when that side does not run it.
    b: Option<UnitJson<'a>>,
}

/// A unit on one side.
#[derive(Debug, Serialize, JsonSchema)]
struct UnitJson<'a> {
    /// Its version, when known.
    version: Option<&'a str>,
    /// Where it comes from.
    source: &'a str,
    /// Its exact pin: a commit, a tree or a hash.
    pin: &'a str,
}

fn json(sides: &[Side; 2], entries: &[Entry], branch_specific: &[String]) -> String {
    fn side(side: &Side) -> SideJson<'_> {
        SideJson {
            env: &side.env.name,
            branch: &side.env.branch,
            series: &side.env.series,
            from: &side.from,
        }
    }
    fn unit(unit: &Unit) -> UnitJson<'_> {
        UnitJson {
            version: unit.version.as_deref(),
            source: &unit.source,
            pin: &unit.pin,
        }
    }
    super::to_json(&EnvDiffJson {
        schema: 1,
        a: side(&sides[0]),
        b: side(&sides[1]),
        series_differ: sides[0].env.series != sides[1].env.series,
        entries: entries
            .iter()
            .map(|entry| EntryJson {
                kind: entry.kind.name(),
                name: &entry.name,
                change: entry.change.name(),
                a: entry.a.as_ref().map(unit),
                b: entry.b.as_ref().map(unit),
            })
            .collect(),
        branch_specific,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pins() {
        assert_eq!(short("0123456789abcdef0123"), "0123456");
        assert_eq!(short("sha256:0123456789abcdef"), "sha256:0123456");
        assert_eq!(short("sha256=AbCd-_0123456789"), "sha256:AbCd-_0");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short("../local/path/somewhere"), "../local/path/somewhere");
    }
}
