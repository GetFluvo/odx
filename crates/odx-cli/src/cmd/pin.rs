//! Temporary pins of unreleased dependencies, the OCA way: while a
//! dependency only exists as an open migration pull request upstream, a
//! line `odoo-addon-<name> @ git+<repository>.git@refs/pull/<n>/head#subdirectory=<name>`
//! in `test-requirements.txt`, in a commit of its own titled
//! `[DON'T MERGE] test-requirements.txt`, and a `Depends on:` checklist in
//! the pull request.

use std::path::PathBuf;

use odx_core::Exit;
use odx_core::system::OsRunner;

use super::deps::{Lookup, Waiting};
use super::propose::{default_remote, git};
use super::{Report, output_of};
use crate::Context;
use crate::cli::GlobalArgs;

/// The commit a pin lives in, as OCA asks.
pub(crate) const SUBJECT: &str = "[DON'T MERGE] test-requirements.txt";

/// What the pull request should say about the temporary red check.
pub(crate) const RED_CHECK: &str = "OCA CI's check for unreleased dependencies stays red while the `[DON'T MERGE] test-requirements.txt` commit is there; that is expected: drop the commit once the pull requests it names are merged and released";

/// The migration branch checked out, as `odx migrate addon` made it.
#[derive(Debug, Clone)]
pub(crate) struct MigrationBranch {
    pub(crate) name: String,
    pub(crate) to: String,
    pub(crate) addon: String,
    pub(crate) from: String,
    pub(crate) remote: String,
}

/// The series before `series`: `18.0` for `19.0`.
pub(crate) fn previous(series: &str) -> Option<String> {
    let (major, _) = series.split_once('.')?;
    let major: u32 = major.parse().ok()?;
    major.checked_sub(1).map(|before| format!("{before}.0"))
}

/// The migration branch checked out: `<to>-mig-<addon>`, with the source
/// series and remote `odx migrate addon` recorded.
pub(crate) fn current(ctx: &Context<'_>) -> Result<MigrationBranch, Report> {
    let name = output_of(&git(ctx).args(["branch", "--show-current"])).unwrap_or_default();
    let Some((to, addon)) = name.split_once("-mig-") else {
        return Err(Report::error(
            Exit::Usage,
            format!("{name:?} is no migration branch (`<series>-mig-<addon>`)"),
        ));
    };
    let config = |key: &str| {
        output_of(&git(ctx).args(["config", "--get", &format!("branch.{name}.{key}")])).ok()
    };
    let from = config("odxMigrateFrom")
        .or_else(|| previous(to))
        .unwrap_or_default();
    let remote = config("odxMigrateRemote").unwrap_or_else(|| default_remote(ctx));
    Ok(MigrationBranch {
        to: to.to_owned(),
        addon: addon.to_owned(),
        from,
        remote,
        name: name.clone(),
    })
}

/// The top of the worktree (`current` made sure there is one).
fn root(ctx: &Context<'_>) -> PathBuf {
    PathBuf::from(output_of(&git(ctx).args(["rev-parse", "--show-toplevel"])).unwrap_or_default())
}

/// The dependencies of the branch's addon that its target series does not
/// have yet.
pub(crate) fn waiting(
    ctx: &Context<'_>,
    global: &GlobalArgs,
    branch: &MigrationBranch,
) -> Vec<Waiting> {
    let manifest = root(ctx).join(&branch.addon).join("__manifest__.py");
    let depends = ctx
        .fs
        .read(&manifest)
        .ok()
        .and_then(|bytes| {
            odx_analyze::manifest::parse_manifest(&String::from_utf8_lossy(&bytes)).ok()
        })
        .map(|manifest| manifest.depends)
        .unwrap_or_default();
    let runner = OsRunner;
    let repo = odx_port::git::Repo::new(&runner, &ctx.dir);
    let target = format!("refs/remotes/{}/{}", branch.remote, branch.to);
    let available = |addon: &str| {
        repo.show(&target, &format!("{addon}/__manifest__.py"))
            .is_some()
    };
    Lookup::new(ctx, global.offline).waiting(&depends, &branch.from, &branch.to, &available)
}

/// The requirement line pinning `waiting` to its migration pull request.
fn requirement(waiting: &Waiting) -> Option<String> {
    let pr = waiting.pull_request.as_ref()?;
    let addon = &waiting.addon;
    Some(format!(
        "odoo-addon-{addon} @ git+{}/{}.git@{}#subdirectory={addon}",
        pr.origin, pr.repository, pr.head
    ))
}

/// Pin every dependency in `waiting` that has an open migration pull
/// request in `test-requirements.txt`, in a commit of its own; the
/// requirement lines written, none when there is nothing to pin.
pub(crate) fn pin(ctx: &Context<'_>, waiting: &[Waiting]) -> Result<Vec<String>, Report> {
    let lines: Vec<(String, String)> = waiting
        .iter()
        .filter_map(|waiting| requirement(waiting).map(|line| (waiting.addon.clone(), line)))
        .collect();
    if lines.is_empty() {
        return Ok(Vec::new());
    }
    let file = root(ctx).join("test-requirements.txt");
    let before = std::fs::read_to_string(&file).unwrap_or_default();
    let mut text = before.clone();
    for (addon, line) in &lines {
        let prefixes = [
            format!("odoo-addon-{addon} "),
            format!("odoo-addon-{} ", addon.replace('_', "-")),
        ];
        let kept: Vec<&str> = text
            .lines()
            .filter(|existing| !prefixes.iter().any(|prefix| existing.starts_with(prefix)))
            .collect();
        text = kept.join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
        text.push('\n');
    }
    if text == before {
        // Pinned already, to the same pull requests.
        return Ok(lines.into_iter().map(|(_, line)| line).collect());
    }
    std::fs::write(&file, &text)
        .map_err(|e| Report::error(Exit::Failure, format!("{}: {e}", file.display())))?;
    let path = file.to_string_lossy().into_owned();
    super::run_checked(&git(ctx).args(["add", "--", &path]), "git add")
        .and_then(|()| {
            super::run_checked(
                &git(ctx).args([
                    "commit",
                    "--quiet",
                    "--no-verify",
                    "-m",
                    SUBJECT,
                    "--",
                    &path,
                ]),
                "git commit",
            )
        })
        .map_err(|e| Report::error(Exit::Failure, e))?;
    Ok(lines.into_iter().map(|(_, line)| line).collect())
}

/// The pages of the pull requests `test-requirements.txt` pins, for a
/// `Depends on:` checklist.
pub(crate) fn depends_on(ctx: &Context<'_>) -> Vec<String> {
    let text = std::fs::read_to_string(root(ctx).join("test-requirements.txt")).unwrap_or_default();
    text.lines().filter_map(pinned_page).collect()
}

/// The pull request page a requirement line pins, if it pins one.
fn pinned_page(line: &str) -> Option<String> {
    let (_, url) = line.split_once("git+")?;
    let (repository, rest) = url.split_once(".git@")?;
    let reference = rest.split('#').next().unwrap_or(rest);
    if let Some(number) = reference
        .strip_prefix("refs/pull/")
        .and_then(|rest| rest.strip_suffix("/head"))
    {
        return Some(format!("{repository}/pull/{number}"));
    }
    let number = reference
        .strip_prefix("refs/merge-requests/")?
        .strip_suffix("/head")?;
    Some(format!("{repository}/-/merge_requests/{number}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_pages() {
        assert_eq!(
            pinned_page(
                "odoo-addon-x @ git+https://github.com/OCA/s.git@refs/pull/3/head#subdirectory=x"
            )
            .as_deref(),
            Some("https://github.com/OCA/s/pull/3")
        );
        assert_eq!(
            pinned_page("odoo-addon-x @ git+https://gitlab.com/g/s.git@refs/merge-requests/4/head")
                .as_deref(),
            Some("https://gitlab.com/g/s/-/merge_requests/4")
        );
        for line in [
            "requests",
            "odoo-addon-x @ git+https://github.com/OCA/s",
            "odoo-addon-x @ git+https://github.com/OCA/s.git@abc123",
            "odoo-addon-x @ git+https://github.com/OCA/s.git@refs/merge-requests/4",
        ] {
            assert_eq!(pinned_page(line), None, "{line}");
        }
        let waiting = Waiting {
            addon: "dep_x".to_owned(),
            repository: Some("https://github.com/OCA/s".to_owned()),
            pull_request: Some(super::super::deps::Migration {
                repository: "OCA/s".to_owned(),
                origin: "https://github.com".to_owned(),
                number: 3,
                url: "https://github.com/OCA/s/pull/3".to_owned(),
                head: "refs/pull/3/head".to_owned(),
            }),
        };
        assert_eq!(
            requirement(&waiting).as_deref(),
            Some(
                "odoo-addon-dep_x @ git+https://github.com/OCA/s.git@refs/pull/3/head#subdirectory=dep_x"
            )
        );
        assert_eq!(previous("0.0"), None);
    }
}
