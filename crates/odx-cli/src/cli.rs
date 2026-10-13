//! The `odx` command-line interface definition.
//!
//! Every command from the design is present so that help output, shell
//! completions and editor integrations can be built against the final
//! shape. Commands report the roadmap phase in which they land until they
//! are implemented.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// Top-level `odx` arguments.
#[derive(Debug, Parser)]
#[command(
    name = "odx",
    version,
    about = "Rust-native workspace manager for Odoo projects",
    long_about = "Rust-native workspace manager for Odoo projects.\n\n\
        Aggregates pull requests, locks and vaults every commit, ports fixes \
        across Odoo series and promotes exact pins between environments, \
        with pyproject.toml [tool.odx] as the single source of truth.",
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Options accepted by every command.
    #[command(flatten)]
    pub global: GlobalArgs,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Options accepted by every command.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct GlobalArgs {
    /// Run as if started in DIR.
    #[arg(short = 'C', long, global = true, value_name = "DIR")]
    pub project_dir: Option<PathBuf>,

    /// The pyproject.toml or repos.yaml to use.
    #[arg(short, long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Override the number of parallel jobs.
    #[arg(short, long, global = true, value_name = "N")]
    pub jobs: Option<usize>,

    /// Serve forge and index data from the cache only.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Ignore cached data; refresh everything before answering.
    #[arg(long, global = true, conflicts_with_all = ["offline", "no_refresh"])]
    pub fresh: bool,

    /// Use cached data even when stale.
    #[arg(long, global = true)]
    pub no_refresh: bool,

    /// Rebuild the addon index for the series involved.
    #[arg(long, global = true)]
    pub refresh_index: bool,

    /// Machine-readable output on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Increase log verbosity (repeatable).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Errors only.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
}

/// All `odx` commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    // Workspace & dependencies
    /// Fetch and merge the configured PRs and branches.
    Aggregate(AggregateArgs),
    /// Resolve every merge and package to a SHA, vault it, write odx.lock.
    Lock(LockArgs),
    /// Install the locked packages into the addon directories; write the platform requirements.
    Vendor(VendorArgs),
    /// Build (and publish) wheels of aggregated, unmerged and private addons.
    Build(BuildArgs),
    /// Check that the working tree, lockfiles, vendored content or installed wheels match.
    Verify(VerifyArgs),
    /// Prepare a local development workspace, or develop one package.
    Dev(DevArgs),
    /// Add a repository, merge or package to pyproject.toml.
    Add(AddArgs),
    /// Remove a merge or package from pyproject.toml and odx.lock.
    Drop(DropArgs),
    /// Convert repos.yaml (and addons.yaml) into [tool.odx].
    Import(ImportArgs),
    /// Write repos.yaml / addons.yaml from [tool.odx].
    Export(ExportArgs),
    /// Run the gitaggregate-compatible drop-in (same as `git-aggregate-rs`).
    GitAggregate(git_aggregate_rs::Cli),

    // Analysis
    /// One combined view of pinned PRs, upstream changes, ports, updates and drift.
    Dashboard(DashboardArgs),
    /// Aggregation and upstream status per merge and package.
    #[command(visible_alias = "track")]
    Status,
    /// Upstream changes on other series that touch your modules.
    Suggest(SuggestArgs),
    /// Record, list, revoke and share decisions about upstream changes.
    #[command(subcommand)]
    Verdict(VerdictCommand),
    /// Compare __manifest__.py dependencies with pyproject.toml.
    CheckDeps,
    /// List database migration scripts introduced by merges.
    Sniff(SniffArgs),
    /// Lines of code per language.
    Stats(StatsArgs),
    /// Module availability and code readiness for the next Odoo series.
    Upgrade(UpgradeArgs),

    // Porting & environments
    /// Port PRs or missing commits to another series; maintainer sweeps; blacklists.
    #[command(subcommand)]
    Port(PortCommand),
    /// Copy exact pins (and optionally code) of modules or a feature between environments.
    Promote(PromoteArgs),
    /// Compare environments.
    #[command(subcommand)]
    Env(EnvCommand),

    // Automation
    /// Upstream dashboard issue and actionable PRs (nightly CI).
    #[command(subcommand)]
    Bot(BotCommand),
    /// Prefetch repositories, PR refs, index metadata and wheels into the cache.
    Sync,
    /// Run sync continuously or install it as a user-level scheduled job.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// Inspect and garbage-collect the shared cache.
    #[command(subcommand)]
    Cache(CacheCommand),

    // Quality & tooling
    /// Odoo-specific lint checks with project context.
    Lint(LintArgs),
    /// Check the local setup: git, mergiraf, uv, tokens, cache, vault protection.
    Doctor,
    /// Store or remove forge tokens in the OS keyring.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Print shell completions.
    Completions(CompletionsArgs),
}

/// Arguments of `odx aggregate`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct AggregateArgs {
    /// Only fill the cache; do not touch working trees.
    #[arg(long)]
    pub fetch_only: bool,
    /// Only this repository (its `name`, or its URL when unnamed).
    #[arg(long, value_name = "NAME")]
    pub repo: Option<String>,
    /// Fetch locked commits from the configured mirror first (default in CI).
    #[arg(long)]
    pub prefer_mirror: bool,
    /// Keep conflicted working trees for inspection instead of aborting.
    #[arg(long)]
    pub keep_conflicts: bool,
    /// Discard local changes in working trees and replace directories odx does not manage.
    #[arg(long)]
    pub force: bool,
    /// Fetch locked commits from the git bundles `odx lock --bundle` wrote here first.
    #[arg(long, value_name = "DIR")]
    pub bundle_dir: Option<PathBuf>,
    /// Fail instead of resolving anything odx.lock does not pin; never write odx.lock.
    #[arg(long, conflicts_with = "update")]
    pub locked: bool,
    /// Resolve every branch and pull request again instead of using odx.lock.
    #[arg(long)]
    pub update: bool,
}

/// Arguments of `odx lock`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct LockArgs {
    /// Only this repository (its `name`, or its URL when unnamed).
    #[arg(long, value_name = "NAME")]
    pub repo: Option<String>,
    /// Rewrite odx.lock in the current format and stop.
    #[arg(long, conflicts_with_all = ["check", "update"])]
    pub migrate: bool,
    /// Fail if odx.lock would change.
    #[arg(long, conflicts_with = "update")]
    pub check: bool,
    /// Re-resolve every entry instead of keeping existing pins.
    #[arg(long)]
    pub update: bool,
    /// Adopt this pull request's new head even when its CI is not green (`<repo>#<pr>`).
    #[arg(long, value_name = "REPO#PR")]
    pub follow: Vec<String>,
    /// Also write one git bundle per repository with its locked commits into DIR.
    #[arg(long, value_name = "DIR")]
    pub bundle: Option<PathBuf>,
    /// Do not run `uv lock` after rewriting package sources.
    #[arg(long)]
    pub no_uv: bool,
    /// Do not push locked commits to the configured vault.
    #[arg(long)]
    pub no_push: bool,
    /// Apply the configured lock auto-update policy to PyPI packages.
    #[arg(long)]
    pub auto_update: bool,
}

/// Arguments of `odx vendor`.
#[derive(Debug, Args)]
pub struct VendorArgs {
    /// Write the platform requirements file (default: `[tool.odx.layout] requirements`).
    #[arg(long, value_name = "FILE", num_args = 0..=1)]
    pub requirements: Option<Option<PathBuf>>,
    /// Fail when a requirement excludes the version the platform provides.
    #[arg(long)]
    pub strict: bool,
}

/// Arguments of `odx build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Publish the built wheels to the private index.
    #[arg(long)]
    pub publish: bool,
    /// Emit a CycloneDX SBOM per wheel.
    #[arg(long)]
    pub sbom: bool,
}

/// Arguments of `odx verify`.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Check the vendored addon directories against the vendor manifest.
    #[arg(long)]
    pub vendored: bool,
    /// Check installed wheels against uv.lock and odx.lock.
    #[arg(long)]
    pub installed: bool,
    /// Check the platform requirements file against the provided-package set.
    #[arg(long)]
    pub requirements: bool,
}

/// Arguments of `odx stats`.
#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Also count the aggregated and vendored addons.
    #[arg(long)]
    pub include_vendor: bool,
    /// One row per addon instead of per language.
    #[arg(long)]
    pub per_addon: bool,
}

/// Arguments of `odx sniff`.
#[derive(Debug, Args)]
pub struct SniffArgs {
    /// Compare with odx.lock as committed at this git revision instead of
    /// each repository's base.
    #[arg(long, value_name = "REV")]
    pub since: Option<String>,
    /// Fail when migration scripts are found (like `[tool.odx.sniff] fail_on_new`).
    #[arg(long)]
    pub fail_on_new: bool,
}

/// Arguments of `odx dev`.
#[derive(Debug, Args)]
pub struct DevArgs {
    /// Package to switch to an editable local clone.
    pub package: Option<String>,
    /// Restore the package to its locked version.
    #[arg(long, requires = "package")]
    pub restore: bool,
}

/// Arguments of `odx add`.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// `owner/repo#PR`, `owner/repo@REF`, a pull request URL, or `URL#PR` / `URL@REF`.
    pub spec: String,
    /// Add as package DIST instead of as a merge.
    #[arg(long, value_name = "DIST")]
    pub package: Option<String>,
    /// Subdirectory of the addon in the repository (packages).
    #[arg(long, value_name = "DIR", requires = "package")]
    pub subdirectory: Option<String>,
    /// Dependency group of the package.
    #[arg(long, value_name = "GROUP", requires = "package")]
    pub group: Option<String>,
    /// Odoo series for a new [tool.odx] table.
    #[arg(long, value_name = "SERIES")]
    pub odoo_version: Option<String>,
}

/// Arguments of `odx drop`.
#[derive(Debug, Args)]
pub struct DropArgs {
    /// The merge (`owner/repo#PR`, ...) or, with `--package`, the distribution name.
    pub spec: String,
    /// Drop package SPEC instead of a merge.
    #[arg(long)]
    pub package: bool,
    /// Why the merge or package is dropped.
    #[arg(long, value_name = "TEXT")]
    pub reason: Option<String>,
}

/// Arguments of `odx import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// The repos.yaml to convert.
    #[arg(default_value = "repos.yaml")]
    pub repos: PathBuf,
    /// The addons.yaml to read; defaults to addons.yaml next to the repos file.
    pub addons: Option<PathBuf>,
    /// Write the result into pyproject.toml instead of printing it.
    #[arg(long)]
    pub write: bool,
    /// Odoo series for a new [tool.odx] table; defaults to the first base branch.
    #[arg(long, value_name = "SERIES", requires = "write")]
    pub odoo_version: Option<String>,
}

/// Arguments of `odx export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// The repos.yaml to write, relative to the project root.
    #[arg(default_value = "repos.yaml")]
    pub repos: PathBuf,
    /// The addons.yaml to write, relative to the project root.
    #[arg(default_value = "addons.yaml")]
    pub addons: PathBuf,
    /// Print repos.yaml (and addons.yaml) instead of writing files.
    #[arg(long)]
    pub stdout: bool,
}

/// Output formats of `odx dashboard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DashboardFormat {
    /// Terminal table.
    Table,
    /// Interactive terminal UI.
    Tui,
    /// Markdown, as used for the dashboard issue.
    Markdown,
}

/// Arguments of `odx dashboard`.
#[derive(Debug, Args)]
pub struct DashboardArgs {
    /// Output format (`--json` overrides it).
    #[arg(long, value_enum, default_value_t = DashboardFormat::Table)]
    pub format: DashboardFormat,
    /// Environment to show; defaults to all.
    #[arg(long, value_name = "ENV")]
    pub env: Option<String>,
}

/// Arguments of `odx suggest`.
#[derive(Debug, Args)]
pub struct SuggestArgs {
    /// Environment whose series is used; defaults to all.
    #[arg(long, value_name = "ENV")]
    pub env: Option<String>,
    /// Include suggestions suppressed by verdicts and mutes.
    #[arg(long)]
    pub show_muted: bool,
}

/// Verdict kinds; every verdict also needs a free-text reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerdictKind {
    /// The change does not apply to the target series.
    NotApplicable,
    /// The change is not needed on the target series.
    NotNeeded,
    /// Superseded by another change.
    Superseded,
    /// Already present on the target, possibly by another route.
    AlreadyPresent,
    /// Declined for this project.
    Declined,
    /// Not wanted in this project.
    NotWanted,
    /// Review later; re-checked on expiry and on target code changes.
    Deferred,
}

/// Verdict scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerdictScope {
    /// One environment of this project.
    Environment,
    /// Every environment of this project.
    Project,
    /// Every project of the organisation.
    Organisation,
}

/// `odx verdict` subcommands.
#[derive(Debug, Subcommand)]
pub enum VerdictCommand {
    /// Record a verdict or mute.
    Add {
        /// Suggestion or change id, as shown by `odx dashboard`.
        id: String,
        /// Kind of verdict.
        #[arg(long, value_enum)]
        kind: VerdictKind,
        /// Why; mandatory.
        #[arg(long)]
        reason: String,
        /// Scope; the default depends on the kind.
        #[arg(long, value_enum)]
        scope: Option<VerdictScope>,
        /// Expiry date (YYYY-MM-DD) for deferred verdicts.
        #[arg(long, value_name = "DATE")]
        until: Option<String>,
    },
    /// List verdicts and mutes.
    List,
    /// Show one verdict.
    Show {
        /// Verdict id.
        id: String,
    },
    /// Revoke a verdict (recorded, never deleted).
    Revoke {
        /// Verdict id.
        id: String,
    },
    /// Propose a verdict upstream as an oca-port blacklist entry.
    Upstream {
        /// Verdict id.
        id: String,
        /// Reason to publish; the private reason never leaves.
        #[arg(long)]
        public_reason: String,
        /// Open the pull request on the upstream repository.
        #[arg(long)]
        open_pr: bool,
    },
    /// Pull and push the private verdict repository.
    Sync,
}

/// Arguments of `odx upgrade`.
#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Target Odoo series, for example `19.0`.
    #[arg(long, value_name = "SERIES")]
    pub target: String,
}

/// `odx port` subcommands.
#[derive(Debug, Subcommand)]
pub enum PortCommand {
    /// Port one pull request to another series.
    Pr {
        /// Pull request, for example `OCA/contract#1476`.
        pr: String,
        /// Target series.
        #[arg(long, value_name = "SERIES")]
        to: String,
        /// Push to your fork and open the pull request.
        #[arg(long)]
        open_pr: bool,
    },
    /// Port an addon's missing commits from another series.
    Addon {
        /// Addon name.
        addon: String,
        /// Source series.
        #[arg(long, value_name = "SERIES")]
        from: String,
        /// Target series.
        #[arg(long, value_name = "SERIES")]
        to: String,
    },
    /// Sweep every addon of a repository against older series (maintainer mode).
    Sweep {
        /// Target series.
        #[arg(long, value_name = "SERIES")]
        to: String,
        /// Source series; repeatable.
        #[arg(long, value_name = "SERIES", required = true)]
        from: Vec<String>,
        /// Write oca-port blacklist files for the false positives.
        #[arg(long)]
        write_blacklist: bool,
    },
}

/// Arguments of `odx promote`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct PromoteArgs {
    /// Modules to promote, with their dependency closure.
    #[arg(required_unless_present_any = ["feature", "search"])]
    pub modules: Vec<String>,
    /// Promote a feature set from [tool.odx.features].
    #[arg(long, value_name = "NAME")]
    pub feature: Option<String>,
    /// Find modules by functional description and show candidates.
    #[arg(long, value_name = "TEXT")]
    pub search: Option<String>,
    /// Source environment.
    #[arg(long, value_name = "ENV")]
    pub from: Option<String>,
    /// Target environment.
    #[arg(long, value_name = "ENV")]
    pub to: Option<String>,
    /// Also promote code by patch-id.
    #[arg(long)]
    pub with_code: bool,
    /// Do not promote the Odoo core revision.
    #[arg(long)]
    pub no_with_odoo: bool,
    /// Show the plan without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply without confirmation.
    #[arg(long, conflicts_with = "dry_run")]
    pub yes: bool,
}

/// `odx env` subcommands.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Compare the pins of two environments.
    Diff {
        /// First environment.
        a: String,
        /// Second environment.
        b: String,
    },
}

/// `odx bot` subcommands.
#[derive(Debug, Subcommand)]
pub enum BotCommand {
    /// Update the dashboard issue and open actionable pull requests.
    Run {
        /// Write to the forge; without it the run is a dry run.
        #[arg(long)]
        apply: bool,
        /// Maximum number of pull requests to open in this run.
        #[arg(long, value_name = "N")]
        max_prs: Option<usize>,
    },
}

/// `odx daemon` subcommands.
#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Install the user-level scheduled sync job.
    Install,
    /// Remove the scheduled sync job.
    Uninstall,
    /// Run the sync loop in the foreground.
    Run,
    /// Show the scheduled job and last sync.
    Status,
}

/// `odx cache` subcommands.
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Show cache location, size and freshness.
    Info,
    /// Remove least recently used data above the configured size.
    Gc,
}

/// Arguments of `odx lint`.
#[derive(Debug, Args)]
pub struct LintArgs {
    /// Paths to lint; defaults to the project addons.
    pub paths: Vec<PathBuf>,
}

/// `odx auth` subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store a token for a forge host.
    Login {
        /// Forge host, for example `github.com`.
        host: String,
    },
    /// Remove the token for a forge host.
    Logout {
        /// Forge host.
        host: String,
    },
    /// Show which token source is used per host.
    Status,
}

/// Arguments of `odx completions`.
#[derive(Debug, Args)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

impl Command {
    /// The command name as typed on the command line.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Aggregate(_) => "aggregate",
            Self::Lock(_) => "lock",
            Self::Vendor(_) => "vendor",
            Self::Build(_) => "build",
            Self::Verify(_) => "verify",
            Self::Dev(_) => "dev",
            Self::Add(_) => "add",
            Self::Drop(_) => "drop",
            Self::Import(_) => "import",
            Self::Export(_) => "export",
            Self::GitAggregate(_) => "git-aggregate",
            Self::Dashboard(_) => "dashboard",
            Self::Status => "status",
            Self::Suggest(_) => "suggest",
            Self::Verdict(_) => "verdict",
            Self::CheckDeps => "check-deps",
            Self::Sniff(_) => "sniff",
            Self::Stats(_) => "stats",
            Self::Upgrade(_) => "upgrade",
            Self::Port(_) => "port",
            Self::Promote(_) => "promote",
            Self::Env(_) => "env",
            Self::Bot(_) => "bot",
            Self::Sync => "sync",
            Self::Daemon(_) => "daemon",
            Self::Cache(_) => "cache",
            Self::Lint(_) => "lint",
            Self::Doctor => "doctor",
            Self::Auth(_) => "auth",
            Self::Completions(_) => "completions",
        }
    }

    /// The roadmap phase in which the command is implemented.
    pub const fn phase(&self) -> u8 {
        match self {
            Self::Completions(_) => 0,
            Self::Add(_) | Self::Drop(_) | Self::Import(_) | Self::Export(_) => 1,
            Self::Aggregate(_) | Self::GitAggregate(_) => 3,
            Self::Lock(_) | Self::Verify(_) => 4,
            Self::Vendor(_) | Self::Build(_) | Self::Stats(_) | Self::Sniff(_) => 5,
            Self::Dev(_)
            | Self::Sync
            | Self::Daemon(_)
            | Self::Cache(_)
            | Self::Doctor
            | Self::Auth(_) => 6,
            Self::Dashboard(_)
            | Self::Status
            | Self::Suggest(_)
            | Self::Verdict(_)
            | Self::CheckDeps
            | Self::Upgrade(_) => 7,
            Self::Port(_) => 8,
            Self::Lint(_) => 9,
            Self::Promote(_) | Self::Env(_) => 10,
            Self::Bot(_) => 11,
        }
    }
}
