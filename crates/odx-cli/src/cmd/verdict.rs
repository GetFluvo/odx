//! `odx verdict`: record, list, revoke and share decisions about upstream
//! changes. Organisation verdicts and project mutes live in the private
//! verdict repository (`[verdicts] repo` in the user configuration), cloned
//! into the cache; without one, mutes live in the project's
//! `.odx/mutes.toml`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use odx_aggregate::date;
use odx_config::Problem;
use odx_config::user::load_user_config;
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation};
use odx_forge::{CachedHttp, ForgeClient, RepoUrl, ReqwestHttp, Visibility};
use odx_git::{Forge, Source};
use odx_port::git::Repo;
use odx_verdicts::{
    Entry, Kind, Recheck, Revoked, Scope, Standing, append, confirm, mutes_file, read, read_all,
    revoke, update, verdict_file,
};
use sha2::{Digest as _, Sha256};

use super::aggregate::{Prepared, prepare};
use super::suggest::{Found, collect};
use super::{Report, output_of};
use crate::Context;
use crate::cli::{
    GlobalArgs, SuggestArgs, VerdictAddArgs, VerdictCommand, VerdictKind, VerdictScope,
};
use odx_port::analysis::Group;

/// Where verdicts and mutes are kept.
struct Store {
    /// The verdict repository's URL and its clone in the cache.
    repo: Option<(String, PathBuf)>,
    /// The project's own mutes file.
    local: PathBuf,
    /// The project id under `projects/`.
    project: String,
}

fn failed(exit: Exit, message: impl std::fmt::Display) -> Report {
    Report::error(exit, message.to_string())
}

/// Run git in `dir`.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    output_of(
        &Invocation::new("git")
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .arg("-C")
            .arg(dir.as_os_str())
            .args(args),
    )
}

/// `[tool.odx] project_id`, else the first twelve hex digits of the SHA-256
/// of the project repository's `origin` URL (or of its path).
pub(crate) fn project_id(prepared: &Prepared) -> String {
    let root = &prepared.project.root;
    prepared
        .project
        .config
        .project_id
        .clone()
        .unwrap_or_else(|| {
            let origin = git(root, &["remote", "get-url", "origin"])
                .unwrap_or_else(|_| root.display().to_string());
            let digest = Sha256::digest(origin.as_bytes());
            digest.iter().take(6).fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            })
        })
}

fn store(ctx: &Context<'_>, prepared: &Prepared) -> Store {
    // The project was prepared, so the user configuration is valid.
    let user = load_user_config(ctx.fs, ctx.env).unwrap_or_default();
    let repo = user
        .verdicts
        .and_then(|verdicts| verdicts.repo)
        .map(|url| (url, prepared.cache.root().join("verdicts")));
    Store {
        repo,
        local: prepared.project.root.join(".odx").join("mutes.toml"),
        project: project_id(prepared),
    }
}

/// Clone the verdict repository, or bring the clone up to date (online).
fn sync(store: &Store, offline: bool) -> Result<(), String> {
    let Some((url, dir)) = &store.repo else {
        return Ok(());
    };
    if !dir.join(".git").exists() {
        if offline {
            return Ok(());
        }
        let parent = dir.parent().unwrap_or(dir);
        // A parent that cannot be made fails the clone below.
        let _ = std::fs::create_dir_all(parent);
        return git(
            parent,
            &["clone", "--quiet", url, &dir.display().to_string()],
        )
        .map(drop);
    }
    // A repository without commits yet has nothing to pull.
    if offline || git(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_err() {
        return Ok(());
    }
    git(dir, &["pull", "--rebase", "--quiet"]).map(drop)
}

/// Every verdict and mute that applies to the project: the verdict
/// repository's (synced first, unless offline) and the project's own.
pub(crate) fn entries(
    ctx: &Context<'_>,
    prepared: &Prepared,
    global: &GlobalArgs,
    warnings: &mut Vec<Problem>,
) -> Vec<(PathBuf, Entry)> {
    let mut found = Vec::new();
    let store = store(ctx, prepared);
    let mut warn = |message: String| {
        warnings.push(Problem {
            key: "verdicts".to_owned(),
            message,
        });
    };
    if let Err(message) = sync(&store, global.offline) {
        warn(message);
    }
    if let Some((_, dir)) = &store.repo {
        match read_all(dir) {
            Ok(all) => found.extend(all.into_iter().filter(|(file, _)| {
                file.starts_with(dir.join("verdicts"))
                    || file.starts_with(dir.join("projects").join(&store.project))
            })),
            Err(message) => warn(message),
        }
    }
    match read(&store.local) {
        Ok(local) => found.extend(local.into_iter().map(|entry| (store.local.clone(), entry))),
        Err(message) => warn(message),
    }
    found
}

/// The verdict repository must be private: refused when the forge says
/// otherwise. A URL on no forge (a local or bare repository) is fine.
fn private(ctx: &Context<'_>, prepared: &Prepared, url: &str) -> Result<(), String> {
    let Some(repo) = RepoUrl::parse(url) else {
        return Ok(());
    };
    let sleep = std::thread::sleep;
    let visibility = ReqwestHttp::new()
        .map(|http| CachedHttp::new(http, prepared.cache.root().join("http")))
        .and_then(|http| {
            let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
            client
                .kind(&repo)
                .and_then(|kind| client.visibility(&repo, kind))
        })
        .map_err(|e| format!("cannot check that {url} is private: {e}"))?;
    if visibility == Visibility::Private {
        Ok(())
    } else {
        Err(format!(
            "{url} is not private; verdicts may name customers and must stay private"
        ))
    }
}

/// Commit everything in the verdict repository and push it, rebasing once
/// on a concurrent push.
fn publish(
    ctx: &Context<'_>,
    prepared: &Prepared,
    url: &str,
    dir: &Path,
    message: &str,
) -> Result<(), String> {
    private(ctx, prepared, url)?;
    // Recorded with the user's git identity, or odx's when there is none,
    // also for the commits a rebase rewrites.
    let name = git(dir, &["config", "--default", "odx", "user.name"]).unwrap_or_default();
    let email = author(dir);
    let name = format!("user.name={name}");
    let email = format!("user.email={email}");
    let as_user = |args: &[&str]| {
        let mut all = vec!["-c", name.as_str(), "-c", email.as_str()];
        all.extend_from_slice(args);
        git(dir, &all)
    };
    git(dir, &["add", "-A"])
        .and_then(|_| as_user(&["commit", "--quiet", "-m", message]))
        .and_then(|_| {
            git(dir, &["push", "--quiet", "origin", "HEAD"]).or_else(|_| {
                // Someone pushed meanwhile: rebase on their commits, once.
                git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
                    .and_then(|branch| as_user(&["pull", "--rebase", "--quiet", "origin", &branch]))
                    .and_then(|_| git(dir, &["push", "--quiet", "origin", "HEAD"]))
            })
        })
        .map(drop)
}

fn today() -> String {
    date(SystemTime::now())
}

/// Who records: git's `user.email`, else `unknown`.
fn author(dir: &Path) -> String {
    git(dir, &["config", "--default", "unknown", "user.email"]).unwrap_or_default()
}

const fn kind(kind: VerdictKind) -> Kind {
    match kind {
        VerdictKind::NotApplicable => Kind::NotApplicable,
        VerdictKind::NotNeeded => Kind::NotNeeded,
        VerdictKind::Superseded => Kind::Superseded,
        VerdictKind::AlreadyPresent => Kind::AlreadyPresent,
        VerdictKind::Declined => Kind::Declined,
        VerdictKind::NotWanted => Kind::NotWanted,
        VerdictKind::Deferred => Kind::Deferred,
    }
}

/// The tree of an entry's files on its target branch in the cache, as it
/// is now.
fn current_tree(prepared: &Prepared, entry: &Entry) -> Option<String> {
    let recheck = entry.recheck.as_ref()?;
    let runner = OsRunner;
    let target =
        Source::Branch(entry.target_series.clone()).local_ref(Forge::detect(&recheck.repo), None);
    Repo::new(&runner, prepared.cache.repo_dir(&recheck.repo))
        .tree_of(&target, &recheck.files)
        .ok()
}

/// `odx verdict`.
pub(crate) fn run_verdict(
    command: &VerdictCommand,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Report {
    verdict(command, global, ctx).unwrap_or_else(|report| report)
}

fn verdict(
    command: &VerdictCommand,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    let prepared = prepare(global, ctx)?;
    let store = store(ctx, &prepared);
    let mut report = Report::default();
    match command {
        VerdictCommand::Add(args) => add(args, global, ctx, &prepared, &store, &mut report)?,
        VerdictCommand::List {
            series,
            addon,
            stale,
        } => {
            let all = entries(ctx, &prepared, global, &mut report.warnings);
            report.out = list(&prepared, &all, series.as_deref(), addon.as_deref(), *stale);
        }
        VerdictCommand::Show { id } => {
            let all = entries(ctx, &prepared, global, &mut report.warnings);
            let (file, entry) = all
                .into_iter()
                .find(|(_, entry)| entry.id == *id)
                .ok_or_else(|| failed(Exit::Usage, format!("no verdict or mute {id}")))?;
            let _ = writeln!(report.out, "# {}", file.display());
            let mut doc = toml_edit::DocumentMut::new();
            let mut entries = toml_edit::ArrayOfTables::new();
            entries.push(entry.table());
            doc.insert("verdict", toml_edit::Item::ArrayOfTables(entries));
            report.out.push_str(&doc.to_string());
        }
        VerdictCommand::Revoke { id, reason } => {
            if reason.trim().is_empty() {
                return Err(failed(Exit::Usage, "revoking needs a reason"));
            }
            let revoked = Revoked {
                by: author(&prepared.project.root),
                date: today(),
                reason: reason.clone(),
            };
            change(ctx, global, &prepared, &store, id, "revoke", |table| {
                revoke(table, &revoked);
            })?;
            let _ = writeln!(report.out, "revoked {id}");
        }
        VerdictCommand::Confirm { id } => {
            let all = entries(ctx, &prepared, global, &mut report.warnings);
            let entry = all
                .into_iter()
                .map(|(_, entry)| entry)
                .find(|entry| entry.id == *id)
                .ok_or_else(|| failed(Exit::Usage, format!("no verdict or mute {id}")))?;
            let tree = current_tree(&prepared, &entry).ok_or_else(|| {
                failed(
                    Exit::Failure,
                    format!("the {} branch of {id}'s repository is not in the cache; run odx suggest first", entry.target_series),
                )
            })?;
            let today = today();
            change(ctx, global, &prepared, &store, id, "confirm", |table| {
                confirm(table, &tree, &today);
            })?;
            let _ = writeln!(
                report.out,
                "confirmed {id} against the current {} code",
                entry.target_series
            );
        }
        VerdictCommand::Upstream { .. } => {
            return Err(failed(
                Exit::Usage,
                "odx verdict upstream lands with odx port (roadmap phase 8)",
            ));
        }
        VerdictCommand::Sync => {
            sync(&store, global.offline).map_err(|e| failed(Exit::Failure, e))?;
            report.out = match &store.repo {
                Some((url, _)) => format!("verdict repository {url} is up to date\n"),
                None => format!(
                    "no verdict repository configured ([verdicts] repo); mutes live in {}\n",
                    store.local.display()
                ),
            };
        }
    }
    Ok(report)
}

/// `odx verdict list`: the entries for `series` and `addon`, only stale
/// ones with `stale`.
fn list(
    prepared: &Prepared,
    all: &[(PathBuf, Entry)],
    series: Option<&str>,
    addon: Option<&str>,
    stale: bool,
) -> String {
    let mut out = String::new();
    for (_, entry) in all {
        let is_stale = entry.standing(current_tree(prepared, entry).as_deref()) == Standing::Stale;
        let wanted = series.is_none_or(|s| s == entry.target_series)
            && addon.is_none_or(|a| a == entry.addon)
            && (!stale || is_stale);
        if !wanted {
            continue;
        }
        let _ = write!(
            out,
            "{} {} {} on {} {}: {}",
            entry.id,
            entry.kind.name(),
            entry.addon,
            entry.target_series,
            entry.change,
            entry.reason
        );
        if entry.revoked.is_some() {
            out.push_str(" [revoked]");
        } else if is_stale {
            out.push_str(" [stale: target code changed]");
        }
        out.push('\n');
    }
    out
}

/// Apply `edit` to entry `id` wherever it lives, and publish it.
fn change(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    prepared: &Prepared,
    store: &Store,
    id: &str,
    what: &str,
    edit: impl FnOnce(&mut toml_edit::Table),
) -> Result<(), Report> {
    sync(store, global.offline).map_err(|e| failed(Exit::Failure, e))?;
    let file = store
        .repo
        .iter()
        .flat_map(|(_, dir)| read_all(dir).unwrap_or_default())
        .map(|(file, _)| file)
        .chain(std::iter::once(store.local.clone()))
        .find(|file| {
            read(file)
                .unwrap_or_default()
                .iter()
                .any(|entry| entry.id == id)
        })
        .ok_or_else(|| failed(Exit::Usage, format!("no verdict or mute {id}")))?;
    update(&file, id, edit)
        .and_then(
            |_| match store.repo.as_ref().filter(|(_, dir)| file.starts_with(dir)) {
                Some((url, dir)) => {
                    publish(ctx, prepared, url, dir, &format!("verdict: {what} {id}"))
                }
                None => Ok(()),
            },
        )
        .map_err(|e| failed(Exit::Failure, e))?;
    Ok(())
}

/// `base`, or `base-2`, `base-3`... when one change gets several entries.
fn unique_id(store: &Store, base: &str) -> String {
    let mut taken: Vec<String> = store
        .repo
        .iter()
        .flat_map(|(_, dir)| read_all(dir).unwrap_or_default())
        .map(|(_, entry)| entry.id)
        .collect();
    taken.extend(
        read(&store.local)
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.id),
    );
    std::iter::once(base.to_owned())
        .chain((2..=taken.len() + 1).map(|n| format!("{base}-{n}")))
        .find(|id| !taken.contains(id))
        .unwrap_or_default()
}

/// The file an entry of `scope` goes to.
fn file_for(scope: Scope, store: &Store, target: &str, addon: &str) -> Result<PathBuf, Report> {
    match (scope, &store.repo) {
        (Scope::Org, Some((_, dir))) => Ok(verdict_file(dir, target, addon)),
        (Scope::Org, None) => Err(failed(
            Exit::Usage,
            "organisation verdicts need a verdict repository ([verdicts] repo in the user configuration); use --scope project",
        )),
        (Scope::Project | Scope::Env, Some((_, dir))) => Ok(mutes_file(dir, &store.project)),
        (Scope::Project | Scope::Env, None) => Ok(store.local.clone()),
    }
}

/// The suggestion `change` of `args`'s addon and series.
fn suggestion(
    args: &SuggestArgs,
    change: &str,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
    warnings: &mut Vec<Problem>,
) -> Result<(Found, Group), Report> {
    let addon = args.addon.clone().unwrap_or_default();
    let target = args.series.clone().unwrap_or_default();
    collect(args, global, ctx, prepared, warnings)?
        .into_iter()
        .find_map(|mut item| {
            let at = item.groups.iter().position(|group| group.key == change)?;
            let group = item.groups.swap_remove(at);
            Some((item, group))
        })
        .ok_or_else(|| {
            failed(
                Exit::Usage,
                format!("no suggestion {change} for {addon} on {target}; see odx suggest --addon {addon}"),
            )
        })
}

/// What makes a new entry of `kind` stale: the files `group` touches, as
/// they are on the target now; none for kinds (or a `--no-recheck`) that
/// never resurface.
fn recheck(
    kind: Kind,
    no_recheck: bool,
    item: &Found,
    group: &Group,
    addon: &str,
) -> Option<Recheck> {
    (kind.rechecks() && !no_recheck).then(|| {
        let files: Vec<String> = group
            .files
            .iter()
            .map(|file| format!("{addon}/{file}"))
            .collect();
        Recheck {
            on: "target-code-change".to_owned(),
            repo: item.target_url.clone(),
            target_tree: item.tree(&files).unwrap_or_default(),
            files,
        }
    })
}

/// `odx verdict add`.
fn add(
    args: &VerdictAddArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
    prepared: &Prepared,
    store: &Store,
    report: &mut Report,
) -> Result<(), Report> {
    let VerdictAddArgs {
        change,
        addon,
        target,
        from,
        kind: verdict_kind,
        reason,
        scope,
        environments,
        until,
        no_recheck,
    } = args;
    if reason.trim().is_empty() {
        return Err(failed(Exit::Usage, "a verdict needs a reason"));
    }
    let kind = kind(*verdict_kind);
    let scope = scope.map_or_else(
        || kind.default_scope(),
        |scope| match scope {
            VerdictScope::Organisation => Scope::Org,
            VerdictScope::Project => Scope::Project,
            VerdictScope::Environment => Scope::Env,
        },
    );
    if scope == Scope::Env && environments.is_empty() {
        return Err(failed(Exit::Usage, "an environment mute needs --env"));
    }
    let target = target
        .clone()
        .unwrap_or_else(|| prepared.project.config.odoo_version.clone());
    let args = SuggestArgs {
        env: None,
        series: Some(target.clone()),
        from: from.clone(),
        addon: Some(addon.clone()),
        show_muted: true,
    };
    let (item, group) = suggestion(&args, change, global, ctx, prepared, &mut report.warnings)?;
    let file = file_for(scope, store, &target, addon)?;
    let short: String = group
        .patch_ids
        .first()
        .unwrap_or(change)
        .chars()
        .take(8)
        .collect();
    let prefix = if scope == Scope::Org { "v" } else { "m" };
    let recheck = recheck(kind, *no_recheck, &item, &group, addon);
    let id = unique_id(store, &format!("{prefix}:{target}:{addon}:{short}"));
    let entry = Entry {
        id,
        change: change.clone(),
        patch_ids: group.patch_ids,
        source_series: item.source,
        target_series: target.clone(),
        addon: addon.clone(),
        kind,
        reason: reason.clone(),
        author: author(&prepared.project.root),
        date: today(),
        until: until.clone(),
        environments: (scope == Scope::Env).then(|| environments.clone()),
        recheck,
        revoked: None,
    };
    let message = format!(
        "verdict: {} ({}) for {addon} on {target}",
        entry.id,
        kind.name()
    );
    sync(store, global.offline)
        .and_then(|()| append(&file, &entry))
        .and_then(
            |()| match store.repo.as_ref().filter(|(_, dir)| file.starts_with(dir)) {
                Some((url, dir)) => publish(ctx, prepared, url, dir, &message),
                None => Ok(()),
            },
        )
        .map_err(|e| failed(Exit::Failure, e))?;
    let _ = writeln!(
        report.out,
        "recorded {} ({}) for {change} on {addon} {target}: {reason}",
        entry.id,
        kind.name()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_maps() {
        let all = [
            VerdictKind::NotApplicable,
            VerdictKind::NotNeeded,
            VerdictKind::Superseded,
            VerdictKind::AlreadyPresent,
            VerdictKind::Declined,
            VerdictKind::NotWanted,
            VerdictKind::Deferred,
        ];
        assert_eq!(
            all.map(|k| kind(k).name()),
            [
                "not-applicable",
                "not-needed",
                "superseded",
                "already-present",
                "declined",
                "not-wanted",
                "deferred"
            ]
        );
    }
}
