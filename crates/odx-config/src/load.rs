use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use odx_core::FileSystem;
use serde::Deserialize;

use crate::error::line_column;
use crate::schema::{MergeSource, OdxConfig, RefStyle};
use crate::{ConfigError, Problem};

/// A loaded project: where it lives and its validated `[tool.odx]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Directory containing the configuration file.
    pub root: PathBuf,
    /// The `pyproject.toml` that was read.
    pub file: PathBuf,
    /// The validated configuration.
    pub config: OdxConfig,
    /// The text of the file, as read.
    pub text: String,
    /// Non-fatal findings, such as values that were corrected.
    pub warnings: Vec<Problem>,
}

/// Where the configuration comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A `pyproject.toml` with `[tool.odx]`.
    Pyproject(Box<Project>),
    /// A git-aggregator `repos.yaml` (drop-in mode).
    ReposYaml {
        /// Directory containing the file.
        root: PathBuf,
        /// The file.
        file: PathBuf,
    },
}

impl Source {
    /// The project, when the source is a `pyproject.toml`.
    pub fn project(&self) -> Option<&Project> {
        match self {
            Self::Pyproject(project) => Some(project),
            Self::ReposYaml { .. } => None,
        }
    }
}

#[derive(Deserialize)]
struct PyProject {
    tool: Option<ToolTable>,
}

#[derive(Deserialize)]
struct ToolTable {
    odx: Option<OdxConfig>,
}

/// Parse and validate the `[tool.odx]` table of a `pyproject.toml`.
///
/// Returns `Ok(None)` when the file has no `[tool.odx]` table. Every other
/// table of the file is ignored.
///
/// ```
/// use odx_config::parse_pyproject;
/// use std::path::Path;
///
/// let text = "[project]\nname = \"x\"\n\n[tool.odx]\nodoo_version = \"17.0\"\n";
/// let project = parse_pyproject(text, Path::new("/p/pyproject.toml")).unwrap().unwrap();
/// assert_eq!(project.config.odoo_version, "17.0");
/// assert_eq!(project.root, Path::new("/p"));
///
/// let err = parse_pyproject("[tool.odx]\nodoo_versoin = \"17.0\"\n", Path::new("pyproject.toml"))
///     .unwrap_err();
/// assert!(err.to_string().starts_with("pyproject.toml:2:1: unknown field `odoo_versoin`"));
/// ```
///
/// # Errors
///
/// [`ConfigError::Syntax`] for invalid TOML, wrong types and unknown keys
/// inside `[tool.odx]`, with line and column; [`ConfigError::Invalid`] with
/// every consistency problem found.
pub fn parse_pyproject(text: &str, file: &Path) -> Result<Option<Project>, ConfigError> {
    let parsed: PyProject = toml::from_str(text).map_err(|error| {
        let (line, column) = line_column(text, error.span().map_or(0, |span| span.start));
        ConfigError::Syntax {
            file: file.to_path_buf(),
            line,
            column,
            message: error.message().to_owned(),
        }
    })?;
    let Some(mut config) = parsed.tool.and_then(|tool| tool.odx) else {
        return Ok(None);
    };
    let warnings = normalize(&mut config);
    let problems = validate(&config);
    if !problems.is_empty() {
        return Err(ConfigError::Invalid {
            file: file.to_path_buf(),
            problems,
        });
    }
    Ok(Some(Project {
        root: file.parent().map(Path::to_path_buf).unwrap_or_default(),
        file: file.to_path_buf(),
        config,
        text: text.to_owned(),
        warnings,
    }))
}

/// Find and load the project configuration.
///
/// With `explicit`, that file is used: a `.yaml`/`.yml` path is a
/// git-aggregator `repos.yaml`, anything else a `pyproject.toml`. Without
/// it, `pyproject.toml` is looked up in `start` and each parent; the first
/// one with a `[tool.odx]` table wins.
///
/// # Errors
///
/// [`ConfigError::NotFound`] when the search finds nothing,
/// [`ConfigError::NoOdxTable`] when an explicit `pyproject.toml` lacks the
/// table, and the errors of [`parse_pyproject`].
pub fn discover(
    fs: &dyn FileSystem,
    start: &Path,
    explicit: Option<&Path>,
) -> Result<Source, ConfigError> {
    if let Some(path) = explicit {
        let file = start.join(path);
        let is_yaml = file
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml");
        if is_yaml {
            return Ok(Source::ReposYaml {
                root: file.parent().map(Path::to_path_buf).unwrap_or_default(),
                file,
            });
        }
        return load_file(fs, &file)?
            .map(|project| Source::Pyproject(Box::new(project)))
            .ok_or(ConfigError::NoOdxTable { file });
    }
    find_pyproject(fs, start).map(|project| Source::Pyproject(Box::new(project)))
}

/// Find the first `pyproject.toml` with a `[tool.odx]` table in `start` or
/// its parents.
///
/// # Errors
///
/// [`ConfigError::NotFound`] when there is none, and the errors of
/// [`parse_pyproject`] for a file that cannot be read or is invalid.
pub fn find_pyproject(fs: &dyn FileSystem, start: &Path) -> Result<Project, ConfigError> {
    // `a/../b` is `b`: walking its ancestors must not pass through `a`.
    // Other paths stay as given, separators included.
    let mut normal = start.to_path_buf();
    if start.components().any(|part| part == Component::ParentDir) {
        normal.clear();
        for part in start.components() {
            if part == Component::ParentDir {
                normal.pop();
            } else {
                normal.push(part);
            }
        }
    }
    for dir in normal.ancestors() {
        let file = dir.join("pyproject.toml");
        if fs.exists(&file)
            && let Some(project) = load_file(fs, &file)?
        {
            return Ok(project);
        }
    }
    Err(ConfigError::NotFound {
        start: start.to_path_buf(),
    })
}

fn load_file(fs: &dyn FileSystem, file: &Path) -> Result<Option<Project>, ConfigError> {
    let bytes = fs.read(file).map_err(|source| ConfigError::Io {
        file: file.to_path_buf(),
        source,
    })?;
    let text = String::from_utf8_lossy(&bytes);
    parse_pyproject(&text, file)
}

/// Apply corrections that are safe to make, and report them.
fn normalize(config: &mut OdxConfig) -> Vec<Problem> {
    let mut warnings = Vec::new();
    if let Some(mirror) = &mut config.mirror
        && is_gitlab(&mirror.url)
        && mirror.ref_style == Some(RefStyle::Refs)
    {
        mirror.ref_style = Some(RefStyle::Tags);
        warnings.push(Problem {
            key: "tool.odx.mirror.ref_style".to_owned(),
            message: "GitLab allows deleting custom refs; using protected tags instead".to_owned(),
        });
    }
    warnings
}

fn is_gitlab(url: &str) -> bool {
    url.to_ascii_lowercase().contains("gitlab")
}

fn is_series(text: &str) -> bool {
    text.strip_suffix(".0")
        .is_some_and(|major| !major.is_empty() && major.bytes().all(|b| b.is_ascii_digit()))
}

fn is_sha(text: &str) -> bool {
    (7..=40).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

type Report<'a> = dyn FnMut(String, &str) + 'a;

/// Collect every consistency problem of a parsed configuration.
fn validate(config: &OdxConfig) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut report = |key: String, message: &str| {
        problems.push(Problem {
            key,
            message: message.to_owned(),
        });
    };
    if !is_series(&config.odoo_version) {
        report(
            "tool.odx.odoo_version".to_owned(),
            "must be an Odoo series such as \"17.0\"",
        );
    }
    validate_repositories(config, &mut report);
    validate_packages(config, &mut report);
    for (name, feature) in &config.features {
        if feature.modules.is_empty() {
            report(
                format!("tool.odx.features.{name}.modules"),
                "must name at least one module",
            );
        }
    }
    validate_environments(config, &mut report);
    problems
}

fn validate_repositories(config: &OdxConfig, report: &mut Report<'_>) {
    let mut names = BTreeSet::new();
    for (i, repo) in config.repositories.iter().enumerate() {
        let key = format!("tool.odx.repositories[{i}]");
        if let Some(name) = &repo.name
            && !names.insert(name.as_str())
        {
            report(format!("{key}.name"), "duplicate repository name");
        }
        for (j, merge) in repo.merges.iter().enumerate() {
            let key = format!("{key}.merges[{j}]");
            match merge.source() {
                Err(message) => report(key, &message),
                Ok(MergeSource::Pr(0)) => report(key, "`pr` must be a positive number"),
                Ok(MergeSource::Sha(sha)) if !is_sha(sha) => {
                    report(key, "`sha` must be 7 to 40 hexadecimal characters");
                }
                Ok(_) => {}
            }
        }
    }
}

fn validate_packages(config: &OdxConfig, report: &mut Report<'_>) {
    let groups: BTreeSet<&str> = config
        .groups
        .iter()
        .flat_map(|groups| groups.dirs.keys().map(String::as_str))
        .collect();
    if let Some(default) = config.groups.as_ref().and_then(|g| g.default.as_deref())
        && !groups.contains(default)
    {
        report(
            "tool.odx.groups.default".to_owned(),
            "names no configured group",
        );
    }
    for (dist, package) in &config.packages {
        let key = format!("tool.odx.packages.{dist}");
        if usize::from(package.repo.is_some()) + usize::from(package.url.is_some()) != 1 {
            report(key.clone(), "set exactly one of `repo` or `url`");
        }
        let refs = [
            package.pr.is_some(),
            package.branch.is_some(),
            package.tag.is_some(),
            package.rev.is_some(),
        ];
        if refs.into_iter().filter(|set| *set).count() != 1 {
            report(
                key.clone(),
                "set exactly one of `pr`, `branch`, `tag` or `rev`",
            );
        }
        if let Some(group) = &package.group
            && !groups.contains(group.as_str())
        {
            report(format!("{key}.group"), "names no configured group");
        }
    }
    if !config.packages.is_empty()
        && config
            .mirror
            .as_ref()
            .is_some_and(|m| m.ref_style != Some(RefStyle::Tags))
    {
        report(
            "tool.odx.mirror.ref_style".to_owned(),
            "must be \"tags\" when packages are configured",
        );
    }
}

fn validate_environments(config: &OdxConfig, report: &mut Report<'_>) {
    let Some(envs) = &config.environments else {
        return;
    };
    let defined = |name: &str| envs.env.is_empty() || envs.env.contains_key(name);
    for (i, edge) in envs.promotion.iter().flatten().enumerate() {
        for name in edge {
            if !defined(name) {
                report(
                    format!("tool.odx.environments.promotion[{i}]"),
                    "names an environment that is not configured",
                );
            }
        }
    }
    if let Some(env) = config
        .bot
        .as_ref()
        .and_then(|bot| bot.environment.as_deref())
        && !defined(env)
    {
        report(
            "tool.odx.bot.environment".to_owned(),
            "names an environment that is not configured",
        );
    }
    for (name, env) in &envs.env {
        if let Some(series) = &env.series
            && !is_series(series)
        {
            report(
                format!("tool.odx.environments.env.{name}.series"),
                "must be an Odoo series such as \"17.0\"",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::MemoryFs;
    use std::io::ErrorKind;

    fn problems(toml: &str) -> Vec<String> {
        let text = format!("[tool.odx]\nodoo_version = \"17.0\"\n{toml}");
        let err = parse_pyproject(&text, Path::new("pyproject.toml")).unwrap_err();
        err.problems().iter().map(ToString::to_string).collect()
    }

    #[test]
    fn full_documented_example_is_valid() {
        let text = include_str!("../tests/fixtures/full.toml");
        let project = parse_pyproject(text, Path::new("pyproject.toml"))
            .unwrap()
            .unwrap();
        assert_eq!(project.warnings, []);
    }

    #[test]
    fn file_without_tool_odx_is_none() {
        assert!(
            parse_pyproject("[project]\nname = \"x\"\n", Path::new("p.toml"))
                .unwrap()
                .is_none()
        );
        assert!(
            parse_pyproject("[tool.ruff]\n", Path::new("p.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn syntax_and_type_errors_have_positions() {
        let err = parse_pyproject("[tool.odx\n", Path::new("p.toml")).unwrap_err();
        assert!(err.to_string().starts_with("p.toml:1:"), "{err}");
        let err =
            parse_pyproject("[tool.odx]\nodoo_version = 17\n", Path::new("p.toml")).unwrap_err();
        assert!(err.to_string().starts_with("p.toml:2:"), "{err}");
    }

    #[test]
    fn unknown_keys_in_nested_tables_are_rejected() {
        let text = "[tool.odx]\nodoo_version = \"17.0\"\n\n[tool.odx.mirror]\nurl = \"u\"\npush_on_lok = true\n";
        let err = parse_pyproject(text, Path::new("p.toml")).unwrap_err();
        assert!(
            err.to_string()
                .contains("p.toml:6:1: unknown field `push_on_lok`"),
            "{err}"
        );
    }

    #[test]
    fn root_of_bare_file_name_is_empty() {
        let project = parse_pyproject(
            "[tool.odx]\nodoo_version = \"17.0\"\n",
            Path::new("pyproject.toml"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(project.root, PathBuf::new());
    }

    #[test]
    fn every_validation_problem_is_reported() {
        let found = problems(
            r#"
[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"
merges = [{ pr = 0 }, { sha = "xyz" }, {}, { pr = 1, sha = "abcdef1" }]

[[tool.odx.repositories]]
name = "pos"
url = "https://github.com/OCA/pos.git"

[tool.odx.groups]
default = "missing"
oca = "modules/oca"

[tool.odx.packages]
"odoo-addon-a" = { group = "nope" }
"odoo-addon-b" = { repo = "OCA/x", url = "u", pr = 1, rev = "r" }

[tool.odx.mirror]
url = "https://forgejo.example/vault.git"

[tool.odx.features.empty]
modules = []

[tool.odx.environments]
promotion = [["test", "nowhere"]]
[tool.odx.environments.env.test]
series = "seventeen"

[tool.odx.bot]
environment = "staging"
"#,
        );
        assert_eq!(
            found,
            [
                "tool.odx.repositories[0].merges[0]: `pr` must be a positive number",
                "tool.odx.repositories[0].merges[1]: `sha` must be 7 to 40 hexadecimal characters",
                "tool.odx.repositories[0].merges[2]: needs one of `pr`, `sha` or `branch`",
                "tool.odx.repositories[0].merges[3]: set only one of `pr`, `sha` or `branch`",
                "tool.odx.repositories[1].name: duplicate repository name",
                "tool.odx.groups.default: names no configured group",
                "tool.odx.packages.odoo-addon-a: set exactly one of `repo` or `url`",
                "tool.odx.packages.odoo-addon-a: set exactly one of `pr`, `branch`, `tag` or `rev`",
                "tool.odx.packages.odoo-addon-a.group: names no configured group",
                "tool.odx.packages.odoo-addon-b: set exactly one of `repo` or `url`",
                "tool.odx.packages.odoo-addon-b: set exactly one of `pr`, `branch`, `tag` or `rev`",
                "tool.odx.mirror.ref_style: must be \"tags\" when packages are configured",
                "tool.odx.features.empty.modules: must name at least one module",
                "tool.odx.environments.promotion[0]: names an environment that is not configured",
                "tool.odx.bot.environment: names an environment that is not configured",
                "tool.odx.environments.env.test.series: must be an Odoo series such as \"17.0\"",
            ]
        );
    }

    #[test]
    fn bad_series_is_reported() {
        let text = "[tool.odx]\nodoo_version = \"17\"\n";
        let err = parse_pyproject(text, Path::new("p.toml")).unwrap_err();
        assert!(
            err.to_string()
                .contains("odoo_version: must be an Odoo series")
        );
        for bad in [".0", "x.0", "17.1"] {
            assert!(!is_series(bad), "{bad}");
        }
    }

    #[test]
    fn environments_without_explicit_env_accept_any_name() {
        let text = "[tool.odx]\nodoo_version = \"17.0\"\n[tool.odx.environments]\npromotion = [[\"test\", \"prod\"]]\n[tool.odx.bot]\nenvironment = \"test\"\n";
        assert!(
            parse_pyproject(text, Path::new("p.toml"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn gitlab_mirror_is_switched_to_tags_with_a_warning() {
        let text = "[tool.odx]\nodoo_version = \"17.0\"\n[tool.odx.mirror]\nurl = \"https://GitLab.example.com/v.git\"\nref_style = \"refs\"\n";
        let project = parse_pyproject(text, Path::new("p.toml")).unwrap().unwrap();
        assert_eq!(
            project.config.mirror.unwrap().ref_style,
            Some(RefStyle::Tags)
        );
        assert_eq!(project.warnings.len(), 1);
        assert!(
            project.warnings[0]
                .to_string()
                .starts_with("tool.odx.mirror.ref_style: GitLab")
        );

        let text = "[tool.odx]\nodoo_version = \"17.0\"\n[tool.odx.mirror]\nurl = \"https://forgejo.example/v.git\"\nref_style = \"refs\"\n";
        let project = parse_pyproject(text, Path::new("p.toml")).unwrap().unwrap();
        assert_eq!(project.warnings, []);
    }

    fn fs_with(files: &[(&str, &str)]) -> MemoryFs {
        let fs = MemoryFs::default();
        for (path, text) in files {
            fs.write(Path::new(path), text.as_bytes()).unwrap();
        }
        fs
    }

    const ODX: &str = "[tool.odx]\nodoo_version = \"17.0\"\n";

    #[test]
    fn discover_walks_up_to_the_first_odx_table() {
        let fs = fs_with(&[
            ("/w/pyproject.toml", ODX),
            ("/w/addons/pyproject.toml", "[project]\nname = \"addon\"\n"),
        ]);
        let source = discover(&fs, Path::new("/w/addons/x"), None).unwrap();
        let project = source.project().unwrap();
        assert_eq!(project.root, Path::new("/w"));
        assert_eq!(project.file, Path::new("/w/pyproject.toml"));
    }

    #[test]
    fn discover_does_not_walk_through_parent_components() {
        let fs = fs_with(&[("/w/a/pyproject.toml", ODX)]);
        let err = discover(&fs, Path::new("/w/a/../b"), None).unwrap_err();
        assert!(err.to_string().starts_with("no pyproject.toml"), "{err}");
        let source = discover(&fs, Path::new("/w/b/../a/./x"), None).unwrap();
        assert_eq!(source.project().unwrap().root, Path::new("/w/a"));
    }

    #[test]
    fn discover_reports_not_found() {
        let fs = fs_with(&[("/w/pyproject.toml", "[project]\n")]);
        let err = discover(&fs, Path::new("/w"), None).unwrap_err();
        assert_eq!(
            err.to_string(),
            "no pyproject.toml with a [tool.odx] table in /w or its parents"
        );
    }

    #[test]
    fn explicit_files() {
        let fs = fs_with(&[("/w/pyproject.toml", ODX), ("/w/other.toml", "[project]\n")]);
        let source = discover(&fs, Path::new("/w"), Some(Path::new("pyproject.toml"))).unwrap();
        assert!(source.project().is_some());
        let err = discover(&fs, Path::new("/w"), Some(Path::new("other.toml"))).unwrap_err();
        let other = Path::new("/w").join("other.toml");
        assert_eq!(
            err.to_string(),
            format!("{}: no [tool.odx] table", other.display())
        );
        for name in ["repos.yaml", "repos.yml"] {
            let source = discover(&fs, Path::new("/w"), Some(Path::new(name))).unwrap();
            assert_eq!(source.project(), None);
            assert_eq!(
                source,
                Source::ReposYaml {
                    root: PathBuf::from("/w"),
                    file: PathBuf::from("/w").join(name)
                }
            );
        }
        assert_eq!(
            discover(&fs, Path::new(""), Some(Path::new("repos.yaml"))).unwrap(),
            Source::ReposYaml {
                root: PathBuf::new(),
                file: PathBuf::from("repos.yaml")
            }
        );
    }

    #[test]
    fn read_errors_name_the_file() {
        let fs = fs_with(&[("/w/pyproject.toml", ODX)]);
        fs.fail_next(ErrorKind::PermissionDenied);
        let err = discover(&fs, Path::new("/w"), None).unwrap_err();
        let file = Path::new("/w").join("pyproject.toml");
        assert!(
            err.to_string()
                .starts_with(&format!("{}: ", file.display()))
        );
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn explicit_read_error() {
        let fs = fs_with(&[("/w/pyproject.toml", ODX)]);
        fs.fail_next(ErrorKind::PermissionDenied);
        let err = discover(&fs, Path::new("/w"), Some(Path::new("pyproject.toml"))).unwrap_err();
        let file = Path::new("/w").join("pyproject.toml");
        assert!(
            err.to_string()
                .starts_with(&format!("{}: ", file.display()))
        );
    }

    #[test]
    fn non_utf8_is_read_lossily() {
        let fs = MemoryFs::default();
        fs.write(
            Path::new("/w/pyproject.toml"),
            b"# \xff\n[tool.odx]\nodoo_version = \"17.0\"\n",
        )
        .unwrap();
        assert!(discover(&fs, Path::new("/w"), None).is_ok());
    }
}
