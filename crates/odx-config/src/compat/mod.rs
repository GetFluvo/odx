//! git-aggregator `repos.yaml` and doodba `addons.yaml`.
//!
//! [`parse_repos`] and [`parse_addons`] read the files, [`import`] converts
//! them to `[[tool.odx.repositories]]`, and [`export`] writes them back from
//! `[tool.odx]` for tools and container setups that still read them.

mod addons;
mod env;
mod export;
mod import;
mod ordered;
mod repos;

pub use addons::AddonsFile;
pub use env::{expand_env, parse_env_file};
pub use export::{Export, export};
pub use import::{Import, import};
pub use repos::{MergeSpec, RepoSpec, ReposFile, TargetSpec};

use crate::Problem;

/// Errors from reading `repos.yaml` or `addons.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    /// The YAML is malformed or has an unexpected shape.
    #[error("{0}")]
    Yaml(String),
    /// Entries are inconsistent.
    #[error("invalid repos.yaml:\n{}", list(.0))]
    Invalid(Vec<Problem>),
}

fn list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parse a git-aggregator `repos.yaml`.
///
/// # Errors
///
/// [`CompatError::Yaml`] for malformed YAML or unknown keys,
/// [`CompatError::Invalid`] listing every malformed merge, unknown remote
/// and missing base branch.
pub fn parse_repos(text: &str) -> Result<ReposFile, CompatError> {
    let (file, problems) = repos::parse(text).map_err(CompatError::Yaml)?;
    if problems.is_empty() {
        Ok(file)
    } else {
        Err(CompatError::Invalid(problems))
    }
}

/// Parse a doodba `addons.yaml` (all documents).
///
/// Use [`parse_addons_with_warnings`] to also learn which documents were
/// skipped.
///
/// # Errors
///
/// [`CompatError::Yaml`] for malformed YAML.
pub fn parse_addons(text: &str) -> Result<AddonsFile, CompatError> {
    parse_addons_with_warnings(text).map(|(file, _)| file)
}

/// Parse a doodba `addons.yaml` and report skipped documents and entries.
///
/// # Errors
///
/// [`CompatError::Yaml`] for malformed YAML.
pub fn parse_addons_with_warnings(text: &str) -> Result<(AddonsFile, Vec<Problem>), CompatError> {
    addons::parse(text).map_err(CompatError::Yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render() {
        let err = parse_repos("./x:\n  remotes:\n    o: u\n  merges: [bad, o 17.0]\n").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid repos.yaml:\n  - ./x: merges[0]: expected \"<remote> <ref>\""
        );
        let yaml = parse_repos("[").unwrap_err();
        assert_eq!(yaml, CompatError::Yaml(yaml.to_string()));
        assert!(parse_addons("[").is_err());
    }
}
