//! `odx lock`: resolve, guard and vault every source, and write `odx.lock`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use odx_aggregate::{
    HeadDecision, Planned, PlannedSource, classify, date, decide, lock_changes, locked_entry,
    package_entries, package_fetch_plans, pin, plan_config, rewrite_packages, run_fetches,
};
use odx_config::deps::{normalize_name, read_uv_git_sources};
use odx_config::edit::PyprojectEdit;
use odx_config::lock::{
    CiState as LockCi, LockedRepository, Lockfile, MergeStatus, OdooPin, RedClassification,
};
use odx_config::packages::{PackageSpec, package_specs};
use odx_config::schema::OnHeadChange;
use odx_config::{Source, discover};
use odx_core::system::OsRunner;
use odx_core::{Exit, Invocation, ProcessRunner, repo_key};
use odx_forge::{CachedHttp, CiState, ForgeClient, ForgeKind, RepoUrl, ReqwestHttp};
use odx_git::{Cache, Fetched, GitBackend, Merger, SystemGit, bundle, url_hash};
use toml_edit::DocumentMut;

use super::super::Report;
use super::super::edit::save;
use super::super::progress::{Reporter, to_stderr};
use super::{
    Prepared, Summary, build, lock_entries, merger_and_options, next_lock, prepare, uv_program,
    vault_lock, write_lock,
};
use crate::Context;
use crate::cli::{GlobalArgs, LockArgs};

/// `odx lock [--repo NAME] [--check | --update]`.
pub(crate) fn run_lock(args: &LockArgs, global: &GlobalArgs, ctx: &Context<'_>) -> Report {
    lock(args, global, ctx).unwrap_or_else(|report| report)
}

pub(super) fn lock(
    args: &LockArgs,
    global: &GlobalArgs,
    ctx: &Context<'_>,
) -> Result<Report, Report> {
    if args.auto_update {
        return super::super::autoupdate::auto_update(global, ctx);
    }
    if args.migrate {
        return migrate(ctx);
    }
    let mut prepared = prepare(global, ctx)?;
    // A lock records what upstream has now, never what was cached earlier.
    prepared.max_age = None;
    super::super::sync::auto_register(&prepared, ctx);
    let mut planned = plan_config(
        &prepared.project.config,
        &prepared.project.root,
        args.repo.as_deref(),
    )
    .map_err(|e| Report::error(Exit::Usage, e.to_string()))?;
    if !args.update {
        for repo in &mut planned {
            pin(repo, prepared.locked());
        }
    }
    // Pinned repositories keep their entries; the others are built in
    // scratch working trees inside the cache, never in the project.
    let mut planned: Vec<Planned> = planned.into_iter().filter(|repo| !repo.pinned).collect();
    in_scratch_dirs(&prepared.cache, &mut planned);
    let (merger, options) = merger_and_options(&prepared.project.config, ctx, false, true)?;
    let mut warnings = prepared.project.warnings.clone();
    let kept = if args.update && prepared.lock.is_some() {
        // Resolve the heads first: the guard rail decides which to adopt.
        let resolved = build(&prepared, &planned, &options, None, true, None);
        let fetched: Vec<(&str, &[Fetched])> = resolved
            .outcomes
            .iter()
            .filter_map(|o| {
                o.result
                    .as_ref()
                    .ok()
                    .map(|f| (o.url.as_str(), f.as_slice()))
            })
            .collect();
        guard(
            ctx,
            &prepared,
            &mut planned,
            &fetched,
            &args.follow,
            &merger,
            &mut warnings,
        )
    } else {
        Vec::new()
    };
    let built = build(&prepared, &planned, &options, Some(&merger), true, None);
    let mut summary = Summary::new(&built, &planned, &prepared.project.root, true);
    let (mut entries, failures) = lock_entries(&planned, &built, &merger);
    record_kept(&mut entries, &kept, prepared.locked());
    summary.kept = kept;
    summary.failures.extend(failures);
    let mut lock = next_lock(&prepared, entries, args.repo.as_deref());
    // Packages and the Odoo core belong to the whole project, not to a
    // repository: `--repo` leaves them as they are.
    let packages = if args.repo.is_none() {
        Some(lock_packages(
            &prepared,
            args.update,
            &mut lock,
            &mut summary,
            &mut warnings,
        )?)
    } else {
        None
    };
    let previous = prepared.lock_or_default();
    summary.changes = lock_changes(&previous, &lock);
    let outdated = *previous != lock;
    if args.check {
        if outdated {
            summary.failures.push(format!(
                "{} is not up to date; run `odx lock`",
                prepared.lock_path.display()
            ));
        }
    } else {
        if !args.no_push {
            vault_lock(ctx, &prepared, &mut lock, &mut summary, &mut warnings)?;
            if let Some((specs, doc)) = packages {
                point_packages_at_the_vault(
                    ctx,
                    &prepared,
                    args.no_uv,
                    &specs,
                    doc,
                    &lock,
                    &mut summary,
                )?;
            }
        }
        if let Some(dir) = &args.bundle {
            bundle_lock(&prepared, &lock, &ctx.dir.join(dir), &mut summary);
        }
        if *previous != lock {
            write_lock(ctx, &prepared, &lock)?;
        }
        warnings.extend(super::super::glue::hint(&prepared));
    }
    Ok(summary.report(global.json, false, warnings))
}

/// `odx lock --migrate`: rewrite `odx.lock` in the current format.
pub(super) fn migrate(ctx: &Context<'_>) -> Result<Report, Report> {
    let project = match discover(ctx.fs, &ctx.dir, ctx.config) {
        Ok(Source::Pyproject(project)) => project,
        Ok(Source::ReposYaml { file, .. }) => {
            return Err(Report::error(
                Exit::Usage,
                format!("{}: repos.yaml projects have no odx.lock", file.display()),
            ));
        }
        Err(e) => return Err(Report::error(Exit::Usage, e.to_string())),
    };
    let path = project.root.join(project.config.lockfile());
    let text = ctx
        .fs
        .read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", path.display())))?;
    let lock = Lockfile::migrate(&text)
        .map_err(|e| Report::error(Exit::Usage, format!("{}: {e}", path.display())))?;
    let rendered = lock.render();
    if rendered == text {
        return Ok(Report {
            out: format!("{} is already in the current format\n", path.display()),
            ..Report::default()
        });
    }
    ctx.fs
        .write(&path, rendered.as_bytes())
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", path.display())))?;
    Ok(Report {
        out: format!("rewrote {} in the current format\n", path.display()),
        ..Report::default()
    })
}

/// `odx lock --bundle`: one git bundle per repository URL with every
/// locked commit of it, named after the URL's hash (no project or client
/// name).
pub(super) fn bundle_lock(prepared: &Prepared, lock: &Lockfile, dir: &Path, summary: &mut Summary) {
    let mut by_url: Vec<(String, Vec<String>)> = Vec::new();
    let commits = lock
        .repositories
        .iter()
        .flat_map(|entry| {
            std::iter::once(&entry.base_sha)
                .chain(entry.merges.iter().map(|m| &m.sha))
                .map(move |sha| (&entry.url, sha))
        })
        .chain(
            lock.packages
                .values()
                .map(|package| (&package.url, &package.sha)),
        );
    for (url, sha) in commits {
        let key = repo_key(url);
        match by_url.iter_mut().find(|(known, _)| repo_key(known) == key) {
            Some((_, shas)) if !shas.contains(sha) => shas.push(sha.clone()),
            Some(_) => {}
            None => by_url.push((url.clone(), vec![sha.clone()])),
        }
    }
    let git = SystemGit::new(Arc::new(OsRunner));
    for (url, shas) in by_url {
        let file = dir.join(format!("{}.bundle", url_hash(&url)));
        match bundle(&git, &prepared.cache.repo_dir(&url), &shas, &file) {
            Ok(()) => summary
                .bundled
                .push((url, shas.len(), file.display().to_string())),
            Err(e) => summary
                .failures
                .push(format!("{url}: bundling failed: {e}")),
        }
    }
}

/// Build in scratch working trees inside the cache, never in the project.
pub(super) fn in_scratch_dirs(cache: &Cache, planned: &mut [Planned]) {
    for repo in planned.iter_mut() {
        let name: String = repo
            .name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        repo.target_dir = cache
            .root()
            .join("locks")
            .join(format!("{}-{name}", url_hash(&repo.url)));
    }
}

/// Resolve the project's git packages (pinned by `odx.lock` unless
/// `update`) and its Odoo core source into `lock`; returns the packages and
/// the parsed `pyproject.toml` for rewriting.
pub(super) fn lock_packages(
    prepared: &Prepared,
    update: bool,
    lock: &mut Lockfile,
    summary: &mut Summary,
    warnings: &mut Vec<odx_config::Problem>,
) -> Result<(Vec<PackageSpec>, PyprojectEdit), Report> {
    // The text was read and validated when the project was loaded.
    let edit = PyprojectEdit::parse(&prepared.project.text).unwrap_or_default();
    let doc = edit.document();
    let previous = prepared.lock_or_default();
    let none = BTreeMap::new();
    let pinned = if update { &none } else { &previous.packages };
    let (specs, problems) = package_specs(doc, &prepared.project.config, &previous.packages);
    warnings.extend(problems);
    let vault = prepared
        .project
        .config
        .mirror
        .as_ref()
        .map(|m| m.url.as_str());
    let mut plans = package_fetch_plans(&specs, pinned, vault);
    for plan in &mut plans {
        plan.clone = prepared.clones.find(&plan.url).cloned();
        plan.offline = prepared.offline;
    }
    let backend: Arc<dyn GitBackend> = Arc::new(SystemGit::new(Arc::new(OsRunner)));
    let reporter = Reporter::new(prepared.verbosity, &[], &to_stderr);
    let outcomes = run_fetches(
        &prepared.cache,
        &backend,
        plans,
        prepared.limits,
        &|event| {
            reporter.hear(event);
        },
    );
    let fetched: Vec<(&str, &[Fetched])> = outcomes
        .iter()
        .filter_map(|o| {
            o.result
                .as_ref()
                .ok()
                .map(|f| (o.url.as_str(), f.as_slice()))
        })
        .collect();
    summary.failures.extend(outcomes.iter().filter_map(|o| {
        o.result
            .as_ref()
            .err()
            .map(|e| format!("packages from {}: {e}", o.url))
    }));
    let (entries, missing) = package_entries(&specs, &fetched, pinned);
    summary
        .failures
        .extend(missing.iter().map(|dist| format!("{dist}: not resolved")));
    lock.packages = entries;
    lock.odoo = odoo_pin(doc, &previous, update, prepared.offline)
        .map_err(|e| Report::error(Exit::Failure, e))?;
    Ok((specs, edit))
}

/// The Odoo core revision from `[tool.uv.sources] odoo` (git), asked of
/// the remote without downloading anything; the locked one while the
/// source is unchanged and not `update`.
pub(super) fn odoo_pin(
    doc: &DocumentMut,
    previous: &Lockfile,
    update: bool,
    offline: bool,
) -> Result<Option<OdooPin>, String> {
    let Some(source) = read_uv_git_sources(doc)
        .into_iter()
        .find(|source| normalize_name(&source.name) == "odoo")
    else {
        return Ok(None);
    };
    let kept = previous
        .odoo
        .clone()
        .filter(|pin| !update && repo_key(&pin.url) == repo_key(&source.git));
    if kept.is_some() {
        return Ok(kept);
    }
    let reference = source
        .rev
        .clone()
        .or_else(|| source.tag.clone())
        .or_else(|| source.branch.clone())
        .unwrap_or_else(|| "HEAD".to_owned());
    let full =
        matches!(reference.len(), 40 | 64) && reference.bytes().all(|b| b.is_ascii_hexdigit());
    let rev = if full {
        reference
    } else if offline {
        return Err(format!(
            "odoo: {reference} of {} is asked of the remote; run without --offline",
            source.git
        ));
    } else {
        SystemGit::new(Arc::new(OsRunner))
            .ls_remote(&source.git, &reference)
            .map_err(|e| format!("odoo: {e}"))?
            .ok_or_else(|| format!("odoo: {} has no {reference}", source.git))?
    };
    Ok(Some(OdooPin {
        url: source.git,
        rev,
    }))
}

/// Rewrite vaulted packages to their vault tags in `pyproject.toml`, then
/// update `uv.lock` (`ODX_UV` names the executable) unless `no_uv`.
pub(super) fn point_packages_at_the_vault(
    ctx: &Context<'_>,
    prepared: &Prepared,
    no_uv: bool,
    specs: &[PackageSpec],
    mut edit: PyprojectEdit,
    lock: &Lockfile,
    summary: &mut Summary,
) -> Result<(), Report> {
    let Some(mirror) = prepared.project.config.mirror.as_ref() else {
        return Ok(());
    };
    let before = edit.to_string();
    let file = &prepared.project.file;
    let failures = rewrite_packages(edit.document_mut(), specs, &lock.packages, &mirror.url);
    summary.failures.extend(
        failures
            .iter()
            .map(|dist| format!("{dist}: cannot point it at the vault in {}", file.display())),
    );
    if edit.to_string() == before {
        return Ok(());
    }
    save(ctx, file, &edit)?;
    summary.rewritten = true;
    if !no_uv {
        let uv = uv_program(ctx.env);
        let invocation = Invocation::new(&uv)
            .arg("lock")
            .current_dir(&prepared.project.root);
        let output = OsRunner.run(&invocation);
        let ok = output.as_ref().is_ok_and(odx_core::Output::success);
        if !ok {
            let detail = output.map_or_else(
                |e| e.to_string(),
                |out| String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            );
            summary.failures.push(format!("{uv} lock failed: {detail}"));
        }
    }
    Ok(())
}

/// A pull request the guard rail kept at its locked commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Kept {
    pub(super) repository: String,
    pub(super) step: usize,
    pub(super) label: String,
    pub(super) locked: String,
    pub(super) head: String,
    pub(super) ci: Option<LockCi>,
    pub(super) classification: Option<RedClassification>,
}

pub(super) fn sha_of(commits: &[Fetched], wanted: &PlannedSource) -> Option<String> {
    commits
        .iter()
        .find(|f| f.remote == wanted.remote && f.source == wanted.source)
        .map(|f| f.sha.clone())
}

/// The force-push guard rail: for every pull request whose head moved since
/// it was locked, apply its `on_head_change` policy to the CI state of the
/// new head; a kept pull request goes back to its locked commit. Every
/// other source is set to the commit it resolved to, so building needs no
/// network.
pub(super) fn guard(
    ctx: &Context<'_>,
    prepared: &Prepared,
    planned: &mut [Planned],
    fetched: &[(&str, &[Fetched])],
    follow: &[String],
    merger: &Merger,
    warnings: &mut Vec<odx_config::Problem>,
) -> Vec<Kept> {
    let mut kept_prs = Vec::new();
    // Repositories whose fetch failed are left as planned; building reports
    // them.
    let resolved = planned.iter_mut().filter_map(|repo| {
        let key = repo_key(&repo.url);
        fetched
            .iter()
            .find(|(url, _)| repo_key(url) == key)
            .and_then(|(_, commits)| sha_of(commits, &repo.base).map(|base| (repo, *commits, base)))
    });
    for (repo, commits, base) in resolved {
        let entry = locked_entry(repo, prepared.locked());
        let cache = prepared.cache.repo_dir(&repo.url);
        let locked_merges = entry.map_or(&[][..], |e| e.merges.as_slice());
        repo.base.source = odx_git::Source::Commit(base.clone());
        let steps = repo
            .steps
            .iter_mut()
            .enumerate()
            .filter_map(|(i, step)| sha_of(commits, &step.from).map(|head| (i, step, head)));
        for (i, step, head) in steps {
            step.from.source = odx_git::Source::Commit(head.clone());
            let Some((pr, locked)) = step
                .lock
                .pr
                .zip(locked_merges.get(i))
                .filter(|(_, locked)| locked.sha != head)
            else {
                continue;
            };
            let follows = follow.iter().any(|f| *f == format!("{}#{pr}", repo.name));
            let asks_ci = !follows && step.on_head_change == OnHeadChange::FollowIfGreen;
            let report =
                asks_ci.then(|| head_ci(ctx, prepared.cache.root(), &repo.url, &head, warnings));
            let ci = report.as_ref().map(|(ci, _, _)| *ci);
            if decide(step.on_head_change, ci.unwrap_or(LockCi::Unknown), follows)
                == HeadDecision::Adopt
            {
                continue;
            }
            let classification = report
                .filter(|(ci, _, statuses_only)| *ci == LockCi::Red && !*statuses_only)
                .map(|(_, failing, _)| {
                    let (base_ci, base_failing, _) = head_ci(
                        ctx,
                        prepared.cache.root(),
                        &repo.url,
                        &base,
                        &mut Vec::new(),
                    );
                    classify(
                        &failing,
                        (base_ci == LockCi::Red).then_some(base_failing.as_slice()),
                        &merger
                            .changed_dirs(&cache, &base, &head)
                            .unwrap_or_default(),
                        &merger.dirs(&cache, &base).unwrap_or_default(),
                    )
                })
                .or_else(|| (ci == Some(LockCi::Red)).then_some(RedClassification::Red));
            step.from.source = odx_git::Source::Commit(locked.sha.clone());
            kept_prs.push(Kept {
                repository: repo.name.clone(),
                step: i,
                label: step.label.clone(),
                locked: locked.sha.clone(),
                head,
                ci,
                classification,
            });
        }
    }
    kept_prs
}

/// The CI state of `sha` in the repository at `url`, its failing checks,
/// and whether the forge only has commit statuses (no check names worth
/// classifying). A URL that is no forge has no CI; an API that cannot be
/// read counts as pending, so the locked commit is kept.
pub(super) fn head_ci(
    ctx: &Context<'_>,
    cache: &Path,
    url: &str,
    sha: &str,
    warnings: &mut Vec<odx_config::Problem>,
) -> (LockCi, Vec<String>, bool) {
    let Some(repo) = RepoUrl::parse(url) else {
        return (LockCi::Unknown, Vec::new(), false);
    };
    let answer = ReqwestHttp::new().and_then(|http| {
        let http = CachedHttp::new(http, cache.join("http"));
        let sleep = std::thread::sleep;
        let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
        let kind = client.kind(&repo)?;
        client.ci(&repo, kind, sha).map(|report| (report, kind))
    });
    match answer {
        Ok((report, kind)) => (
            match report.state {
                CiState::Green => LockCi::Green,
                CiState::Red => LockCi::Red,
                CiState::Pending => LockCi::Pending,
                CiState::Unknown => LockCi::Unknown,
            },
            report.failing,
            kind == ForgeKind::Gitea,
        ),
        Err(e) => {
            warnings.push(odx_config::Problem {
                key: url.to_owned(),
                message: format!(
                    "cannot read the CI state of {sha}: {e}; keeping the locked commit"
                ),
            });
            (LockCi::Pending, Vec::new(), false)
        }
    }
}

/// Mark the kept pull requests in the new lock entries: status `pinned`,
/// the head that was not adopted, its CI state, and since when (kept from
/// an earlier pin).
pub(super) fn record_kept(
    entries: &mut [LockedRepository],
    kept: &[Kept],
    locked: &[LockedRepository],
) {
    let today = date(std::time::SystemTime::now());
    for keep in kept {
        let since = locked
            .iter()
            .filter(|entry| entry.name == keep.repository)
            .filter_map(|entry| entry.merges.get(keep.step))
            .find_map(|merge| merge.pinned_since.clone())
            .unwrap_or_else(|| today.clone());
        for merge in entries
            .iter_mut()
            .filter(|entry| entry.name == keep.repository)
            .filter_map(|entry| entry.merges.get_mut(keep.step))
        {
            merge.status = MergeStatus::Pinned;
            merge.upstream_head = Some(keep.head.clone());
            merge.upstream_head_ci = keep.ci;
            merge.upstream_head_classification = keep.classification;
            merge.pinned_since = Some(since.clone());
        }
    }
}
