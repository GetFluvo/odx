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
    /// Neutral project identifier for the verdict repository.
    pub project_id: Option<String>,
    /// Maximum concurrent fetches and merge jobs.
    pub jobs: Option<u16>,
    /// Shared cache directory.
    pub cache_dir: Option<PathBuf>,
    /// How merges are applied on top of the base.
    pub merge_strategy: Option<MergeStrategy>,
    /// When to use mergiraf for conflict resolution.
    pub mergiraf: Option<MergirafMode>,
    /// Default conflict policy for merges.
    pub on_conflict: Option<OnConflict>,
    /// Default force-push guard-rail policy.
    pub on_head_change: Option<OnHeadChange>,
    /// Lockfile path, relative to the project root.
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
    /// Repositories to aggregate.
    #[serde(default)]
    pub repositories: Vec<Repository>,
    /// Packages not expressed as direct references, keyed by distribution name.
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
    /// Feature sets, keyed by name.
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
    /// Prefix of content-addressed refs.
    pub ref_prefix: Option<String>,
    /// Refs or tags.
    pub ref_style: Option<RefStyle>,
    /// Push locked commits on `odx lock`.
    pub push_on_lock: Option<bool>,
    /// Create the vault repository through the forge API.
    pub auto_create: Option<bool>,
    /// Allow a public vault repository.
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

/// `[tool.odx.http]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Http {
    /// In-flight requests and fetches per host.
    pub per_host_concurrency: Option<u16>,
    /// HTTP cache TTL before revalidation.
    pub cache_ttl: Option<Span>,
    /// Serve from the cache only.
    pub offline: Option<bool>,
}

/// `[tool.odx.cache]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    /// Staleness thresholds per kind of cached data.
    pub ttl: Option<CacheTtl>,
    /// Let `odx sync` warm clean worktrees.
    pub warm_worktrees: Option<bool>,
    /// Let `odx sync` prefetch wheels.
    pub prefetch_wheels: Option<bool>,
}

/// `[tool.odx.cache].ttl`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheTtl {
    /// Git refs.
    pub refs: Option<Span>,
    /// Pull request state.
    pub pr_state: Option<Span>,
    /// Addon index.
    pub index: Option<Span>,
    /// Wheels.
    pub wheels: Option<Span>,
    /// Suggestions.
    pub suggest: Option<Span>,
    /// Environment drift.
    pub drift: Option<Span>,
}

/// `[tool.odx.dashboard]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dashboard {
    /// Sections shown, in order.
    pub sections: Option<Vec<DashboardSection>>,
    /// Environment pairs compared in the drift section.
    pub environment_pairs: Option<Vec<[String; 2]>>,
    /// Show suggestions from higher series expanded.
    pub show_backports: Option<bool>,
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
    /// Auto-update candidates.
    AutoUpdate,
    /// Environment drift.
    Drift,
}

/// `[tool.odx.groups]`: dependency group name → vendor directory.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Groups {
    /// Group used by `add` and `check-deps --fix`.
    pub default: Option<String>,
    /// Committed project addon directories.
    pub project_addons: Option<Vec<PathBuf>>,
    /// Group name → directory.
    #[serde(flatten)]
    pub dirs: BTreeMap<String, PathBuf>,
}

/// `[[tool.odx.repositories]]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Short name, unique in the project.
    pub name: Option<String>,
    /// Upstream URL.
    pub url: String,
    /// Working tree directory.
    pub target_dir: Option<PathBuf>,
    /// Base branch.
    pub base: Option<String>,
    /// Aggregated branch.
    pub target_branch: Option<String>,
    /// Forge kind when it cannot be derived from the URL.
    pub forge: Option<ForgeKind>,
    /// Addon globs to link (the doodba `addons.yaml` selection).
    pub addons: Option<Vec<String>>,
    /// Merges applied in order.
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
    /// Remote URL of `branch`; defaults to the repository URL.
    pub remote: Option<String>,
    /// Conflict policy for this merge.
    pub on_conflict: Option<OnConflict>,
    /// Guard-rail policy for this merge.
    pub on_head_change: Option<OnHeadChange>,
    /// Report when the pull request is merged upstream.
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

/// `[tool.odx.packages.<dist>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    /// Repository as `owner/repo` on the default forge.
    pub repo: Option<String>,
    /// Repository URL.
    pub url: Option<String>,
    /// Pull request number.
    pub pr: Option<u32>,
    /// Branch.
    pub branch: Option<String>,
    /// Tag.
    pub tag: Option<String>,
    /// Commit.
    pub rev: Option<String>,
    /// Subdirectory of the addon in the repository.
    pub subdirectory: Option<String>,
    /// Dependency group; defaults to `groups.default`.
    pub group: Option<String>,
}

/// `[tool.odx.vendor]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vendor {
    /// Bump the last manifest version segment on content changes.
    pub bump_on_content_change: Option<bool>,
    /// Files removed from vendored addons.
    pub exclude: Option<Vec<String>>,
    /// Vendor manifest path.
    pub manifest: Option<PathBuf>,
}

/// `[tool.odx.lock]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    /// Automatic re-lock of index packages.
    pub auto_update: Option<AutoUpdate>,
}

/// `[tool.odx.lock.auto_update]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoUpdate {
    /// Which version changes are applied automatically.
    pub policy: Option<AutoUpdatePolicy>,
    /// Distribution globs allowed.
    pub allow: Option<Vec<String>>,
    /// Distribution globs denied.
    pub deny: Option<Vec<String>>,
    /// Compare wheel contents before applying.
    pub content_check: Option<bool>,
    /// Paths allowed to change when `content_check` is on.
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

/// `[tool.odx.upgrade]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    /// Target Odoo series.
    pub target_odoo_version: Option<String>,
    /// Query the package index.
    pub check_pypi: Option<bool>,
    /// Organizations searched on GitHub.
    pub github_orgs: Option<Vec<String>>,
}

/// `[tool.odx.index]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// Index cache TTL.
    pub ttl: Option<Span>,
    /// Organizations indexed.
    pub orgs: Option<Vec<String>>,
    /// Sources, in trust order.
    pub sources: Option<Vec<IndexSource>>,
    /// Addon → series → `owner/repo`.
    #[serde(default)]
    pub overrides: BTreeMap<String, BTreeMap<String, String>>,
    /// Series → old name → new name.
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

/// `[tool.odx.port]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    /// Where declined suggestions are recorded.
    pub blacklist: Option<BlacklistMode>,
    /// Default source series.
    pub default_from: Option<Vec<String>>,
    /// Commits touching more addons are repo-wide.
    pub repo_wide_threshold: Option<u16>,
    /// Commit subjects that are never proposed.
    pub migration_commit_patterns: Option<Vec<String>>,
    /// Migration step after cherry-picking.
    pub migrate: Option<MigrateTool>,
    /// Path prefix rewrites between series.
    #[serde(default)]
    pub rewrites: BTreeMap<String, String>,
}

/// Where porting decisions are recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlacklistMode {
    /// oca-port files when the repository has `.oca/`, otherwise mutes.
    #[default]
    Auto,
    /// oca-port blacklist files.
    OcaPort,
    /// odx mutes.
    Mutes,
    /// Both.
    Both,
}

/// Code migration tool used after a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MigrateTool {
    /// odoo-lint upgrade fixes.
    #[default]
    OdooLint,
    /// odoo-module-migrator.
    OdooModuleMigrator,
    /// No migration step.
    None,
}

/// `[tool.odx.suggest]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suggest {
    /// Series scanned for suggestions.
    pub series: Option<Vec<String>>,
    /// Project mutes file.
    pub mutes_file: Option<PathBuf>,
}

/// `[tool.odx.features.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    /// Addons that make up the feature.
    pub modules: Vec<String>,
    /// Functional description.
    pub description: Option<String>,
}

/// `[tool.odx.layout]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    /// Committed project addon directories.
    pub project_addons: Option<Vec<PathBuf>>,
    /// Parent of repository working trees.
    pub aggregate_dir: Option<PathBuf>,
    /// Alias of `[tool.odx.groups]` directories.
    pub groups: Option<BTreeMap<String, PathBuf>>,
    /// Platform requirements file.
    pub requirements: Option<PathBuf>,
    /// Vendor manifest.
    pub vendor_manifest: Option<PathBuf>,
}

/// `[tool.odx.platform]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    /// Platform profile.
    pub profile: Option<PlatformProfile>,
    /// Provided-package set name or constraints file.
    pub provided: Option<String>,
    /// Extra provided requirements.
    pub provided_extra: Option<Vec<String>>,
    /// Packages removed from the provided set.
    pub provided_remove: Option<Vec<String>>,
    /// Package index name for publishing.
    pub index: Option<String>,
    /// Emit SBOMs from `odx build`.
    pub sbom: Option<bool>,
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
    /// Command that prepares the deployment.
    pub prepare: Option<String>,
    /// Command that verifies the deployment.
    pub verify: Option<String>,
}

/// `[tool.odx.environments]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environments {
    /// Layout preset.
    pub preset: Option<Preset>,
    /// Default series of the environments.
    pub series: Option<String>,
    /// Files that differ per environment.
    pub branch_specific: Option<Vec<String>>,
    /// Environments by name.
    #[serde(default)]
    pub env: BTreeMap<String, Environment>,
    /// Allowed promotion edges.
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

/// `[tool.odx.environments.env.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Branch name or pattern with `{series}` and `{major}`.
    pub branch: Option<String>,
    /// Environment role.
    pub role: Option<Role>,
    /// Series of this environment.
    pub series: Option<String>,
    /// Separate repository of this environment.
    pub repo: Option<String>,
    /// Excluded as a promotion target unless named explicitly.
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

/// `[tool.odx.promote]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Promote {
    /// Allow copying an older pin over a newer one.
    pub allow_downgrade: Option<bool>,
    /// Cherry-pick project addon commits by default.
    pub with_code: Option<bool>,
    /// Promote the Odoo core revision together with the pins.
    pub with_odoo: Option<bool>,
    /// Extra synonyms for `--search`.
    #[serde(default)]
    pub search_synonyms: BTreeMap<String, Vec<String>>,
}

/// `[tool.odx.bot]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bot {
    /// Environment that receives bot pull requests.
    pub environment: Option<String>,
    /// Maintain the dashboard issue.
    pub dashboard: Option<bool>,
    /// One pull request per kind per run.
    pub group_prs: Option<bool>,
    /// Maximum pull requests per run.
    pub max_prs_per_run: Option<u16>,
    /// Labels on every bot pull request.
    pub labels: Option<Vec<String>>,
    /// Expiry of mutes made from the dashboard.
    pub mute_expiry: Option<Span>,
    /// Kinds opened without a checkbox.
    pub auto: Option<Vec<BotAction>>,
    /// Label that triggers a promotion pull request.
    pub promote_label: Option<String>,
    /// Vault every observed tracked head.
    pub vault_heads: Option<bool>,
    /// Run on a public project repository.
    pub allow_public: Option<bool>,
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

/// `[tool.odx.dev]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dev {
    /// Virtual environment directory.
    pub venv: Option<PathBuf>,
    /// File listing the addons path.
    pub addons_path_file: Option<PathBuf>,
    /// Odoo configuration file to update.
    pub odoo_conf: Option<PathBuf>,
    /// Use editable installs.
    pub editable: Option<bool>,
    /// Directory for develop-mode clones.
    pub dev_dir: Option<PathBuf>,
}

/// `[tool.odx.lint]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lint {
    /// Paths linted by default.
    pub paths: Option<Vec<PathBuf>>,
    /// Only lint changed addons.
    pub only_changed: Option<bool>,
}

/// `[tool.odx.sniff]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sniff {
    /// Fail when merges introduce migration scripts.
    pub fail_on_new: Option<bool>,
}

/// `[tool.odx.check_deps]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDeps {
    /// Addons the platform provides beyond Odoo's community addons (for
    /// example enterprise addons), counted as available.
    pub provided: Option<Vec<String>>,
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
    fn enum_defaults() {
        assert_eq!(MergeStrategy::default(), MergeStrategy::Rebase);
        assert_eq!(MergirafMode::default(), MergirafMode::Auto);
        assert_eq!(OnConflict::default(), OnConflict::Abort);
        assert_eq!(OnHeadChange::default(), OnHeadChange::FollowIfGreen);
        assert_eq!(RefStyle::default(), RefStyle::Refs);
        assert_eq!(BlacklistMode::default(), BlacklistMode::Auto);
        assert_eq!(MigrateTool::default(), MigrateTool::OdooLint);
        assert_eq!(PlatformProfile::default(), PlatformProfile::GenericWheels);
        assert_eq!(Preset::default(), Preset::MajorBranches);
    }
}
