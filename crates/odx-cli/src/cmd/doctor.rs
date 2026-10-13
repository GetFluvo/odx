//! `odx doctor`: the local setup in one list, with a fix for each problem.

use std::fmt::Write as _;

use odx_aggregate::mergiraf_program;
use odx_config::schema::{MergirafMode, RefStyle};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner};
use odx_forge::{
    CachedHttp, ForgeClient, ForgeError, ForgeKind, RepoUrl, ReqwestHttp, Visibility, token,
};
use odx_platform::{profile, profile_name, provided_set};

use super::aggregate::{Prepared, prepare, uv_program};
use super::{Report, human, size};
use crate::Context;
use crate::cli::GlobalArgs;

/// How a check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// Fine.
    Ok,
    /// For information only.
    Info,
    /// Works, but worth fixing.
    Warn,
    /// Must be fixed: the command fails.
    Fail,
}

impl Level {
    const fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone)]
struct Check {
    level: Level,
    item: String,
    detail: String,
}

fn check(level: Level, item: &str, detail: impl Into<String>) -> Check {
    Check {
        level,
        item: item.to_owned(),
        detail: detail.into(),
    }
}

/// `odx doctor`.
pub(crate) fn run_doctor(global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    let prepared = prepare(global, ctx).ok();
    let mut checks = tools(ctx, prepared.as_ref());
    match &prepared {
        Some(prepared) => checks.extend(project(ctx, global, prepared)),
        None => checks.push(check(
            Level::Info,
            "project",
            "no pyproject.toml with [tool.odx] here; only the machine was checked",
        )),
    }
    let failed = checks.iter().filter(|c| c.level == Level::Fail).count();
    let mut report = Report::default();
    if global.json {
        let items: Vec<serde_json::Value> = checks
            .iter()
            .map(|c| serde_json::json!({ "level": c.level.label().to_lowercase(), "item": c.item, "detail": c.detail }))
            .collect();
        report.out = format!(
            "{}\n",
            serde_json::json!({ "schema": 1, "ok": failed == 0, "checks": items })
        );
    } else {
        for c in &checks {
            let _ = writeln!(
                report.out,
                "{:<4} {}: {}",
                c.level.label(),
                c.item,
                c.detail
            );
        }
    }
    if failed > 0 {
        report.exit = Exit::Failure;
        report.error = Some(format!("{failed} checks failed"));
    }
    report
}

/// The first line `program --version` prints, if it runs.
fn version(program: &str) -> Option<String> {
    OsRunner
        .run(&Invocation::new(program).arg("--version"))
        .ok()
        .filter(odx_core::Output::success)
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
}

/// git (required), mergiraf, uv and odl.
fn tools(ctx: &Context<'_>, prepared: Option<&Prepared>) -> Vec<Check> {
    let uv = uv_program(ctx.env);
    let odl = ctx.env.var("ODX_ODL").unwrap_or_else(|| "odl".to_owned());
    let programs = [
        ("git", "git".to_owned(), Level::Fail, "install git"),
        (
            "uv",
            uv,
            Level::Warn,
            "packages cannot be locked or built; install uv",
        ),
        ("odl", odl, Level::Info, "optional: odoo-lint for odx lint"),
    ];
    let mut checks: Vec<Check> = programs
        .iter()
        .map(|(item, program, missing, hint)| {
            version(program).map_or_else(
                || check(*missing, item, format!("{program} not found; {hint}")),
                |found| check(Level::Ok, item, found),
            )
        })
        .collect();
    let mode = prepared
        .and_then(|prepared| prepared.project.config.mergiraf)
        .unwrap_or_default();
    checks.push(match mergiraf_program(mode, ctx.env, &OsRunner) {
        Ok(Some(program)) => check(
            Level::Ok,
            "mergiraf",
            format!("{} resolves conflicts", program.to_string_lossy()),
        ),
        Ok(None) if mode == MergirafMode::Never => {
            check(Level::Info, "mergiraf", "off (mergiraf = \"never\")")
        }
        Ok(None) => check(
            Level::Warn,
            "mergiraf",
            "not found; conflicts are not resolved structurally (install mergiraf)",
        ),
        Err(e) => check(Level::Fail, "mergiraf", e),
    });
    checks
}

/// Checks that need the project: tokens, the vault, the platform, the
/// cache, packages in develop mode.
fn project(ctx: &Context<'_>, global: &GlobalArgs, prepared: &Prepared) -> Vec<Check> {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let mut checks = vec![check(
        Level::Ok,
        "project",
        format!(
            "{} (Odoo {})",
            prepared.project.file.display(),
            config.odoo_version
        ),
    )];
    let mut hosts: Vec<(String, ForgeKind)> = config
        .repositories
        .iter()
        .map(|repo| repo.url.as_str())
        .chain(config.packages.values().filter_map(|p| p.url.as_deref()))
        .chain(config.mirror.iter().map(|mirror| mirror.url.as_str()))
        .filter_map(RepoUrl::parse)
        .map(|repo| {
            let kind = ForgeKind::guess(&repo.host);
            (repo.host, kind)
        })
        .collect();
    hosts.sort_by(|a, b| a.0.cmp(&b.0));
    hosts.dedup_by(|a, b| a.0 == b.0);
    for (host, kind) in hosts {
        checks.push(match token(&host, kind, ctx.env, &OsRunner) {
            Some(_) => check(Level::Ok, &format!("token {host}"), "found"),
            None => check(
                Level::Warn,
                &format!("token {host}"),
                "none; anonymous API limits apply (set ODX_TOKEN_<HOST> or the forge's token variable)",
            ),
        });
    }
    checks.extend(vault(ctx, global, prepared));
    let provided = provided_set(config, root, |path| {
        ctx.fs
            .read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    });
    let platform = config.platform.as_ref();
    let overrides = platform
        .map(|p| {
            p.provided_extra.as_ref().map_or(0, Vec::len)
                + p.provided_remove.as_ref().map_or(0, Vec::len)
        })
        .unwrap_or_default();
    checks.push(match provided {
        Ok(set) => check(
            Level::Ok,
            "platform",
            format!(
                "{}, provided set {} ({} packages, {overrides} overrides)",
                profile_name(profile(config)),
                set.label,
                set.len()
            ),
        ),
        Err(e) => check(Level::Fail, "platform", e),
    });
    let cache = prepared.cache.root();
    checks.push(check(
        Level::Info,
        "cache",
        format!("{} ({})", cache.display(), human(size(cache))),
    ));
    checks.push(check(
        Level::Info,
        "background sync",
        "not installed (odx daemon install sets it up)",
    ));
    let developing = ctx
        .fs
        .read(&root.join(".odx/develop.toml"))
        .ok()
        .and_then(|bytes| odx_dev::DevelopState::parse(&String::from_utf8_lossy(&bytes)).ok())
        .map(|state| state.packages.into_keys().collect::<Vec<_>>())
        .unwrap_or_default();
    if !developing.is_empty() {
        checks.push(check(
            Level::Warn,
            "develop mode",
            format!(
                "{}; `odx dev --restore` before locking",
                developing.join(", ")
            ),
        ));
    }
    checks
}

/// The vault: private, and its lock refs protected.
fn vault(ctx: &Context<'_>, global: &GlobalArgs, prepared: &Prepared) -> Vec<Check> {
    let Some(mirror) = prepared.project.config.mirror.as_ref() else {
        return vec![check(
            Level::Warn,
            "vault",
            "no [tool.odx.mirror]; locked commits disappear when upstream force-pushes",
        )];
    };
    let Some(repo) = RepoUrl::parse(&mirror.url) else {
        return vec![check(
            Level::Info,
            "vault",
            format!("{} is local; no forge protection to check", mirror.url),
        )];
    };
    if global.offline {
        return vec![check(Level::Warn, "vault", "not checked (--offline)")];
    }
    let found = ReqwestHttp::new().and_then(|http| {
        let http = CachedHttp::new(http, prepared.cache.root().join("http"));
        let sleep = std::thread::sleep;
        let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
        let kind = client.kind(&repo)?;
        let visibility = client.visibility(&repo, kind)?;
        let protected = client.vault_tags_protected(&repo, kind)?;
        Ok::<_, ForgeError>((kind, visibility, protected))
    });
    let (kind, visibility, protected) = match found {
        Ok(found) => found,
        Err(e) => return vec![check(Level::Fail, "vault", format!("{}: {e}", mirror.url))],
    };
    let mut checks = vec![match visibility {
        Visibility::Private => check(
            Level::Ok,
            "vault visibility",
            format!("{} is private on {}", mirror.url, kind.name()),
        ),
        Visibility::Internal => check(
            Level::Warn,
            "vault visibility",
            format!(
                "{} is internal on {}; everyone on the forge can read it",
                mirror.url,
                kind.name()
            ),
        ),
        Visibility::Public if mirror.allow_public == Some(true) => check(
            Level::Warn,
            "vault visibility",
            format!("{} is public (allow_public = true)", mirror.url),
        ),
        Visibility::Public => check(
            Level::Fail,
            "vault visibility",
            format!("{} is public; make it private", mirror.url),
        ),
    }];
    let tags = mirror.ref_style == Some(RefStyle::Tags) || kind == ForgeKind::Gitlab;
    checks.push(match (tags, protected) {
        (true, true) => check(Level::Ok, "vault protection", "odx/* tags cannot be deleted or moved"),
        (true, false) => check(
            Level::Fail,
            "vault protection",
            "odx/* tags are not protected; protect them on the forge (or create the vault with mirror.auto_create)",
        ),
        (false, _) => check(
            Level::Fail,
            "vault protection",
            "refs/odx/* cannot be protected on the forge; set [tool.odx.mirror] ref_style = \"tags\" and protect odx/* tags",
        ),
    });
    checks
}
