//! `odx lock --auto-update`: take newer releases of index addon packages
//! that only change the last version segment and, with `content_check`,
//! only change files under `allowed_paths`.

use std::fmt::Write as _;
use std::path::Path;

use odx_config::deps::{normalize_name, repin};
use odx_config::edit::PyprojectEdit;
use odx_config::schema::AutoUpdatePolicy;
use odx_config::uvlock::{UvLock, UvPackage};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_forge::{Http, ReqwestHttp};
use odx_platform::autoupdate::{
    Candidate, Update, addon_files, candidate, changed_paths, parse_releases,
};
use odx_platform::requirements::is_odoo;
use odx_vendor::VendorError;
use odx_vendor::archive::{download_error, fetch, get};
use schemars::JsonSchema;
use serde::Serialize;

use super::Report;
use super::aggregate::{Prepared, prepare, uv_program};
use super::edit::save;
use crate::Context;
use crate::cli::GlobalArgs;

const ALLOWED_PATHS: &[&str] = &[
    "i18n/*.po",
    "i18n/*.pot",
    "README.rst",
    "readme/**",
    "static/description/**",
];

/// What the auto-update policy says, with defaults applied.
struct Policy {
    allow: Vec<String>,
    deny: Vec<String>,
    content_check: bool,
    allowed_paths: Vec<String>,
}

/// `odx lock --auto-update`.
pub(crate) fn auto_update(global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let Some((updated, review)) = candidates(ctx, &prepared)? else {
        return Ok(Report {
            out: "auto-update is off ([tool.odx.lock.auto_update] policy = \"none\")\n".to_owned(),
            ..Report::default()
        });
    };
    let mut report = Report::default();
    if !updated.is_empty() {
        repin_pyproject(ctx, &prepared, &updated)?;
        upgrade(ctx, &prepared.project.root, &updated, &mut report);
    }
    report.out = if global.json {
        super::to_json(&AutoUpdateJson {
            schema: 1,
            updated: updated.iter().map(UpdateJson::of).collect(),
            review: review.iter().map(UpdateJson::of).collect(),
        })
    } else {
        describe(&updated, &review)
    };
    Ok(report)
}

/// What `odx --json lock --auto-update` prints.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx lock --auto-update")]
pub(crate) struct AutoUpdateJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// The updates taken.
    updated: Vec<UpdateJson<'a>>,
    /// The updates left for review.
    review: Vec<UpdateJson<'a>>,
}

/// An update taken or left for review.
#[derive(Debug, Serialize, JsonSchema)]
struct UpdateJson<'a> {
    /// The distribution.
    package: &'a str,
    /// The locked version.
    from: &'a str,
    /// The version offered; empty when the index could not be read.
    to: &'a str,
    /// Why it needs review; empty when taken.
    reason: &'a str,
    /// The paths that changed: all of them when taken, those outside
    /// `allowed_paths` when left for review.
    paths: &'a [String],
}

impl<'a> UpdateJson<'a> {
    fn of(update: &'a Update) -> Self {
        Self {
            package: &update.package,
            from: &update.from,
            to: &update.to,
            reason: &update.reason,
            paths: &update.paths,
        }
    }
}

/// Updates the policy takes, and updates it leaves for review.
pub(crate) type Candidates = (Vec<Update>, Vec<Update>);

/// The updates the auto-update policy would take, and those it leaves for
/// review; `None` when the policy is `none`.
pub(crate) fn candidates(
    ctx: &Context<'_>,
    prepared: &Prepared,
) -> Result<Option<Candidates>, Report> {
    let config = &prepared.project.config;
    let settings = config
        .lock
        .as_ref()
        .and_then(|lock| lock.auto_update.clone());
    let settings = settings.as_ref();
    if settings.and_then(|settings| settings.policy) == Some(AutoUpdatePolicy::None) {
        return Ok(None);
    }
    let strings = |values: Option<&Vec<String>>, default: &[&str]| {
        values
            .cloned()
            .unwrap_or_else(|| default.iter().map(|value| (*value).to_owned()).collect())
    };
    let policy = Policy {
        allow: strings(settings.and_then(|s| s.allow.as_ref()), &["odoo-addon-*"]),
        deny: strings(settings.and_then(|s| s.deny.as_ref()), &[]),
        content_check: settings.and_then(|s| s.content_check).unwrap_or(true),
        allowed_paths: strings(
            settings.and_then(|s| s.allowed_paths.as_ref()),
            ALLOWED_PATHS,
        ),
    };
    let root = &prepared.project.root;
    let uv_lock = ctx
        .fs
        .read(&root.join("uv.lock"))
        .ok()
        .and_then(|bytes| UvLock::parse(&String::from_utf8_lossy(&bytes)).ok())
        .ok_or_else(|| {
            Report::error(
                Exit::Failure,
                format!(
                    "{} is missing or unreadable; run `uv lock`",
                    root.join("uv.lock").display()
                ),
            )
        })?;
    let index = ctx
        .env
        .var("ODX_PYPI_URL")
        .unwrap_or_else(|| "https://pypi.org/pypi".to_owned());
    let (mut updated, mut review) = (Vec::new(), Vec::new());
    // One client for every package: each new one starts a runtime and
    // shakes hands anew.
    let http = ReqwestHttp::new();
    for package in uv_lock.packages.iter().filter(|package| {
        let name = normalize_name(&package.name);
        package.source.registry.is_some()
            && is_odoo(&name)
            && name != "odoo"
            && matches_any(&name, &policy.allow)
            && !matches_any(&name, &policy.deny)
    }) {
        let checked = http
            .as_ref()
            .map_err(Clone::clone)
            .map_err(download_error(&index))
            .and_then(|http| check(http, &index, prepared, package, &policy));
        match checked {
            Ok(Some(update)) if update.reason.is_empty() => updated.push(update),
            Ok(Some(update)) => review.push(update),
            Ok(None) => {}
            Err(e) => review.push(Update {
                package: package.name.clone(),
                from: package.version.clone().unwrap_or_default(),
                to: String::new(),
                reason: e.to_string(),
                paths: Vec::new(),
            }),
        }
    }
    Ok(Some((updated, review)))
}

/// Whether the normalized distribution `name` matches one of `patterns`.
fn matches_any(name: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| git_aggregate_rs::fnmatch(name, &normalize_name(pattern)))
}

/// Whether `path` (inside an addon) matches one of `patterns`.
fn allowed(path: &str, patterns: &[String]) -> bool {
    patterns
        .iter()
        .any(|pattern| git_aggregate_rs::fnmatch(path, pattern))
}

/// The update the index offers for `package`, if any, and whether it may
/// be taken.
fn check(
    http: &dyn Http,
    index: &str,
    prepared: &Prepared,
    package: &UvPackage,
    policy: &Policy,
) -> Result<Option<Update>, VendorError> {
    let from = package.version.clone().unwrap_or_default();
    let json = index_json(http, index, &package.name)?;
    let releases = parse_releases(&json).map_err(|message| VendorError::Archive {
        what: format!("{index}/{}/json", package.name),
        message,
    })?;
    let to = match candidate(&from, &prepared.project.config.odoo_version, &releases) {
        Candidate::Current => return Ok(None),
        Candidate::Review(to) => {
            return Ok(Some(Update {
                package: package.name.clone(),
                from,
                to,
                reason: "changes more than the last version segment".to_owned(),
                paths: Vec::new(),
            }));
        }
        Candidate::LastSegment(to) => to,
    };
    let mut update = Update {
        package: package.name.clone(),
        from,
        to,
        reason: String::new(),
        paths: Vec::new(),
    };
    if !policy.content_check {
        return Ok(Some(update));
    }
    let cache = prepared.cache.root().join("wheels");
    let old = package.wheels.first().cloned().unwrap_or_default();
    let new = releases
        .get(&update.to)
        .and_then(|files| files.first())
        .cloned()
        .unwrap_or_default();
    let old_url = old.url.or(old.path).unwrap_or_default();
    let old_files = fetch(http, &cache, &old_url, &old.hash.unwrap_or_default())?;
    let new_files = fetch(http, &cache, &new.url, &new.hash)?;
    let files = |bytes: &[u8], what: &str| {
        addon_files(bytes).map_err(|message| VendorError::Archive {
            what: what.to_owned(),
            message,
        })
    };
    let changed = changed_paths(&files(&old_files, &old_url)?, &files(&new_files, &new.url)?);
    let outside: Vec<String> = changed
        .iter()
        .filter(|path| !allowed(path, &policy.allowed_paths))
        .cloned()
        .collect();
    if outside.is_empty() {
        update.paths = changed;
    } else {
        "changes files outside allowed_paths".clone_into(&mut update.reason);
        update.paths = outside;
    }
    Ok(Some(update))
}

/// `<index>/<name>/json`: over HTTP, or from disk for a `file://` index.
fn index_json(http: &dyn Http, index: &str, name: &str) -> Result<Vec<u8>, VendorError> {
    get(
        http,
        &format!("{}/{name}/json", index.trim_end_matches('/')),
    )
}

/// Move exact pins (`==<old>`) in `pyproject.toml` to the new versions, so
/// uv may take them.
fn repin_pyproject(
    ctx: &Context<'_>,
    prepared: &Prepared,
    updated: &[Update],
) -> Result<(), Report> {
    let mut edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let changed: usize = updated
        .iter()
        .map(|update| {
            repin(
                edit.document_mut(),
                &update.package,
                &update.from,
                &update.to,
            )
        })
        .sum();
    if changed > 0 {
        save(ctx, &prepared.project.file, &edit)?;
    }
    Ok(())
}

/// `uv lock --upgrade-package <dist>==<version>` for every update.
fn upgrade(ctx: &Context<'_>, root: &Path, updated: &[Update], report: &mut Report) {
    let uv = uv_program(ctx.env);
    let invocation = updated.iter().fold(
        Invocation::new(&uv).arg("lock").current_dir(root),
        |invocation, update| {
            invocation
                .arg("--upgrade-package")
                .arg(format!("{}=={}", update.package, update.to))
        },
    );
    let output = OsRunner.run(&invocation);
    if !output.as_ref().is_ok_and(odx_core::Output::success) {
        let detail = output.map_or_else(
            |e| e.to_string(),
            |out| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        );
        report.exit = Exit::Failure;
        report.error = Some(format!("{uv} lock failed: {detail}"));
    }
}

fn describe(updated: &[Update], review: &[Update]) -> String {
    let mut text = String::new();
    for update in updated {
        let changed = if update.paths.is_empty() {
            String::new()
        } else {
            format!(" ({})", update.paths.join(", "))
        };
        let _ = writeln!(
            text,
            "auto-updated {} {} -> {}{changed}",
            update.package, update.from, update.to
        );
    }
    for update in review {
        let to = if update.to.is_empty() {
            String::new()
        } else {
            format!(" -> {}", update.to)
        };
        let paths = if update.paths.is_empty() {
            String::new()
        } else {
            format!(": {}", update.paths.join(", "))
        };
        let _ = writeln!(
            text,
            "needs review {} {}{to}: {}{paths}",
            update.package, update.from, update.reason
        );
    }
    if text.is_empty() {
        text.push_str("no auto-updates\n");
    }
    text
}
