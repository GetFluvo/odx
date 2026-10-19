//! An environment's branch as git has it: its `pyproject.toml`,
//! lockfiles and project addons, read without checking anything out.

use std::path::{Path, PathBuf};

use odx_config::OdxConfig;
use odx_config::environments::{Environment, Model};
use odx_config::lock::Lockfile;
use odx_config::uvlock::UvLock;
use odx_core::{Exit, Invocation};

use super::{Report, output_of};

/// A project addon at a ref.
#[derive(Debug, Clone)]
pub(crate) struct AddonAt {
    /// Technical name.
    pub(crate) name: String,
    /// The folder's tree.
    pub(crate) tree: String,
    /// The manifest's text.
    pub(crate) manifest: String,
}

/// An environment's branch.
#[derive(Debug)]
pub(crate) struct Checkout {
    /// The environment.
    pub(crate) env: Environment,
    /// The git ref it was read from.
    pub(crate) from: String,
    /// Its configuration.
    pub(crate) config: OdxConfig,
    /// Its own addons.
    pub(crate) addons: Vec<AddonAt>,
    /// Its `uv.lock`, text and parsed.
    pub(crate) uv: Option<(String, UvLock)>,
    /// Its `odx.lock`, text and parsed.
    pub(crate) lock: Option<(String, Lockfile)>,
}

/// The environment `arg` names, or the one whose branch it is (with that
/// branch).
pub(crate) fn environment(model: &Model, arg: &str) -> Result<Environment, Report> {
    if let Some(env) = model.get(arg) {
        if !env.is_literal() {
            return Err(Report::error(
                Exit::Usage,
                format!(
                    "the branch of {arg} is a pattern ({}): name one of its branches instead",
                    env.branch
                ),
            ));
        }
        return Ok(env.clone());
    }
    let env = model.of_branch(arg, None).ok_or_else(|| {
        Report::error(
            Exit::Usage,
            format!("{arg} is no environment, nor a branch of one (`odx env list` shows them)"),
        )
    })?;
    Ok(Environment {
        branch: arg.to_owned(),
        ..env.clone()
    })
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    output_of(&Invocation::new("git").arg("-C").arg(root).args(args))
}

/// The git ref of `env`'s branch: fetched from its own repository, else
/// `<remote>/<branch>`, else the local branch.
pub(crate) fn reference(root: &Path, env: &Environment, remote: &str) -> Result<String, Report> {
    if let Some(repo) = &env.repo {
        let local = format!("refs/odx/env/{}", env.name);
        let spec = format!("+refs/heads/{}:{local}", env.branch);
        git(root, &["fetch", "--quiet", "--no-tags", repo, &spec]).map_err(|e| {
            Report::error(
                Exit::Failure,
                format!("cannot fetch {} of {repo}: {e}", env.branch),
            )
        })?;
        return Ok(local);
    }
    [
        format!("refs/remotes/{remote}/{}", env.branch),
        format!("refs/heads/{}", env.branch),
    ]
    .into_iter()
    .find(|candidate| {
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{candidate}^{{commit}}"),
            ],
        )
        .is_ok()
    })
    .ok_or_else(|| {
        Report::error(
            Exit::Failure,
            format!(
                "{}: neither {remote}/{branch} nor {branch} is here; fetch it first",
                env.name,
                branch = env.branch
            ),
        )
    })
}

/// `path` (relative to the project) at `reference`, none when it is not
/// there.
pub(crate) fn show(root: &Path, reference: &str, path: &str) -> Option<String> {
    git(root, &["show", &format!("{reference}:./{path}")]).ok()
}

/// `env`'s branch, through git.
pub(crate) fn read(root: &Path, env: Environment, remote: &str) -> Result<Checkout, Report> {
    let from = reference(root, &env, remote)?;
    let invalid = |what: &str, e: String| {
        Report::error(Exit::Failure, format!("{what} of {}: {e}", env.name))
    };
    let pyproject = show(root, &from, "pyproject.toml")
        .ok_or_else(|| invalid("pyproject.toml", format!("missing on {from}")))?;
    let config = odx_config::parse_pyproject(&pyproject, Path::new("pyproject.toml"))
        .map_err(|e| invalid("pyproject.toml", e.to_string()))?
        .ok_or_else(|| invalid("pyproject.toml", "no [tool.odx]".to_owned()))?
        .config;
    let addons = addons(root, &from, &config.project_addon_dirs());
    let uv = match show(root, &from, "uv.lock") {
        Some(text) => {
            let parsed = UvLock::parse(&text).map_err(|e| invalid("uv.lock", e))?;
            Some((text, parsed))
        }
        None => None,
    };
    let lockfile = config.lockfile().to_string_lossy().replace('\\', "/");
    let lock = match show(root, &from, &lockfile) {
        Some(text) => {
            let parsed = Lockfile::parse(&text).map_err(|e| invalid(&lockfile, e.to_string()))?;
            Some((text, parsed))
        }
        None => None,
    };
    Ok(Checkout {
        env,
        from,
        config,
        addons,
        uv,
        lock,
    })
}

/// The project's own addons at `reference`: each folder directly in one of
/// `dirs` with a manifest, pinned by its tree.
fn addons(root: &Path, reference: &str, dirs: &[PathBuf]) -> Vec<AddonAt> {
    let mut units = Vec::new();
    for dir in dirs {
        let dir = dir.to_string_lossy().replace('\\', "/");
        let dir = dir.trim_end_matches('/');
        let prefix = if dir == "." || dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let listed = git(
            root,
            &[
                "ls-tree",
                "-d",
                "--format=%(objectname) %(path)",
                reference,
                &format!("./{prefix}"),
            ],
        )
        .unwrap_or_default();
        units.extend(
            listed
                .lines()
                .filter_map(|line| line.split_once(' '))
                .filter_map(|(tree, path)| {
                    let manifest = ["__manifest__.py", "__openerp__.py"]
                        .iter()
                        .find_map(|name| show(root, reference, &format!("{path}/{name}")))?;
                    Some(AddonAt {
                        name: path.rsplit('/').next().unwrap_or(path).to_owned(),
                        tree: tree.to_owned(),
                        manifest,
                    })
                }),
        );
    }
    units
}
