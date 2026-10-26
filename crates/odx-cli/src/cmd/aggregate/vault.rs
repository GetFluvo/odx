//! The vault: pushing locked commits and pinned heads to `[tool.odx.mirror]`.

use std::path::Path;
use std::sync::Arc;

use odx_config::lock::{LockedRepository, Lockfile};
use odx_config::schema::RefStyle;
use odx_core::Exit;
use odx_core::system::OsRunner;
use odx_forge::{CachedHttp, ForgeClient, ForgeError, ForgeKind, RepoUrl, ReqwestHttp, Visibility};
use odx_git::{SystemGit, VaultRefs, vault};

use super::super::Report;
use super::{Prepared, Summary};
use crate::Context;

/// Push every locked commit that is not vaulted yet to `[tool.odx.mirror]`
/// and record its ref, after checking that the vault is not public.
pub(super) fn vault_lock(
    ctx: &Context<'_>,
    prepared: &Prepared,
    lock: &mut Lockfile,
    summary: &mut Summary,
    warnings: &mut Vec<odx_config::Problem>,
) -> Result<(), Report> {
    let Some(mirror) = prepared
        .project
        .config
        .mirror
        .as_ref()
        .filter(|mirror| mirror.push_on_lock != Some(false))
    else {
        return Ok(());
    };
    let unvaulted = |entry: &LockedRepository| entry.merges.iter().any(|m| m.mirror_ref.is_none());
    if !lock.repositories.iter().any(unvaulted)
        && lock
            .packages
            .values()
            .all(|package| package.mirror_ref.is_some())
    {
        return Ok(());
    }
    let refs = vault_refs(
        ctx,
        prepared.cache.root(),
        mirror,
        warnings,
        &mut summary.notes,
    )?;
    let git = SystemGit::new(Arc::new(OsRunner));
    for entry in lock
        .repositories
        .iter_mut()
        .filter(|entry| unvaulted(entry))
    {
        let shas: Vec<String> = std::iter::once(entry.base_sha.clone())
            .chain(entry.merges.iter().map(|m| m.sha.clone()))
            .collect();
        match vault(
            &git,
            &prepared.cache.repo_dir(&entry.url),
            &mirror.url,
            &shas,
            &refs,
        ) {
            Ok(names) => {
                for (merge, name) in entry.merges.iter_mut().zip(names.into_iter().skip(1)) {
                    merge.mirror_ref = Some(name);
                }
                summary.vaulted.push((entry.name.clone(), shas.len()));
            }
            Err(e) => summary
                .failures
                .push(format!("{}: vaulting failed: {e}", entry.name)),
        }
    }
    // uv fetches tags and branches only: packages are always vaulted as tags.
    for (dist, package) in lock
        .packages
        .iter_mut()
        .filter(|(_, package)| package.mirror_ref.is_none())
    {
        let shas = [package.sha.clone()];
        match vault(
            &git,
            &prepared.cache.repo_dir(&package.url),
            &mirror.url,
            &shas,
            &VaultRefs::Tags,
        ) {
            Ok(names) => {
                package.mirror_ref = names.into_iter().next();
                summary.vaulted.push((dist.clone(), 1));
            }
            Err(e) => summary
                .failures
                .push(format!("{dist}: vaulting failed: {e}")),
        }
    }
    Ok(())
}

/// Push the newer upstream heads `prepared`'s odx.lock records for the
/// pull requests it keeps pinned to `mirror`, so that it serves them too
/// (`[tool.odx.bot].vault_heads`): how many were vaulted, of how many.
pub(crate) fn vault_heads(
    ctx: &Context<'_>,
    prepared: &Prepared,
    mirror: &odx_config::schema::Mirror,
) -> (usize, usize) {
    let heads: Vec<(&LockedRepository, Vec<String>)> = prepared
        .locked()
        .iter()
        .map(|entry| {
            let shas = entry
                .merges
                .iter()
                .filter_map(|merge| merge.upstream_head.clone())
                .collect();
            (entry, shas)
        })
        .filter(|(_, shas): &(_, Vec<String>)| !shas.is_empty())
        .collect();
    let of = heads.iter().map(|(_, shas)| shas.len()).sum();
    // A vault that is refused vaults nothing, as one that fails.
    let refs = vault_refs(
        ctx,
        prepared.cache.root(),
        mirror,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    let git = SystemGit::new(Arc::new(OsRunner));
    let done = refs.map_or(0, |refs| {
        heads
            .iter()
            .filter(|(entry, shas)| {
                vault(
                    &git,
                    &prepared.cache.repo_dir(&entry.url),
                    &mirror.url,
                    shas,
                    &refs,
                )
                .is_ok()
            })
            .map(|(_, shas)| shas.len())
            .sum()
    });
    (done, of)
}

/// The refs vaulted commits get, after checking the vault's visibility on
/// its forge: a public vault is refused unless `allow_public = true`, and
/// GitLab vaults use tags (GitLab protects tags, not custom refs).
pub(super) fn vault_refs(
    ctx: &Context<'_>,
    cache: &Path,
    mirror: &odx_config::schema::Mirror,
    warnings: &mut Vec<odx_config::Problem>,
    notes: &mut Vec<String>,
) -> Result<VaultRefs, Report> {
    let refuse = |message: String| {
        Report::error(
            Exit::Failure,
            format!("{message}; run `odx lock --no-push` to lock without vaulting"),
        )
    };
    let auto_create = mirror.auto_create == Some(true);
    let protected_tags = match RepoUrl::parse(&mirror.url) {
        Some(repo) => {
            let (kind, visibility, created) = ReqwestHttp::new()
                .and_then(|http| {
                    let http = CachedHttp::new(http, cache.join("http"));
                    let sleep = std::thread::sleep;
                    let client = ForgeClient::new(&http, ctx.env, &OsRunner, &sleep);
                    let kind = client.kind(&repo)?;
                    match client.visibility(&repo, kind) {
                        Err(ForgeError::NotVisible(_)) if auto_create => {
                            client.create_private(&repo, kind)?;
                            client.protect_vault_tags(&repo, kind)?;
                            Ok((kind, Visibility::Private, true))
                        }
                        other => other.map(|visibility| (kind, visibility, false)),
                    }
                })
                .map_err(|e| refuse(e.to_string()))?;
            if created {
                notes.push(format!(
                    "created the private vault {} on {} with protected odx/* tags",
                    mirror.url,
                    kind.name()
                ));
            }
            if visibility == Visibility::Public && mirror.allow_public != Some(true) {
                return Err(refuse(format!(
                    "{} is public on {}; odx vaults commits only in private repositories (allow_public = true overrides)",
                    mirror.url,
                    kind.name()
                )));
            }
            // GitLab protects tags, not custom refs; an auto-created vault
            // protects tags everywhere.
            kind == ForgeKind::Gitlab || auto_create
        }
        None => false,
    };
    Ok(match (protected_tags, mirror.ref_style) {
        (true, Some(RefStyle::Refs)) => {
            warnings.push(odx_config::Problem {
                key: "tool.odx.mirror.ref_style".to_owned(),
                message:
                    "the vault protects tags, not custom refs; vaulting under refs/tags/odx instead"
                        .to_owned(),
            });
            VaultRefs::Tags
        }
        (true, _) | (false, Some(RefStyle::Tags)) => VaultRefs::Tags,
        (false, _) => VaultRefs::Prefix(
            mirror
                .ref_prefix
                .clone()
                .unwrap_or_else(|| "refs/odx".to_owned()),
        ),
    })
}
