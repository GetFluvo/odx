//! `odx sync` (prefetch registered workspaces into the shared cache) and
//! `odx cache` (inspect it, collect unused repositories).

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use odx_aggregate::{date, plan_config};
use odx_config::Span;
use odx_config::user::{self, UserConfig, load_user_config};
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_git::{RepoLock, SystemGit, detach, url_hash};
use odx_sync::{
    CachedRepo, Presence, SyncRecord, SyncedWorkspace, busy, parse_size, register, to_collect,
    unregister,
};

use super::aggregate::{Prepared, prepare, run_aggregate};
use super::vendor::prefetch_wheels;
use super::{Report, human, size};
use crate::Context;
use crate::cli::{AggregateArgs, CacheCommand, GlobalArgs, SyncArgs};

fn failed(exit: Exit, message: impl std::fmt::Display) -> Report {
    Report::error(exit, message.to_string())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The shared cache and the user configuration.
pub(crate) fn shared(ctx: &Context<'_>) -> Result<(UserConfig, PathBuf), Report> {
    let user = load_user_config(ctx.fs, ctx.env).map_err(|e| failed(Exit::Usage, e))?;
    let cache = user::cache_dir(&user, ctx.env)
        .ok_or_else(|| failed(Exit::Usage, "no shared cache directory; set ODX_CACHE_DIR"))?;
    Ok((user, cache))
}

/// Announce a foreground command to `odx sync` for as long as the returned
/// value lives.
pub(crate) fn presence(ctx: &Context<'_>) -> Option<Presence> {
    shared(ctx)
        .ok()
        .and_then(|(_, cache)| Presence::enter(&cache))
}

/// Wait until no foreground odx command uses `cache`; `false` when
/// `deadline` came first.
fn idle(cache: &Path, deadline: Instant) -> bool {
    while busy(cache) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// `odx sync`.
pub(crate) fn run_sync(args: &SyncArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    sync(args, global, ctx).unwrap_or_else(|report| report)
}

fn sync(args: &SyncArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    if args.register || args.unregister {
        return registry(args.register, global, ctx);
    }
    let (user, cache) = shared(ctx)?;
    let settings = user.sync.clone().unwrap_or_default();
    let max_duration = match &args.max_duration {
        Some(text) => text
            .parse::<Span>()
            .map_err(|e| failed(Exit::Usage, e))?
            .as_duration(),
        None => settings
            .max_duration
            .map_or(Duration::from_mins(20), Span::as_duration),
    };
    let workspaces: Vec<PathBuf> = if args.all {
        user.workspaces.keys().map(PathBuf::from).collect()
    } else if args.workspace.is_empty() {
        vec![ctx.dir.clone()]
    } else {
        args.workspace.iter().map(|dir| ctx.dir.join(dir)).collect()
    };
    let Ok(_lock) = RepoLock::acquire(&cache.join("locks").join("sync.lock"), Duration::ZERO)
    else {
        return Ok(Report {
            out: "another odx sync is running; nothing to do\n".to_owned(),
            ..Report::default()
        });
    };
    let deadline = Instant::now() + max_duration;
    let mut record = SyncRecord {
        schema: 1,
        started: now(),
        ..SyncRecord::default()
    };
    let fetch = AggregateArgs {
        fetch_only: true,
        repo: None,
        prefer_mirror: true,
        keep_conflicts: false,
        force: false,
        bundle_dir: None,
        locked: false,
        // Current heads of pull requests, not only the locked commits.
        update: true,
    };
    let quiet = GlobalArgs {
        project_dir: None,
        config: None,
        jobs: settings.jobs.or(global.jobs).or(Some(2)),
        offline: global.offline,
        // Prefetching exists to bring the cache up to date.
        fresh: true,
        no_refresh: false,
        refresh_index: false,
        json: false,
        verbose: 0,
        quiet: true,
    };
    for dir in &workspaces {
        let path = dir.display().to_string();
        // Foreground commands go first.
        let (status, detail) = if Instant::now() >= deadline || !idle(&cache, deadline) {
            ("skipped", "max_duration reached".to_owned())
        } else {
            let workspace = Context {
                fs: ctx.fs,
                env: ctx.env,
                dir: dir.clone(),
                config: None,
            };
            sync_one(&fetch, &quiet, &workspace)
        };
        record.workspaces.push(SyncedWorkspace {
            path,
            status: status.to_owned(),
            detail,
        });
    }
    record.finished = now();
    let failures = keep(&record, &cache);
    let mut report = Report::default();
    if global.json {
        report.out = format!("{}\n", serde_json::to_string(&record).unwrap_or_default());
    } else {
        for workspace in &record.workspaces {
            let _ = writeln!(
                report.out,
                "{} {}: {}",
                workspace.status, workspace.path, workspace.detail
            );
        }
    }
    if failures > 0 {
        report.exit = Exit::Failure;
    }
    Ok(report)
}

/// Write `last_sync.json` and append to `sync.log`; how many workspaces
/// failed.
fn keep(record: &SyncRecord, cache: &Path) -> usize {
    let _ = record.write(&cache.join("last_sync.json"));
    let failures = record
        .workspaces
        .iter()
        .filter(|w| w.status == "failed")
        .count();
    let mut log = format!(
        "{} sync of {} workspaces, {failures} failed\n",
        date(SystemTime::now()),
        record.workspaces.len()
    );
    for workspace in &record.workspaces {
        let _ = writeln!(
            log,
            "  {} {}: {}",
            workspace.status, workspace.path, workspace.detail
        );
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cache.join("sync.log"))
        .and_then(|mut file| file.write_all(log.as_bytes()));
    failures
}

/// Apply `edit` to the user configuration file and write it back; its path
/// and what `edit` returned.
pub(crate) fn edit_user_config<T>(
    ctx: &Context<'_>,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> T,
) -> Result<(PathBuf, T), Report> {
    let file = user::config_dir(ctx.env)
        .map(|dir| dir.join("config.toml"))
        .ok_or_else(|| {
            failed(
                Exit::Usage,
                "no user configuration directory; set ODX_CONFIG_DIR",
            )
        })?;
    // Callers loaded the user configuration first, so it is TOML.
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&file)
        .unwrap_or_default()
        .parse()
        .unwrap_or_default();
    let result = edit(&mut doc);
    file.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&file, doc.to_string()))
        .map_err(|e| failed(Exit::Failure, format!("{}: {e}", file.display())))?;
    Ok((file, result))
}

/// The last sync, for people.
pub(crate) fn last_sync(cache: &Path) -> String {
    SyncRecord::read(&cache.join("last_sync.json")).map_or_else(
        || "never".to_owned(),
        |record| {
            let finished = UNIX_EPOCH + Duration::from_secs(record.finished);
            format!(
                "{} ({} workspaces)",
                date(finished),
                record.workspaces.len()
            )
        },
    )
}

/// Register the workspace of `prepared` for `odx sync --all` when the user
/// configuration asks for that (`[cache] register_workspaces`).
pub(crate) fn auto_register(prepared: &Prepared, ctx: &Context<'_>) {
    if prepared.register {
        let root = prepared.project.root.display().to_string();
        // Registration is a convenience; the command goes on without it.
        let _ = edit_user_config(ctx, |doc| {
            register(
                doc,
                &root,
                &date(SystemTime::now()),
                &prepared.project.config.odoo_version,
            );
        });
    }
}

/// The first line of `text`.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_owned()
}

/// Fetch one workspace and prefetch its wheels; the status and detail.
fn sync_one(
    fetch: &AggregateArgs,
    quiet: &GlobalArgs,
    workspace: &Context<'_>,
) -> (&'static str, String) {
    let report = run_aggregate(fetch, quiet, workspace);
    if let Some(error) = report.error {
        return ("failed", first_line(&error));
    }
    let fetched = first_line(&report.out);
    match prepare(quiet, workspace).and_then(|prepared| prefetch_wheels(workspace, &prepared)) {
        Ok(0) => ("synced", fetched),
        Ok(wheels) => ("synced", format!("{fetched}; {wheels} wheels cached")),
        Err(error) => (
            "failed",
            format!(
                "{fetched}; wheels: {}",
                first_line(&error.error.unwrap_or_default())
            ),
        ),
    }
}

/// `odx sync --register` / `--unregister`: edit `[workspaces]` in the user
/// configuration.
fn registry(add: bool, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let root = prepared.project.root.display().to_string();
    let (file, changed) = edit_user_config(ctx, |doc| {
        if add {
            register(
                doc,
                &root,
                &date(SystemTime::now()),
                &prepared.project.config.odoo_version,
            );
            true
        } else {
            unregister(doc, &root)
        }
    })?;
    let out = match (add, changed) {
        (true, _) => format!(
            "registered {root} for `odx sync --all` in {}\n",
            file.display()
        ),
        (false, true) => format!("unregistered {root}\n"),
        (false, false) => format!("{root} was not registered\n"),
    };
    Ok(Report {
        out,
        ..Report::default()
    })
}

/// The URL of the cache repository `hash`, or the hash when its URL file
/// is gone.
fn repo_url(cache: &Path, hash: &str) -> String {
    std::fs::read_to_string(cache.join("repos").join(format!("{hash}.url")))
        .map_or_else(|_| hash.to_owned(), |url| url.trim().to_owned())
}

/// `odx cache`.
pub(crate) fn run_cache(command: &CacheCommand, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    cache(command, global, ctx).unwrap_or_else(|report| report)
}

/// The repositories in the cache, and whether a registered workspace (or
/// the one here) uses them.
fn repositories(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    user: &UserConfig,
    cache: &Path,
) -> Vec<CachedRepo> {
    let mut wanted = Vec::new();
    let dirs = user
        .workspaces
        .keys()
        .map(PathBuf::from)
        .chain(std::iter::once(ctx.dir.clone()));
    for dir in dirs {
        let workspace = Context {
            fs: ctx.fs,
            env: ctx.env,
            dir,
            config: None,
        };
        let Ok(prepared) = prepare(global, &workspace) else {
            continue;
        };
        let config = &prepared.project.config;
        let planned = plan_config(config, &prepared.project.root, None).unwrap_or_default();
        wanted.extend(planned.iter().map(|repo| url_hash(&repo.url)));
        wanted.extend(
            prepared
                .lock
                .iter()
                .flat_map(|lock| lock.packages.values())
                .map(|package| url_hash(&package.url)),
        );
    }
    let repos = cache.join("repos");
    let mut found: Vec<CachedRepo> = std::fs::read_dir(&repos)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let hash = entry.file_name().to_string_lossy().into_owned();
            let used_at = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_secs());
            CachedRepo {
                referenced: wanted.contains(&hash),
                size: size(&entry.path()),
                used: used_at,
                hash,
            }
        })
        .collect();
    found.sort_by(|a, b| a.hash.cmp(&b.hash));
    found
}

/// `odx cache detach [<repo>]`.
fn detach_cached(
    repo: Option<&str>,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    repos: &[CachedRepo],
    cache: &Path,
    report: &mut Report,
) -> Result<(), Report> {
    let only = repo.map(|repo| {
        let named = prepare(global, ctx).ok().and_then(|prepared| {
            prepared
                .project
                .config
                .repositories
                .iter()
                .find(|candidate| candidate.name.as_deref() == Some(repo))
                .map(|candidate| candidate.url.clone())
        });
        url_hash(named.as_deref().unwrap_or(repo))
    });
    let git = SystemGit::new(Arc::new(OsRunner));
    let mut detached = 0;
    for found in repos
        .iter()
        .filter(|found| only.as_ref().is_none_or(|hash| *hash == found.hash))
    {
        let dir = cache.join("repos").join(&found.hash);
        let lock = cache.join("locks").join(format!("{}.lock", found.hash));
        let borrowed = detach(&git, &dir, &lock, Duration::from_secs(10))
            .map_err(|e| failed(Exit::Failure, format!("{}: {e}", dir.display())))?;
        if borrowed {
            detached += 1;
            let _ = writeln!(report.out, "detached {}", repo_url(cache, &found.hash));
        }
    }
    if detached == 0 {
        report
            .out
            .push_str("no cache repository borrows objects from a local clone\n");
    }
    Ok(())
}

fn cache(command: &CacheCommand, global: &GlobalArgs, ctx: &Context<'_>) -> Result<Report, Report> {
    let (user, cache) = shared(ctx)?;
    let mut report = Report::default();
    let repos = repositories(ctx, global, &user, &cache);
    let total = size(&cache);
    match command {
        CacheCommand::Info => {
            if global.json {
                let last = SyncRecord::read(&cache.join("last_sync.json"));
                report.out = format!(
                    "{}\n",
                    serde_json::json!({
                        "schema": 1,
                        "dir": cache.display().to_string(),
                        "size": total,
                        "repositories": repos.len(),
                        "unused": repos.iter().filter(|r| !r.referenced).count(),
                        "last_sync": last.as_ref().map(|record| record.finished),
                    })
                );
            } else {
                let _ = writeln!(report.out, "cache: {}", cache.display());
                let _ = writeln!(report.out, "size: {}", human(total));
                let _ = writeln!(
                    report.out,
                    "repositories: {} ({} unused by registered workspaces)",
                    repos.len(),
                    repos.iter().filter(|r| !r.referenced).count()
                );
                let _ = writeln!(report.out, "last sync: {}", last_sync(&cache));
            }
        }
        CacheCommand::Detach { repo } => {
            detach_cached(repo.as_deref(), global, ctx, &repos, &cache, &mut report)?;
        }
        CacheCommand::Gc { max_size, dry_run } => {
            let limit = match max_size
                .clone()
                .or_else(|| user.cache.as_ref().and_then(|c| c.max_size.clone()))
            {
                Some(text) => Some(parse_size(&text).ok_or_else(|| {
                    failed(
                        Exit::Usage,
                        format!("invalid size {text:?}: expected e.g. 20GB"),
                    )
                })?),
                None => None,
            };
            let mut freed = 0;
            for repo in to_collect(&repos, total, limit) {
                let dir = cache.join("repos").join(&repo.hash);
                let url_file = cache.join("repos").join(format!("{}.url", repo.hash));
                let url = repo_url(&cache, &repo.hash);
                let lock = cache.join("locks").join(format!("{}.lock", repo.hash));
                // A repository another odx is fetching into is left alone.
                let removed = *dry_run
                    || RepoLock::acquire(&lock, Duration::ZERO)
                        .is_ok_and(|_held| std::fs::remove_dir_all(&dir).is_ok());
                if removed {
                    if !*dry_run {
                        let _ = std::fs::remove_file(&url_file);
                        let _ = std::fs::remove_file(url_file.with_extension("fetched"));
                    }
                    freed += repo.size;
                    let _ = writeln!(
                        report.out,
                        "{} {url} ({})",
                        if *dry_run { "would remove" } else { "removed" },
                        human(repo.size)
                    );
                }
            }
            let _ = writeln!(
                report.out,
                "{} {}",
                if *dry_run { "would free" } else { "freed" },
                human(freed)
            );
        }
    }
    Ok(report)
}
