//! The `[tool.odx]` table of `pyproject.toml`.
//!
//! Every table under `[tool.odx]` rejects unknown keys so that a typo is an
//! error instead of a silently disabled feature. Optional values stay
//! `Option` here; defaults are applied by the accessors and by the crates
//! that consume a section.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::Span;

/// The `[tool.odx]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OdxConfig {
    /// Odoo series of the project, e.g. `"17.0"`.
    pub odoo_version: String,
    /// Languages the project is translated into besides English, as Odoo codes (`"es"`, `"nl_BE"`).
    /// Default: none (English only).
    #[serde(default)]
    pub languages: Vec<String>,
    /// Neutral project identifier for the verdict repository. Default: the first twelve hex digits
    /// of the SHA-256 of the project's `origin` URL.
    pub project_id: Option<String>,
    /// Maximum concurrent fetches and merge jobs. Default: the number of CPUs.
    pub jobs: Option<u16>,
    /// Cache directory, relative to the project root. Default: the shared cache directory of the
    /// user configuration, else `.odx/cache`.
    pub cache_dir: Option<PathBuf>,
    /// How merges are applied on top of the base. Default: `"rebase"`.
    pub merge_strategy: Option<MergeStrategy>,
    /// When to use mergiraf for conflict resolution. Default: `"auto"`.
    pub mergiraf: Option<MergirafMode>,
    /// Default conflict policy for merges. Default: `"abort"`.
    pub on_conflict: Option<OnConflict>,
    /// Default force-push guard-rail policy. Default: `"follow-if-green"`.
    pub on_head_change: Option<OnHeadChange>,
    /// Lockfile path, relative to the project root. Default: `odx.lock`.
    pub lockfile: Option<PathBuf>,
    /// Private vault repository for locked commits.
    pub mirror: Option<Mirror>,
    /// Forge HTTP access.
    pub http: Option<Http>,
    /// Cache staleness and prefetching.
    pub cache: Option<Cache>,
    /// Dashboard sections and environment pairs.
    pub dashboard: Option<Dashboard>,
    /// Dependency groups and the directories their addons are vendored into.
    pub groups: Option<Groups>,
    /// Repositories to aggregate. Default: none.
    #[serde(default)]
    pub repositories: Vec<Repository>,
    /// Packages not expressed as direct references, keyed by distribution name. Default: none.
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
    /// Vendoring options.
    pub vendor: Option<Vendor>,
    /// Lockfile options.
    pub lock: Option<Lock>,
    /// Upgrade analysis.
    pub upgrade: Option<Upgrade>,
    /// Addon index.
    pub index: Option<Index>,
    /// Porting.
    pub port: Option<Port>,
    /// Upstream suggestions.
    pub suggest: Option<Suggest>,
    /// Feature sets, keyed by name. Default: none.
    #[serde(default)]
    pub features: BTreeMap<String, Feature>,
    /// Directory layout.
    pub layout: Option<Layout>,
    /// Deployment platform profile.
    pub platform: Option<Platform>,
    /// Environments and promotion edges.
    pub environments: Option<Environments>,
    /// Promotion options.
    pub promote: Option<Promote>,
    /// Automation bot.
    pub bot: Option<Bot>,
    /// Local development.
    pub dev: Option<Dev>,
    /// Lint options.
    pub lint: Option<Lint>,
    /// Migration script sniffer.
    pub sniff: Option<Sniff>,
    /// `odx check-deps`.
    pub check_deps: Option<CheckDeps>,
    /// `odx glue`.
    pub glue: Option<Glue>,
    /// Addons the project may need later, watched until someone migrates them (`odx wanted`).
    /// Default: none.
    #[serde(default)]
    pub wanted: Vec<Wanted>,
}

impl OdxConfig {
    /// The lockfile path, `odx.lock` unless configured.
    pub fn lockfile(&self) -> PathBuf {
        self.lockfile
            .clone()
            .unwrap_or_else(|| PathBuf::from("odx.lock"))
    }

    /// Directories of committed project addons, relative to the project
    /// root: `[tool.odx.groups] project_addons`, else `[tool.odx.layout]
    /// project_addons`, else the project root.
    pub fn project_addon_dirs(&self) -> Vec<PathBuf> {
        self.groups
            .as_ref()
            .and_then(|groups| groups.project_addons.clone())
            .or_else(|| {
                self.layout
                    .as_ref()
                    .and_then(|layout| layout.project_addons.clone())
            })
            .unwrap_or_else(|| vec![PathBuf::from(".")])
    }

    /// Dependency group → the directory its addons are vendored into:
    /// `[tool.odx.layout] groups`, overridden by `[tool.odx.groups]`.
    pub fn group_dirs(&self) -> BTreeMap<String, PathBuf> {
        let mut dirs = self
            .layout
            .as_ref()
            .and_then(|layout| layout.groups.clone())
            .unwrap_or_default();
        if let Some(groups) = &self.groups {
            dirs.extend(groups.dirs.clone());
        }
        dirs
    }
}

/// How merges are applied on top of the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeStrategy {
    /// Replay the commits on top, keeping history linear.
    #[default]
    Rebase,
    /// One merge commit per merge.
    Merge,
}

/// When mergiraf resolves conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergirafMode {
    /// Use mergiraf when it is installed.
    #[default]
    Auto,
    /// Require mergiraf.
    Always,
    /// Never use mergiraf.
    Never,
}

/// What happens when a merge conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnConflict {
    /// Abort the aggregation of the repository.
    #[default]
    Abort,
    /// Skip the conflicting merge and continue.
    Skip,
}

/// Force-push guard-rail policy for tracked pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OnHeadChange {
    /// Keep the locked commit.
    Pin,
    /// Follow the new head only when its CI is green.
    #[default]
    FollowIfGreen,
    /// Always follow the new head.
    Follow,
}

/// `[tool.odx.mirror]`: the private vault repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    /// Push URL of the vault repository; must be private.
    pub url: String,
    /// Prefix of content-addressed refs. Default: `refs/odx`.
    pub ref_prefix: Option<String>,
    /// Whether locked commits are kept under refs or tags. Default: `"refs"`, or `"tags"` when the
    /// vault protects tags (GitLab, or created with `auto_create`).
    pub ref_style: Option<RefStyle>,
    /// Push locked commits on `odx lock`. Default: on.
    pub push_on_lock: Option<bool>,
    /// Create the vault repository through the forge API. Default: off.
    pub auto_create: Option<bool>,
    /// Allow a public vault repository. Default: off.
    pub allow_public: Option<bool>,
}

/// How vaulted commits are referenced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefStyle {
    /// `<ref_prefix>/<sha>`.
    #[default]
    Refs,
    /// `refs/tags/odx/<sha>`.
    Tags,
}

/// `[tool.odx.http]`: how odx talks to forges over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    /// In-flight requests and fetches per host. Default: 6.
    pub per_host_concurrency: Option<u16>,
}

/// `[tool.odx.cache]`: cache staleness and prefetching.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    /// Staleness thresholds per kind of cached data.
    pub ttl: Option<CacheTtl>,
    /// Let `odx sync` prefetch wheels. Default: on.
    pub prefetch_wheels: Option<bool>,
}

/// `[tool.odx.cache.ttl]`: how long cached data is used before it is refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheTtl {
    /// How long fetched git refs are used before fetching again. Default: `"15m"`.
    pub refs: Option<Span>,
    /// How long a kept dashboard (pull request states) is shown before it is rebuilt. Default:
    /// `"30m"`.
    pub pr_state: Option<Span>,
}

/// `[tool.odx.dashboard]`: what `odx dashboard` shows.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dashboard {
    /// The sections `odx dashboard` shows when `--section` is not given. Default: all.
    pub sections: Option<Vec<DashboardSection>>,
    /// Environment pairs compared in the drift section. Default: every promotion edge between
    /// environments that are not disposable.
    pub environment_pairs: Option<Vec<[String; 2]>>,
}

/// A dashboard section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DashboardSection {
    /// Pinned pull requests.
    Pinned,
    /// Overrides that can be dropped.
    Droppable,
    /// New upstream pull requests.
    NewUpstream,
    /// Forward-port suggestions.
    ForwardPort,
    /// Backport candidates.
    Backport,
    /// Auto-update candidates.
    AutoUpdate,
    /// Glue addons the project misses.
    Glue,
    /// Environment drift.
    Drift,
    /// What verdicts, mutes and blacklists hide.
    Suppressed,
}

impl DashboardSection {
    /// The section's name, as `--section` takes it.
    ///
    /// ```
    /// use odx_config::schema::DashboardSection;
    ///
    /// assert_eq!(DashboardSection::NewUpstream.name(), "new-upstream");
    /// ```
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Droppable => "droppable",
            Self::NewUpstream => "new-upstream",
            Self::ForwardPort => "forward-port",
            Self::Backport => "backport",
            Self::AutoUpdate => "auto-update",
            Self::Glue => "glue",
            Self::Drift => "drift",
            Self::Suppressed => "suppressed",
        }
    }
}

/// `[tool.odx.groups]`: dependency group name → vendor directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Groups {
    /// Group used by `add` and `check-deps --fix`, and by packages that name none. Default: none.
    pub default: Option<String>,
    /// Committed project addon directories. Default: `[tool.odx.layout] project_addons`, else the
    /// project root.
    pub project_addons: Option<Vec<PathBuf>>,
    /// Group name → directory.
    #[serde(flatten)]
    pub dirs: BTreeMap<String, PathBuf>,
}

/// `[[tool.odx.repositories]]`: a repository aggregated from its base and merges.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Short name, unique in the project. Default: `url`.
    pub name: Option<String>,
    /// Upstream URL.
    pub url: String,
    /// Working tree directory. Default: `<aggregate_dir>/<last segment of name>`.
    pub target_dir: Option<PathBuf>,
    /// Base branch. Default: `odoo_version`.
    pub base: Option<String>,
    /// Aggregated branch, as exported to `repos.yaml`. Default: `<base>-odx`.
    pub target_branch: Option<String>,
    /// Forge kind when it cannot be derived from the URL. Default: derived from the URL.
    pub forge: Option<ForgeKind>,
    /// Addon globs to link (the doodba `addons.yaml` selection). Default: every addon.
    pub addons: Option<Vec<String>>,
    /// Merges applied in order. Default: none.
    #[serde(default)]
    pub merges: Vec<Merge>,
}

/// A forge implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeKind {
    /// GitHub.
    Github,
    /// GitLab.
    Gitlab,
    /// Gitea or Forgejo.
    Gitea,
}

/// One merge source: exactly one of `pr`, `sha` or `branch`.
///
/// Kept as a flat table so that a malformed entry yields a precise error;
/// [`Merge::source`] gives the validated form.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merge {
    /// Pull request number.
    pub pr: Option<u32>,
    /// Commit SHA.
    pub sha: Option<String>,
    /// Branch name.
    pub branch: Option<String>,
    /// Remote URL of `branch`. Default: the repository URL.
    pub remote: Option<String>,
    /// Conflict policy for this merge. Default: `[tool.odx] on_conflict`.
    pub on_conflict: Option<OnConflict>,
    /// Guard-rail policy for this merge. Default: `[tool.odx] on_head_change`.
    pub on_head_change: Option<OnHeadChange>,
    /// Report when the pull request is merged upstream. Default: on.
    pub track_upstream: Option<bool>,
}

/// The validated source of a [`Merge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeSource<'a> {
    /// A pull request.
    Pr(u32),
    /// A commit.
    Sha(&'a str),
    /// A branch, optionally on another remote.
    Branch {
        /// Branch name.
        branch: &'a str,
        /// Remote URL, if not the repository's.
        remote: Option<&'a str>,
    },
}

impl Merge {
    /// The merge source, or why the entry is invalid.
    ///
    /// # Errors
    ///
    /// Returns a message when the entry names no source, more than one, or
    /// sets `remote` without `branch`.
    pub fn source(&self) -> Result<MergeSource<'_>, String> {
        let count = usize::from(self.pr.is_some())
            + usize::from(self.sha.is_some())
            + usize::from(self.branch.is_some());
        match (self.pr, self.sha.as_deref(), self.branch.as_deref()) {
            _ if count > 1 => Err("set only one of `pr`, `sha` or `branch`".to_owned()),
            (None, None, Some(branch)) => Ok(MergeSource::Branch {
                branch,
                remote: self.remote.as_deref(),
            }),
            _ if self.remote.is_some() => {
                Err("`remote` is only valid together with `branch`".to_owned())
            }
            (Some(pr), _, _) => Ok(MergeSource::Pr(pr)),
            (_, Some(sha), _) => Ok(MergeSource::Sha(sha)),
            _ => Err("needs one of `pr`, `sha` or `branch`".to_owned()),
        }
    }
}

/// `[tool.odx.packages.<dist>]`: a package pinned to a git source.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    /// Repository as `owner/repo` on GitHub.
    pub repo: Option<String>,
    /// Repository URL. Default: `https://github.com/<repo>.git`.
    pub url: Option<String>,
    /// Pull request number.
    pub pr: Option<u32>,
    /// Branch.
    pub branch: Option<String>,
    /// Tag.
    pub tag: Option<String>,
    /// Commit.
    pub rev: Option<String>,
    /// Subdirectory of the addon in the repository. Default: the repository root.
    pub subdirectory: Option<String>,
    /// Dependency group. Default: `[tool.odx.groups] default`.
    pub group: Option<String>,
}

/// `[tool.odx.vendor]`: how addons are vendored into the project.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vendor {
    /// Bump the last manifest version segment on content changes. Default: on.
    pub bump_on_content_change: Option<bool>,
    /// Files removed from vendored addons. Default: `["static/description/index.html"]`.
    pub exclude: Option<Vec<String>>,
    /// Vendor manifest path. Default: `[tool.odx.layout] vendor_manifest`, else `.odx/vendor.lock`.
    pub manifest: Option<PathBuf>,
}

/// `[tool.odx.lock]`: lockfile options.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    /// Automatic re-lock of index packages.
    pub auto_update: Option<AutoUpdate>,
}

/// `[tool.odx.lock.auto_update]`: automatic re-lock of index packages.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoUpdate {
    /// Which version changes are applied automatically. Default: `"last-segment"`.
    pub policy: Option<AutoUpdatePolicy>,
    /// Distribution globs allowed. Default: `["odoo-addon-*"]`.
    pub allow: Option<Vec<String>>,
    /// Distribution globs denied. Default: none.
    pub deny: Option<Vec<String>>,
    /// Compare wheel contents before applying. Default: on.
    pub content_check: Option<bool>,
    /// Paths allowed to change when `content_check` is on. Default: `i18n/*.po`, `i18n/*.pot`,
    /// `README.rst`, `readme/**` and `static/description/**`.
    pub allowed_paths: Option<Vec<String>>,
}

/// Auto-update policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutoUpdatePolicy {
    /// Only the last version segment may change.
    LastSegment,
    /// No automatic updates.
    None,
}

/// `[tool.odx.upgrade]`: upgrade analysis (`odx migrate plan`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    /// Target Odoo series. Default: none (pass `--target`).
    pub target_odoo_version: Option<String>,
}

/// `[tool.odx.index]`: the addon index that maps addons to repositories.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// How long the cached addon index is used. Default: `"7d"`.
    pub ttl: Option<Span>,
    /// Organizations whose forge listings are indexed, as names on GitHub or URLs. Default: none.
    pub orgs: Option<Vec<String>>,
    /// Sources, in trust order. Default: all.
    pub sources: Option<Vec<IndexSource>>,
    /// Addon → series → `owner/repo`. Default: none.
    #[serde(default)]
    pub overrides: BTreeMap<String, BTreeMap<String, String>>,
    /// Series → old name → new name. Default: none.
    #[serde(default)]
    pub renames: BTreeMap<String, BTreeMap<String, String>>,
}

/// A source of the addon index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndexSource {
    /// `[tool.odx.index.overrides]`.
    Overrides,
    /// The local cache.
    Cache,
    /// Package index metadata.
    PackageIndex,
    /// Forge repository listings.
    Forge,
    /// The OCA repository list.
    OcaRepoList,
    /// OpenUpgrade rename lists.
    Openupgrade,
}

/// `[tool.odx.port]`: porting commits between series.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// Default source series. Default: the series just below and just above the target.
    pub default_from: Option<Vec<String>>,
    /// Commits touching more addons are repo-wide. Default: 5.
    pub repo_wide_threshold: Option<u16>,
    /// Commit subjects that are never proposed, as case-insensitive regular expressions. Default:
    /// `^\[MIG\]` and `^\[ADD\].*\bmigration\b`.
    pub migration_commit_patterns: Option<Vec<String>>,
    /// Author e-mails of bots whose commits `odx migrate` squashes. Default: the OCA bots.
    pub squash_authors: Option<Vec<String>>,
    /// Text in a subject that marks a bot commit to squash. Default: the OCA bots' commits.
    pub squash_subjects: Option<Vec<String>>,
}

/// `[tool.odx.suggest]`: upstream suggestions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suggest {
    /// The series `odx suggest` looks on when `--from` is not given. Default:
    /// `[tool.odx.port] default_from`, else the series just below and just above.
    pub series: Option<Vec<String>>,
}

/// `[tool.odx.features.<name>]`: a named set of addons promoted together.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    /// Addons that make up the feature.
    pub modules: Vec<String>,
    /// Functional description. Default: none.
    pub description: Option<String>,
}

/// `[tool.odx.layout]`: where the project keeps its files.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// Committed project addon directories. Default: the project root.
    pub project_addons: Option<Vec<PathBuf>>,
    /// Parent of repository working trees. Default: `vendor`.
    pub aggregate_dir: Option<PathBuf>,
    /// Alias of `[tool.odx.groups]` directories. Default: none.
    pub groups: Option<BTreeMap<String, PathBuf>>,
    /// Platform requirements file. Default: `requirements.txt` for the `odoo-sh` profile, else
    /// `dist/requirements.txt`.
    pub requirements: Option<PathBuf>,
    /// Vendor manifest. Default: `.odx/vendor.lock`.
    pub vendor_manifest: Option<PathBuf>,
}

/// `[tool.odx.platform]`: the deployment platform.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    /// Platform profile. Default: `"generic-wheels"`.
    pub profile: Option<PlatformProfile>,
    /// Provided-package set name or constraints file. Default: `odoo-sh` for the `odoo-sh` profile,
    /// else `odoo-standard`.
    pub provided: Option<String>,
    /// Extra provided requirements. Default: none.
    pub provided_extra: Option<Vec<String>>,
    /// Packages removed from the provided set. Default: none.
    pub provided_remove: Option<Vec<String>>,
    /// Package index name for publishing. Default: `private`.
    pub index: Option<String>,
    /// Hooks of the `custom` profile.
    pub custom: Option<CustomHooks>,
}

/// A deployment platform profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlatformProfile {
    /// Installs wheels from a package index.
    #[default]
    GenericWheels,
    /// Addons copied into the repository.
    VendoredSource,
    /// Odoo.sh.
    OdooSh,
    /// Project-defined hooks.
    Custom,
}

/// Hooks of the `custom` platform profile.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomHooks {
    /// Command that prepares the deployment. Default: none.
    pub prepare: Option<String>,
    /// Command that verifies the deployment. Default: none.
    pub verify: Option<String>,
}

/// `[tool.odx.environments]`: environments and how changes are promoted between them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environments {
    /// Layout preset. Default: `"explicit"` when environments are declared under `env`, else
    /// `"major-branches"`.
    pub preset: Option<Preset>,
    /// Default series of the environments. Default: `odoo_version`.
    pub series: Option<String>,
    /// Files that differ per environment. Default: none.
    pub branch_specific: Option<Vec<String>>,
    /// Environments by name. Default: the preset's.
    #[serde(default)]
    pub env: BTreeMap<String, Environment>,
    /// Allowed promotion edges. Default: the preset's.
    pub promotion: Option<Vec<[String; 2]>>,
}

/// An environment layout preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    /// `<major>-test` and `<major>-prod` branches.
    #[default]
    MajorBranches,
    /// `<series>` and `<series>-staging` branches.
    SeriesBranches,
    /// `main` and `production`.
    MainProduction,
    /// The Odoo.sh stage model.
    OdooSh,
    /// Only the configured environments.
    Explicit,
}

/// `[tool.odx.environments.env.<name>]`: one environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Branch name or pattern with `{series}` and `{major}`. Default: the preset's, else the
    /// environment name.
    pub branch: Option<String>,
    /// Environment role. Default: the preset's, else implied by a name `dev`, `test`, `uat` or
    /// `prod`.
    pub role: Option<Role>,
    /// Series of this environment. Default: `[tool.odx.environments] series`.
    pub series: Option<String>,
    /// Separate repository of this environment. Default: none (the project repository).
    pub repo: Option<String>,
    /// Excluded as a promotion target unless named explicitly. Default: on for the `uat` role, else
    /// off.
    pub disposable: Option<bool>,
}

/// The role of an environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// Development.
    Dev,
    /// Test.
    Test,
    /// User acceptance.
    Uat,
    /// Production.
    Prod,
}

/// `[tool.odx.promote]`: defaults of `odx promote`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Promote {
    /// Allow copying an older pin over a newer one. Default: off.
    pub allow_downgrade: Option<bool>,
    /// Cherry-pick project addon commits unless `--no-with-code`. Default: off.
    pub with_code: Option<bool>,
    /// Promote the Odoo core revision together with the pins. Default: on.
    pub with_odoo: Option<bool>,
    /// Extra synonyms for `--search`. Default: none.
    #[serde(default)]
    pub search_synonyms: BTreeMap<String, Vec<String>>,
}

/// `[tool.odx.bot]`: the automation bot (`odx bot`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bot {
    /// Environment that receives bot pull requests. Default: the branch checked out.
    pub environment: Option<String>,
    /// Maintain the dashboard issue. Default: on.
    pub dashboard: Option<bool>,
    /// Maximum pull requests per run. Default: 10.
    pub max_prs_per_run: Option<u16>,
    /// Extra labels on every bot pull request, besides odx's own. Default: none.
    pub labels: Option<Vec<String>>,
    /// Expiry of mutes made from the dashboard. Default: `"180d"`.
    pub mute_expiry: Option<Span>,
    /// Kinds opened without a checkbox. Default: all.
    pub auto: Option<Vec<BotAction>>,
    /// Label that triggers a promotion pull request. Default: `promote`.
    pub promote_label: Option<String>,
    /// Vault every observed tracked head. Default: off.
    pub vault_heads: Option<bool>,
    /// Run on a public project repository. Default: off.
    pub allow_public: Option<bool>,
    /// Forge users mentioned when a wanted addon's migration starts or lands. Default: none.
    pub notify: Option<Vec<String>>,
}

/// A kind of bot pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BotAction {
    /// Drop an override merged upstream.
    DropOverride,
    /// Re-pin a pull request whose head changed and is green.
    Repin,
    /// Apply an auto-update.
    AutoUpdate,
}

/// `[tool.odx.dev]`: local development (`odx dev`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dev {
    /// Virtual environment directory. Default: `.venv`.
    pub venv: Option<PathBuf>,
    /// File listing the addons path. Default: `.odx/addons-path`.
    pub addons_path_file: Option<PathBuf>,
    /// Odoo configuration file whose `addons_path` is updated. Default: none.
    pub odoo_conf: Option<PathBuf>,
    /// Use editable installs. Default: on.
    pub editable: Option<bool>,
    /// Directory for develop-mode clones. Default: `.src`.
    pub dev_dir: Option<PathBuf>,
}

/// `[tool.odx.lint]`: defaults of `odx lint`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lint {
    /// Paths linted by default. Default: the project addon directories.
    pub paths: Option<Vec<PathBuf>>,
    /// Only lint changed addons. Default: off.
    pub only_changed: Option<bool>,
}

/// `[tool.odx.sniff]`: the migration script sniffer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sniff {
    /// Fail when merges introduce migration scripts. Default: off.
    pub fail_on_new: Option<bool>,
}

/// `[tool.odx.check_deps]`: defaults of `odx check-deps`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDeps {
    /// Addons the platform provides beyond Odoo's community addons (for example enterprise addons),
    /// counted as available. Default: none.
    pub provided: Option<Vec<String>>,
}

/// `[tool.odx.glue]`: glue addons (`odx glue`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Glue {
    /// Glue addons the project does not want, each with why. Default: none.
    #[serde(default)]
    pub ignore: BTreeMap<String, String>,
}

/// `[[tool.odx.wanted]]`: an addon the project may need later. `odx wanted`
/// says per series whether it is there, whether someone is migrating it, or
/// neither.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wanted {
    /// The addon's technical name.
    pub addon: String,
    /// Series to watch it on. Default: `odoo_version`.
    pub series: Option<Vec<String>>,
    /// Its repository, as `owner/repo` on GitHub or a URL. Default: where the addon index finds it.
    pub repository: Option<String>,
    /// Why it is wanted. Default: none.
    pub note: Option<String>,
}

impl OdxConfig {
    /// The series `wanted` is watched on.
    pub fn wanted_series(&self, wanted: &Wanted) -> Vec<String> {
        wanted
            .series
            .clone()
            .unwrap_or_else(|| vec![self.odoo_version.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(
        pr: Option<u32>,
        sha: Option<&str>,
        branch: Option<&str>,
        remote: Option<&str>,
    ) -> Merge {
        Merge {
            pr,
            sha: sha.map(str::to_owned),
            branch: branch.map(str::to_owned),
            remote: remote.map(str::to_owned),
            on_conflict: None,
            on_head_change: None,
            track_upstream: None,
        }
    }

    #[test]
    fn merge_sources() {
        assert_eq!(
            merge(Some(1), None, None, None).source(),
            Ok(MergeSource::Pr(1))
        );
        assert_eq!(
            merge(None, Some("abc1234"), None, None).source(),
            Ok(MergeSource::Sha("abc1234"))
        );
        assert_eq!(
            merge(None, None, Some("fix"), Some("https://x/y.git")).source(),
            Ok(MergeSource::Branch {
                branch: "fix",
                remote: Some("https://x/y.git")
            })
        );
        assert!(
            merge(None, None, None, None)
                .source()
                .unwrap_err()
                .contains("needs one")
        );
        assert!(
            merge(None, None, None, Some("r"))
                .source()
                .unwrap_err()
                .contains("`remote`")
        );
        assert!(
            merge(Some(1), None, None, Some("r"))
                .source()
                .unwrap_err()
                .contains("`remote`")
        );
        assert!(
            merge(Some(1), Some("a"), None, None)
                .source()
                .unwrap_err()
                .contains("only one")
        );
        assert!(
            merge(Some(1), None, Some("b"), Some("r"))
                .source()
                .unwrap_err()
                .contains("only one")
        );
    }

    #[test]
    fn lockfile_default() {
        let config: OdxConfig = toml::from_str("odoo_version = \"17.0\"").unwrap();
        assert_eq!(config.lockfile(), PathBuf::from("odx.lock"));
        let config: OdxConfig =
            toml::from_str("odoo_version = \"17.0\"\nlockfile = \"locks/odx.lock\"").unwrap();
        assert_eq!(config.lockfile(), PathBuf::from("locks/odx.lock"));
    }

    #[test]
    fn addon_dirs() {
        let config: OdxConfig = toml::from_str("odoo_version = \"17.0\"").unwrap();
        assert_eq!(config.project_addon_dirs(), [PathBuf::from(".")]);
        assert!(config.group_dirs().is_empty());
        let config: OdxConfig = toml::from_str(
            "odoo_version = \"17.0\"\n\
             [layout]\nproject_addons = [\"addons\"]\ngroups = { oca = \"a\", private = \"p\" }\n",
        )
        .unwrap();
        assert_eq!(config.project_addon_dirs(), [PathBuf::from("addons")]);
        let config: OdxConfig = toml::from_str(
            "odoo_version = \"17.0\"\n\
             [groups]\nproject_addons = [\"custom\"]\noca = \"modules/oca\"\n\
             [layout]\nproject_addons = [\"addons\"]\ngroups = { oca = \"a\", private = \"p\" }\n",
        )
        .unwrap();
        assert_eq!(config.project_addon_dirs(), [PathBuf::from("custom")]);
        assert_eq!(
            config.group_dirs().into_iter().collect::<Vec<_>>(),
            [
                ("oca".to_owned(), PathBuf::from("modules/oca")),
                ("private".to_owned(), PathBuf::from("p"))
            ]
        );
    }

    #[test]
    fn dashboard_section_names_are_their_values() {
        let names = [
            "pinned",
            "droppable",
            "new-upstream",
            "forward-port",
            "backport",
            "auto-update",
            "glue",
            "drift",
            "suppressed",
        ];
        for name in names {
            let parsed: DashboardSection = toml::Value::String(name.to_owned()).try_into().unwrap();
            assert_eq!(parsed.name(), name);
        }
    }

    #[test]
    fn enum_defaults() {
        assert_eq!(MergeStrategy::default(), MergeStrategy::Rebase);
        assert_eq!(MergirafMode::default(), MergirafMode::Auto);
        assert_eq!(OnConflict::default(), OnConflict::Abort);
        assert_eq!(OnHeadChange::default(), OnHeadChange::FollowIfGreen);
        assert_eq!(RefStyle::default(), RefStyle::Refs);
        assert_eq!(PlatformProfile::default(), PlatformProfile::GenericWheels);
        assert_eq!(Preset::default(), Preset::MajorBranches);
    }
}
