//! The environment model: `[tool.odx.environments]` with its preset applied.
//!
//! A preset supplies environments, their branches and roles, and the
//! promotion edges; `env.<name>` overrides a preset environment key by key
//! or adds one (its branch defaults to its name), and `promotion` replaces
//! the preset's edges. Without a `preset`, a table that declares
//! environments is `explicit`, else `major-branches`.

use std::collections::BTreeMap;

use crate::schema::{Environments, OdxConfig, Preset, Role};

/// An environment with every default applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// Its name.
    pub name: String,
    /// The branch, or a pattern where `*` matches anything (`staging/*`).
    pub branch: String,
    /// Its role.
    pub role: Option<Role>,
    /// The series it runs.
    pub series: String,
    /// A separate repository; none is the project's.
    pub repo: Option<String>,
    /// Never chosen as the default source of a promotion.
    pub disposable: bool,
}

impl Environment {
    /// Whether `branch` is this environment's.
    #[must_use]
    pub fn matches(&self, branch: &str) -> bool {
        glob(&self.branch, branch)
    }

    /// Whether the branch is one name rather than a pattern.
    #[must_use]
    pub fn is_literal(&self) -> bool {
        !self.branch.contains('*')
    }
}

/// The project's environments and promotion edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The preset in effect.
    pub preset: Preset,
    /// Environments by name.
    pub environments: BTreeMap<String, Environment>,
    /// Allowed promotions, `[from, to]`.
    pub promotion: Vec<[String; 2]>,
    /// Files that differ per environment and are never promoted.
    pub branch_specific: Vec<String>,
}

/// A preset environment: name, branch pattern, role, disposable.
type Defaults = (&'static str, &'static str, Role);

fn preset_environments(preset: Preset) -> &'static [Defaults] {
    match preset {
        Preset::MajorBranches => &[
            ("test", "{major}-test", Role::Test),
            ("uat", "{major}-uat", Role::Uat),
            ("prod", "{major}-prod", Role::Prod),
        ],
        Preset::SeriesBranches => &[
            ("test", "{series}-staging", Role::Test),
            ("prod", "{series}", Role::Prod),
        ],
        Preset::MainProduction => &[
            ("test", "main", Role::Test),
            ("prod", "production", Role::Prod),
        ],
        Preset::OdooSh => &[
            ("dev", "*", Role::Dev),
            ("uat", "staging/*", Role::Uat),
            ("prod", "production", Role::Prod),
        ],
        Preset::Explicit => &[],
    }
}

fn preset_promotion(preset: Preset) -> &'static [[&'static str; 2]] {
    match preset {
        Preset::MajorBranches => &[
            ["test", "uat"],
            ["test", "prod"],
            ["uat", "prod"],
            ["prod", "test"],
        ],
        Preset::SeriesBranches | Preset::MainProduction => &[["test", "prod"], ["prod", "test"]],
        Preset::OdooSh => &[["dev", "uat"], ["uat", "prod"], ["prod", "uat"]],
        Preset::Explicit => &[],
    }
}

/// The role an environment's name implies.
fn role_of_name(name: &str) -> Option<Role> {
    match name {
        "dev" => Some(Role::Dev),
        "test" => Some(Role::Test),
        "uat" => Some(Role::Uat),
        "prod" => Some(Role::Prod),
        _ => None,
    }
}

/// The preset of `table`: the configured one, else `explicit` when it
/// declares environments, else `major-branches`.
fn preset_of(table: &Environments) -> Preset {
    table.preset.unwrap_or(if table.env.is_empty() {
        Preset::MajorBranches
    } else {
        Preset::Explicit
    })
}

/// The environment `name` of `table`, its preset's defaults (`preset_env`)
/// applied.
fn resolve_one(
    table: &Environments,
    name: &str,
    preset_env: Option<&(&str, Role)>,
    default_series: &str,
) -> Result<Environment, (String, String)> {
    let key = format!("tool.odx.environments.env.{name}.branch");
    let set = table.env.get(name);
    let series = set
        .and_then(|env| env.series.clone())
        .unwrap_or_else(|| default_series.to_owned());
    let pattern = set
        .and_then(|env| env.branch.as_deref())
        .or_else(|| preset_env.map(|(branch, _)| *branch))
        .unwrap_or(name);
    let branch = expand(pattern, &series).map_err(|message| (key, message))?;
    let role = set
        .and_then(|env| env.role)
        .or_else(|| preset_env.map(|(_, role)| *role))
        .or_else(|| role_of_name(name));
    Ok(Environment {
        name: name.to_owned(),
        branch,
        role,
        series,
        repo: set.and_then(|env| env.repo.clone()),
        disposable: set
            .and_then(|env| env.disposable)
            .unwrap_or_else(|| role == Some(Role::Uat)),
    })
}

/// `pattern` with `{series}` and `{major}` filled in.
fn expand(pattern: &str, series: &str) -> Result<String, String> {
    let major = series.split('.').next().unwrap_or(series);
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            return Err(format!("unclosed `{{` in {pattern:?}"));
        };
        match &rest[start + 1..start + end] {
            "series" => out.push_str(series),
            "major" => out.push_str(major),
            other => {
                return Err(format!(
                    "unknown placeholder {{{other}}}; use {{series}} or {{major}}"
                ));
            }
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    if out.is_empty() {
        return Err("must not be empty".to_owned());
    }
    Ok(out)
}

/// Whether `text` matches `pattern`, where `*` matches any run of
/// characters.
fn glob(pattern: &str, text: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == text,
        Some((head, tail)) => text.strip_prefix(head).is_some_and(|rest| {
            (0..=rest.len())
                .filter(|at| rest.is_char_boundary(*at))
                .any(|at| glob(tail, &rest[at..]))
        }),
    }
}

/// How specific a branch pattern is: its characters other than `*`.
fn specificity(pattern: &str) -> usize {
    pattern.chars().filter(|c| *c != '*').count()
}

impl Model {
    /// The model of `config`, or none when it has no
    /// `[tool.odx.environments]`.
    ///
    /// # Errors
    ///
    /// Every problem, as configuration key and message.
    pub fn resolve(config: &OdxConfig) -> Result<Option<Self>, Vec<(String, String)>> {
        config
            .environments
            .as_ref()
            .map(|table| Self::from_table(table, &config.odoo_version))
            .transpose()
    }

    /// The names of the environments `table` declares, its preset's
    /// included.
    #[must_use]
    pub fn names(table: &Environments) -> Vec<String> {
        let mut names: Vec<String> = preset_environments(preset_of(table))
            .iter()
            .map(|(name, ..)| (*name).to_owned())
            .collect();
        names.extend(table.env.keys().cloned());
        names.sort();
        names.dedup();
        names
    }

    fn from_table(table: &Environments, odoo_version: &str) -> Result<Self, Vec<(String, String)>> {
        let mut problems = Vec::new();
        let key = |rest: &str| format!("tool.odx.environments.{rest}");
        let preset = preset_of(table);
        let default_series = table.series.as_deref().unwrap_or(odoo_version);
        let defaults: BTreeMap<&str, (&str, Role)> = preset_environments(preset)
            .iter()
            .map(|(name, branch, role)| (*name, (*branch, *role)))
            .collect();
        let mut names: Vec<&str> = defaults.keys().copied().collect();
        names.extend(
            table
                .env
                .keys()
                .map(String::as_str)
                .filter(|name| !defaults.contains_key(name)),
        );
        let mut environments = BTreeMap::new();
        for name in names {
            match resolve_one(table, name, defaults.get(name), default_series) {
                Ok(env) => {
                    environments.insert(name.to_owned(), env);
                }
                Err(problem) => problems.push(problem),
            }
        }
        if preset == Preset::Explicit && table.env.is_empty() {
            problems.push((
                key("env"),
                "the explicit preset needs at least one environment".to_owned(),
            ));
        }
        let mut seen: BTreeMap<(Option<&str>, &str), &str> = BTreeMap::new();
        for env in environments.values() {
            if let Some(other) = seen.insert((env.repo.as_deref(), &env.branch), &env.name) {
                problems.push((
                    key(&format!("env.{}.branch", env.name)),
                    format!("is also the branch of environment {other}"),
                ));
            }
        }
        let promotion: Vec<[String; 2]> = table.promotion.clone().unwrap_or_else(|| {
            preset_promotion(preset)
                .iter()
                .map(|edge| edge.map(str::to_owned))
                .collect()
        });
        for (i, [from, to]) in promotion.iter().enumerate() {
            if [from, to].iter().any(|name| {
                !environments.contains_key(name.as_str()) && !table.env.contains_key(*name)
            }) {
                problems.push((
                    key(&format!("promotion[{i}]")),
                    "names an environment that is not configured".to_owned(),
                ));
            } else if from == to {
                problems.push((
                    key(&format!("promotion[{i}]")),
                    "promotes an environment into itself".to_owned(),
                ));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(Self {
            preset,
            environments,
            promotion,
            branch_specific: table.branch_specific.clone().unwrap_or_default(),
        })
    }

    /// Whether `path` (relative to the project, `/`-separated) is one of
    /// the files that differ per environment.
    #[must_use]
    pub fn is_branch_specific(&self, path: &str) -> bool {
        self.branch_specific
            .iter()
            .any(|pattern| glob(pattern.trim_start_matches("./"), path))
    }

    /// The environment named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Environment> {
        self.environments.get(name)
    }

    /// Whether promoting `from` into `to` is allowed.
    #[must_use]
    pub fn allows(&self, from: &str, to: &str) -> bool {
        self.promotion.iter().any(|[a, b]| a == from && b == to)
    }

    /// The environments `from` may be promoted into, in configuration order.
    #[must_use]
    pub fn targets(&self, from: &str) -> Vec<&str> {
        self.promotion
            .iter()
            .filter(|[a, _]| a == from)
            .map(|[_, b]| b.as_str())
            .collect()
    }

    /// The environment a promotion into `to` comes from when `--from` is
    /// not given: the one allowed source that is not disposable, else the
    /// one allowed source.
    ///
    /// # Errors
    ///
    /// When no source, or more than one, qualifies.
    pub fn default_source(&self, to: &str) -> Result<&str, String> {
        let sources: Vec<&str> = self
            .promotion
            .iter()
            .filter(|[_, b]| b == to)
            .map(|[a, _]| a.as_str())
            .collect();
        let lasting: Vec<&str> = sources
            .iter()
            .copied()
            .filter(|name| self.get(name).is_some_and(|env| !env.disposable))
            .collect();
        match (lasting.as_slice(), sources.as_slice()) {
            ([one], _) | ([], [one]) => Ok(one),
            (_, []) => Err(format!("no environment promotes into {to}")),
            _ => Err(format!(
                "more than one environment promotes into {to} ({}): name one with --from",
                sources.join(", ")
            )),
        }
    }

    /// The environment `branch` of `repo` (none: the project's) belongs to:
    /// the most specific matching branch pattern.
    #[must_use]
    pub fn of_branch(&self, branch: &str, repo: Option<&str>) -> Option<&Environment> {
        self.environments
            .values()
            .filter(|env| env.repo.as_deref() == repo && env.matches(branch))
            .max_by_key(|env| specificity(&env.branch))
    }
}

impl Preset {
    /// The name used in the configuration.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MajorBranches => "major-branches",
            Self::SeriesBranches => "series-branches",
            Self::MainProduction => "main-production",
            Self::OdooSh => "odoo-sh",
            Self::Explicit => "explicit",
        }
    }
}

impl Role {
    /// The name used in the configuration.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Test => "test",
            Self::Uat => "uat",
            Self::Prod => "prod",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml: &str) -> toml_config::Wrapper {
        toml::from_str(&format!("[tool.odx]\nodoo_version = \"19.0\"\n{toml}")).unwrap()
    }

    fn model(toml: &str) -> Result<Option<Model>, Vec<(String, String)>> {
        Model::resolve(&config(toml).tool.odx)
    }

    mod toml_config {
        #[derive(serde::Deserialize)]
        pub(in super::super) struct Wrapper {
            pub(in super::super) tool: Tool,
        }
        #[derive(serde::Deserialize)]
        pub(in super::super) struct Tool {
            pub(in super::super) odx: crate::OdxConfig,
        }
    }

    fn branches(model: &Model) -> Vec<(&str, &str, Option<&str>, bool)> {
        model
            .environments
            .values()
            .map(|env| {
                (
                    env.name.as_str(),
                    env.branch.as_str(),
                    env.role.map(Role::name),
                    env.disposable,
                )
            })
            .collect()
    }

    #[test]
    fn presets() {
        assert_eq!(model(""), Ok(None));
        let major = model("[tool.odx.environments]\n").unwrap().unwrap();
        assert_eq!(major.preset, Preset::MajorBranches);
        assert_eq!(
            branches(&major),
            [
                ("prod", "19-prod", Some("prod"), false),
                ("test", "19-test", Some("test"), false),
                ("uat", "19-uat", Some("uat"), true),
            ]
        );
        assert_eq!(major.promotion.len(), 4);
        let table = Environments {
            preset: None,
            series: None,
            branch_specific: None,
            env: BTreeMap::new(),
            promotion: None,
        };
        assert_eq!(Model::names(&table), ["prod", "test", "uat"]);
        assert_eq!(major.default_source("prod"), Ok("test"));
        assert_eq!(major.default_source("uat"), Ok("test"));
        assert_eq!(
            major.default_source("nowhere"),
            Err("no environment promotes into nowhere".to_owned())
        );
        let series =
            model("[tool.odx.environments]\npreset = \"series-branches\"\nseries = \"18.0\"\n")
                .unwrap()
                .unwrap();
        assert_eq!(
            branches(&series),
            [
                ("prod", "18.0", Some("prod"), false),
                ("test", "18.0-staging", Some("test"), false),
            ]
        );
        let main = model("[tool.odx.environments]\npreset = \"main-production\"\n")
            .unwrap()
            .unwrap();
        assert_eq!(main.get("test").unwrap().branch, "main");
        assert!(main.allows("test", "prod") && main.allows("prod", "test"));
        assert!(!main.allows("test", "test"));
        assert_eq!(main.targets("test"), ["prod"]);
        let sh = model("[tool.odx.environments]\npreset = \"odoo-sh\"\n")
            .unwrap()
            .unwrap();
        assert_eq!(
            branches(&sh),
            [
                ("dev", "*", Some("dev"), false),
                ("prod", "production", Some("prod"), false),
                ("uat", "staging/*", Some("uat"), true),
            ]
        );
        // The only source into prod is disposable, and still the default.
        assert_eq!(sh.default_source("prod"), Ok("uat"));
        assert_eq!(sh.of_branch("production", None).unwrap().name, "prod");
        assert_eq!(sh.of_branch("staging/sprint-4", None).unwrap().name, "uat");
        assert_eq!(sh.of_branch("feature-x", None).unwrap().name, "dev");
        assert!(sh.of_branch("production", Some("elsewhere")).is_none());
        assert!(!sh.get("dev").unwrap().is_literal());
        assert!(sh.get("prod").unwrap().is_literal());
        for (preset, name) in [
            (Preset::MajorBranches, "major-branches"),
            (Preset::SeriesBranches, "series-branches"),
            (Preset::MainProduction, "main-production"),
            (Preset::OdooSh, "odoo-sh"),
            (Preset::Explicit, "explicit"),
        ] {
            assert_eq!(preset.name(), name);
        }
    }

    #[test]
    fn overrides_and_explicit_environments() {
        let mixed = model(
            "[tool.odx.environments]\npreset = \"major-branches\"\nbranch_specific = [\".x\"]\n\
             [tool.odx.environments.env.test]\nseries = \"20.0\"\n\
             [tool.odx.environments.env.uat]\ndisposable = false\n\
             [tool.odx.environments.env.prod]\nrepo = \"https://example.com/prod.git\"\nbranch = \"main\"\n",
        )
        .unwrap()
        .unwrap();
        let test = mixed.get("test").unwrap();
        assert_eq!(
            (test.branch.as_str(), test.series.as_str()),
            ("20-test", "20.0")
        );
        assert!(!mixed.get("uat").unwrap().disposable);
        assert_eq!(
            mixed.default_source("prod"),
            Err(
                "more than one environment promotes into prod (test, uat): name one with --from"
                    .to_owned()
            )
        );
        let prod = mixed.get("prod").unwrap();
        assert_eq!(prod.repo.as_deref(), Some("https://example.com/prod.git"));
        assert_eq!(
            mixed.of_branch("main", prod.repo.as_deref()).unwrap().name,
            "prod"
        );
        assert!(mixed.of_branch("main", None).is_none());
        assert_eq!(mixed.branch_specific, [".x"]);
        assert!(mixed.is_branch_specific(".x"));
        assert!(!mixed.is_branch_specific("pyproject.toml"));
        // Without a preset, declared environments are the whole model.
        let explicit = model(
            "[tool.odx.environments]\n\
             [tool.odx.environments.env.staging]\nbranch = \"{series}-staging\"\n\
             [tool.odx.environments.env.prod]\nbranch = \"{series}\"\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(explicit.preset, Preset::Explicit);
        assert_eq!(
            branches(&explicit),
            [
                ("prod", "19.0", Some("prod"), false),
                ("staging", "19.0-staging", None, false),
            ]
        );
        assert_eq!(explicit.promotion, Vec::<[String; 2]>::new());
        // An environment without a branch is on the branch of its name.
        let named = model("[tool.odx.environments.env.next]\nseries = \"20.0\"\n")
            .unwrap()
            .unwrap();
        assert_eq!(named.get("next").unwrap().branch, "next");
        let edged = model(
            "[tool.odx.environments]\npreset = \"main-production\"\npromotion = [[\"test\", \"prod\"]]\n",
        )
        .unwrap()
        .unwrap();
        assert!(!edged.allows("prod", "test"));
    }

    #[test]
    fn problems() {
        let problems = |toml: &str| {
            model(&format!("[tool.odx.environments]\n{toml}"))
                .unwrap_err()
                .into_iter()
                .map(|(key, message)| format!("{key}: {message}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            problems("preset = \"explicit\"\n"),
            ["tool.odx.environments.env: the explicit preset needs at least one environment"]
        );
        assert_eq!(
            problems("[tool.odx.environments.env.test]\nbranch = \"{version}-test\"\n"),
            [
                "tool.odx.environments.env.test.branch: unknown placeholder {version}; use {series} or {major}"
            ]
        );
        assert_eq!(
            problems("[tool.odx.environments.env.test]\nbranch = \"{major\"\n"),
            ["tool.odx.environments.env.test.branch: unclosed `{` in \"{major\""]
        );
        assert_eq!(
            problems("[tool.odx.environments.env.test]\nbranch = \"\"\n"),
            ["tool.odx.environments.env.test.branch: must not be empty"]
        );
        assert_eq!(
            problems(
                "preset = \"main-production\"\n[tool.odx.environments.env.prod]\nbranch = \"main\"\n"
            ),
            ["tool.odx.environments.env.test.branch: is also the branch of environment prod"]
        );
        assert_eq!(
            problems(
                "preset = \"main-production\"\npromotion = [[\"test\", \"qa\"], [\"prod\", \"prod\"]]\n"
            ),
            [
                "tool.odx.environments.promotion[0]: names an environment that is not configured",
                "tool.odx.environments.promotion[1]: promotes an environment into itself",
            ]
        );
        // An edge to an environment that failed to resolve is not reported twice.
        assert_eq!(
            problems(
                "promotion = [[\"prod\", \"qa\"]]\n[tool.odx.environments.env.qa]\nbranch = \"{x}\"\n\
                 [tool.odx.environments.env.prod]\nbranch = \"main\"\n"
            ),
            [
                "tool.odx.environments.env.qa.branch: unknown placeholder {x}; use {series} or {major}"
            ]
        );
    }

    #[test]
    fn patterns() {
        assert!(glob("staging/*", "staging/x/y"));
        assert!(glob("*-test", "19-test"));
        assert!(glob("a*b*c", "aXbYc"));
        assert!(!glob("a*b", "ac"));
        assert!(glob("*", ""));
        assert!(glob("é*", "éa"));
        assert_eq!(specificity("staging/*"), 8);
        assert_eq!(role_of_name("qa"), None);
        for role in [Role::Dev, Role::Test, Role::Uat, Role::Prod] {
            assert_eq!(role_of_name(role.name()), Some(role));
        }
        assert_eq!(Role::Dev.name(), "dev");
    }
}
