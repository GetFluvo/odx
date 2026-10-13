use std::fmt::Write as _;
use std::path::Path;

use crate::{GitError, SystemGit};

/// Where vaulted commits are kept on the mirror.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultRefs {
    /// `<prefix>/<sha>`, e.g. `refs/odx/<sha>`.
    Prefix(String),
    /// `refs/tags/odx/<sha>`, for forges that only protect tags and for
    /// tools that only fetch tags.
    Tags,
}

impl VaultRefs {
    /// The ref that keeps commit `sha`: its name is the commit id and
    /// nothing else, so it never moves and identifies no project.
    ///
    /// ```
    /// use odx_git::VaultRefs;
    ///
    /// assert_eq!(VaultRefs::Prefix("refs/odx".into()).name("abc"), "refs/odx/abc");
    /// assert_eq!(VaultRefs::Tags.name("abc"), "refs/tags/odx/abc");
    /// ```
    pub fn name(&self, sha: &str) -> String {
        match self {
            Self::Prefix(prefix) => format!("{}/{sha}", prefix.trim_end_matches('/')),
            Self::Tags => format!("refs/tags/odx/{sha}"),
        }
    }
}

/// Push `shas` from the cache repository `repo` to `mirror`, one ref per
/// commit, in one atomic push; returns the ref of each commit, in order.
/// A ref that is already on the mirror with the same commit is fine.
///
/// # Errors
///
/// When the mirror cannot be reached or refuses a ref.
pub fn vault(
    git: &SystemGit,
    repo: &Path,
    mirror: &str,
    shas: &[String],
    refs: &VaultRefs,
) -> Result<Vec<String>, GitError> {
    let names: Vec<String> = shas.iter().map(|sha| refs.name(sha)).collect();
    let mut refspecs: Vec<String> = shas
        .iter()
        .zip(&names)
        .map(|(sha, name)| format!("{sha}:{name}"))
        .collect();
    refspecs.sort();
    refspecs.dedup();
    if !refspecs.is_empty() {
        git.push(repo, mirror, &refspecs)?;
    }
    Ok(names)
}

/// Write `file`, a git bundle of the commits `shas` of the cache repository.
///
/// Each commit is kept under `refs/odx/locked/<sha>` in `repo`, so
/// collecting the cache never drops a locked commit. `odx aggregate
/// --bundle-dir` fetches the commits from such a bundle by id.
///
/// # Errors
///
/// When git cannot create the refs or the bundle.
pub fn bundle(git: &SystemGit, repo: &Path, shas: &[String], file: &Path) -> Result<(), GitError> {
    let mut unique: Vec<&String> = shas.iter().collect();
    unique.sort();
    unique.dedup();
    let names: Vec<String> = unique
        .iter()
        .map(|sha| format!("refs/odx/locked/{sha}"))
        .collect();
    let updates = unique.iter().fold(String::new(), |mut text, sha| {
        let _ = writeln!(text, "update refs/odx/locked/{sha} {sha}");
        text
    });
    git.update_refs(repo, &updates)?;
    file.parent().map_or(Ok(()), std::fs::create_dir_all)?;
    git.bundle(repo, file, &names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::system::OsRunner;
    use std::process::Command;
    use std::sync::Arc;

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
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[test]
    fn commits_are_pushed_under_their_own_id() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "a"]);
        let a = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "b"]);
        let b = git(&repo, &["rev-parse", "HEAD"]);
        let mirror = tmp.path().join("vault.git");
        git(
            tmp.path(),
            &["init", "--quiet", "--bare", mirror.to_str().unwrap()],
        );
        let system = SystemGit::new(Arc::new(OsRunner));
        let url = mirror.to_str().unwrap();

        let refs = vault(
            &system,
            &repo,
            url,
            &[a.clone(), b.clone(), a.clone()],
            &VaultRefs::Prefix("refs/odx/".into()),
        )
        .unwrap();
        assert_eq!(
            refs,
            [
                format!("refs/odx/{a}"),
                format!("refs/odx/{b}"),
                format!("refs/odx/{a}")
            ]
        );
        assert_eq!(git(&mirror, &["rev-parse", &format!("refs/odx/{b}")]), b);
        // Again: nothing moves, nothing fails.
        vault(
            &system,
            &repo,
            url,
            std::slice::from_ref(&a),
            &VaultRefs::Prefix("refs/odx".into()),
        )
        .unwrap();
        let tags = vault(
            &system,
            &repo,
            url,
            std::slice::from_ref(&b),
            &VaultRefs::Tags,
        )
        .unwrap();
        assert_eq!(git(&mirror, &["rev-parse", &tags[0]]), b);
        assert_eq!(
            vault(&system, &repo, url, &[], &VaultRefs::Tags).unwrap(),
            Vec::<String>::new()
        );

        // A ref name that already holds another commit is refused.
        git(&mirror, &["update-ref", &format!("refs/odx/{a}"), &b]);
        let err = vault(
            &system,
            &repo,
            url,
            &[a],
            &VaultRefs::Prefix("refs/odx".into()),
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("git push failed"), "{err}");
    }

    #[test]
    fn bundles_hold_commits_under_their_own_id() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "--quiet"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "a"]);
        let a = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["commit", "--quiet", "--allow-empty", "-m", "b"]);
        let b = git(&repo, &["rev-parse", "HEAD"]);
        let system = SystemGit::new(Arc::new(OsRunner));
        let bundled = tmp.path().join("bundles").join("repo.bundle");
        bundle(&system, &repo, &[a.clone(), b, a.clone()], &bundled).unwrap();
        let back = tmp.path().join("back.git");
        git(
            tmp.path(),
            &["init", "--quiet", "--bare", back.to_str().unwrap()],
        );
        git(
            &back,
            &[
                "fetch",
                "--quiet",
                bundled.to_str().unwrap(),
                &format!("{a}:refs/a"),
            ],
        );
        assert_eq!(git(&back, &["rev-parse", "refs/a"]), a);
        assert!(bundle(&system, &repo, &["1".repeat(40)], &bundled).is_err());
        assert!(
            bundle(
                &system,
                &repo,
                std::slice::from_ref(&a),
                &tmp.path().join("x").join("..").join("repo")
            )
            .is_err()
        );
        let blocked = tmp.path().join("file");
        std::fs::write(&blocked, "").unwrap();
        assert!(
            bundle(
                &system,
                &repo,
                std::slice::from_ref(&a),
                &blocked.join("repo.bundle")
            )
            .is_err()
        );
    }
}
