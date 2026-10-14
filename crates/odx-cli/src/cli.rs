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
#[derive(Debug, Clone, Args)]
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

    /// Serve forge and index data from the cache only. Only network calls are skipped:
    /// local work (git, pre-commit, odoo-lint) takes as long as before.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Ignore cached data; refresh everything before answering.
    #[arg(long, global = true, conflicts_with_all = ["offline", "no_refresh"])]
    pub fresh: bool,

    /// Use cached data even when stale. Like --offline, this saves network time only.
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
    Status(StatusArgs),
    /// Upstream changes on other series that touch your modules.
    Suggest(SuggestArgs),
    /// Record, list, revoke and share decisions about upstream changes.
    #[command(subcommand)]
    Verdict(VerdictCommand),
    /// Compare __manifest__.py dependencies with pyproject.toml.
    CheckDeps(CheckDepsArgs),
    /// List database migration scripts introduced by merges.
    Sniff(SniffArgs),
    /// Lines of code per language.
    Stats(StatsArgs),
    /// Module availability and code readiness for the next Odoo series.
    Upgrade(UpgradeArgs),

    // Porting & environments
    /// Port PRs or missing commits to another series; maintainer sweeps; blacklists.
    Port(PortArgs),
    /// Migrate addons to another Odoo series, the OCA way: history, pre-commit and odoo-lint fixes.
    Migrate(MigrateArgs),
    /// Copy exact pins (and optionally code) of modules or a feature between environments.
    Promote(PromoteArgs),
    /// Compare environments.
    #[command(subcommand)]
    Env(EnvCommand),

    // Automation
    /// Upstream dashboard issue and actionable PRs (nightly CI).
    #[command(subcommand)]
    Bot(BotCommand),
    /// Prefetch the repositories and PR refs of workspaces into the cache.
    Sync(SyncArgs),
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
    /// Run the `custom` profile's prepare hook (also allowed by `[trust]` in the user configuration).
    #[arg(long)]
    pub allow_shell: bool,
}

/// Arguments of `odx build`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Publish the built wheels to the private index.
    #[arg(long)]
    pub publish: bool,
    /// The `[[tool.uv.index]]` to publish to (default: `[tool.odx.platform] index`, else `private`).
    #[arg(long, value_name = "NAME")]
    pub index: Option<String>,
    /// Only addons whose name matches (repeatable, shell-style patterns).
    #[arg(long, value_name = "GLOB")]
    pub addon: Vec<String>,
    /// Also build the project's own addons (from the committed HEAD).
    #[arg(long)]
    pub project: bool,
    /// Where the wheels go (default: `dist/wheels`).
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
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
    /// Check the wheels installed in a virtual environment (or site-packages) against uv.lock.
    #[arg(long, value_name = "DIR")]
    pub installed: Option<PathBuf>,
    /// Check the platform requirements file against the provided-package set.
    #[arg(long)]
    pub requirements: bool,
    /// Check the addon directories of a deployment checkout against the vendor manifest.
    #[arg(long, value_name = "DIR")]
    pub deployed: Option<PathBuf>,
    /// Run the `custom` profile's verify hook (also allowed by `[trust]` in the user configuration).
    #[arg(long)]
    pub allow_shell: bool,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct DevArgs {
    /// Package to switch to an editable local clone.
    pub package: Option<String>,
    /// Put the locked source back (one package, or every one in develop mode).
    #[arg(long)]
    pub restore: bool,
    /// Branch to create or switch to in the package's clone.
    #[arg(
        long,
        value_name = "NAME",
        requires = "package",
        conflicts_with = "restore"
    )]
    pub branch: Option<String>,
    /// Do not install addons in editable mode.
    #[arg(long)]
    pub no_editable: bool,
    /// The virtual environment for editable installs (default: `[tool.odx.dev] venv`, else `.venv`).
    #[arg(long, value_name = "DIR")]
    pub venv: Option<PathBuf>,
    /// Only print the addons path.
    #[arg(long)]
    pub print_addons_path: bool,
    /// Show what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
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
    /// Only these sections (pinned, droppable, new-upstream, forward-port, backport, auto-update, suppressed).
    #[arg(long, value_name = "SECTION")]
    pub section: Vec<String>,
    /// With `--format markdown`, a box per action for the bot to read back.
    #[arg(long)]
    pub checkboxes: bool,
    /// Take an entry's action instead of showing the dashboard.
    #[command(subcommand)]
    pub act: Option<DashboardAct>,
}

/// `odx dashboard act`.
#[derive(Debug, Subcommand)]
pub enum DashboardAct {
    /// Take one entry's action.
    Act(DashboardActArgs),
}

/// Arguments of `odx dashboard act`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per action, as the command line reads"
)]
#[command(group(clap::ArgGroup::new("action").required(true).args(["include", "port", "mute", "drop", "follow"])))]
pub struct DashboardActArgs {
    /// The entry id, as the dashboard shows it.
    pub id: String,
    /// Add the pull request to the project.
    #[arg(long)]
    pub include: bool,
    /// Port the change: cherry-picked onto a branch of the target series in a clone under
    /// the cache.
    #[arg(long)]
    pub port: bool,
    /// With --port: then push the branch and open a draft pull request.
    #[arg(long, requires = "port")]
    pub open_pr: bool,
    /// Record a mute (or, with an organisation kind, a verdict).
    #[arg(long)]
    pub mute: bool,
    /// Remove the override from the project.
    #[arg(long)]
    pub drop: bool,
    /// Take the index update.
    #[arg(long)]
    pub follow: bool,
    /// Why (mandatory with --mute).
    #[arg(long)]
    pub reason: Option<String>,
    /// Kind of the mute or verdict.
    #[arg(long, value_enum, default_value_t = VerdictKind::NotWanted)]
    pub kind: VerdictKind,
}

/// Arguments of `odx suggest`.
#[derive(Debug, Args)]
pub struct SuggestArgs {
    /// Environment whose series is used; defaults to all.
    #[arg(long, value_name = "ENV")]
    pub env: Option<String>,
    /// Target series instead of the environments' (for example `18.0`).
    #[arg(long, value_name = "SERIES", conflicts_with = "env")]
    pub series: Option<String>,
    /// Series to look on (default: `[tool.odx.port] default_from`, else the series below and above).
    #[arg(long, value_name = "SERIES", value_delimiter = ',')]
    pub from: Vec<String>,
    /// Only addons matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub addon: Option<String>,
    /// Include suggestions suppressed by blacklists, verdicts and mutes.
    #[arg(long, visible_alias = "show-suppressed")]
    pub show_muted: bool,
    /// Tell for each portable suggestion whether its commits apply cleanly on the target.
    #[arg(long)]
    pub check_apply: bool,
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

/// Arguments of `odx verdict add`.
#[derive(Debug, Args)]
pub struct VerdictAddArgs {
    /// The suggestion: `<owner>/<repo>#<n>` or `orphaned_commits`.
    pub change: String,
    /// The addon it is about.
    #[arg(long)]
    pub addon: String,
    /// The series it is judged for (default: the project's).
    #[arg(long, value_name = "SERIES")]
    pub target: Option<String>,
    /// The series the suggestion came from, when several have it.
    #[arg(long, value_name = "SERIES", value_delimiter = ',')]
    pub from: Vec<String>,
    /// Kind of verdict.
    #[arg(long, value_enum)]
    pub kind: VerdictKind,
    /// Why; mandatory.
    #[arg(long)]
    pub reason: String,
    /// Scope; the default depends on the kind.
    #[arg(long, value_enum)]
    pub scope: Option<VerdictScope>,
    /// Environments of an environment-scope mute.
    #[arg(long = "env", value_name = "ENV")]
    pub environments: Vec<String>,
    /// Expiry date (YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,
    /// Do not resurface when the target code changes.
    #[arg(long)]
    pub no_recheck: bool,
}

/// `odx verdict` subcommands.
#[derive(Debug, Subcommand)]
pub enum VerdictCommand {
    /// Record a verdict or mute about a suggestion of `odx suggest`.
    Add(VerdictAddArgs),
    /// List verdicts and mutes.
    List {
        /// Only those for this target series.
        #[arg(long, value_name = "SERIES")]
        series: Option<String>,
        /// Only those for this addon.
        #[arg(long)]
        addon: Option<String>,
        /// Only those whose target code changed since.
        #[arg(long)]
        stale: bool,
    },
    /// Show one verdict.
    Show {
        /// Verdict id.
        id: String,
    },
    /// Revoke a verdict (recorded, never deleted).
    Revoke {
        /// Verdict id.
        id: String,
        /// Why; mandatory.
        #[arg(long)]
        reason: String,
    },
    /// Keep a stale verdict after reviewing the changed target code.
    Confirm {
        /// Verdict id.
        id: String,
    },
    /// Propose a verdict upstream as an oca-port blacklist entry: written and committed in
    /// a worktree of the target branch, pushed and proposed with --open-pr --yes.
    Upstream {
        /// Verdict id.
        id: String,
        /// Reason to publish; the private reason never leaves.
        #[arg(long)]
        public_reason: String,
        /// Open the pull request on the upstream repository.
        #[arg(long)]
        open_pr: bool,
        /// Confirm what --open-pr publishes; without it odx shows both reasons and stops.
        #[arg(long, requires = "open_pr")]
        yes: bool,
    },
    /// Pull and push the private verdict repository.
    Sync,
}

/// Arguments of `odx upgrade`.
#[derive(Debug, Args)]
pub struct UpgradeArgs {
    /// Target Odoo series, for example `19.0` (default: `[tool.odx.upgrade] target_odoo_version`).
    #[arg(long, value_name = "SERIES")]
    pub target: Option<String>,
    /// Only addons not available on the target series.
    #[arg(long)]
    pub only_missing: bool,
    /// Skip odoo-lint's readiness counts for the project's own addons.
    #[arg(long)]
    pub no_lint: bool,
}

/// Arguments of `odx migrate`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct MigrateArgs {
    /// What to migrate.
    #[command(subcommand)]
    pub command: Option<MigrateCommand>,
    /// Go on after resolving a conflict in the addon's history.
    #[arg(long = "continue", conflicts_with = "abort")]
    pub resume: bool,
    /// Give up the migration in progress and delete its branch.
    #[arg(long)]
    pub abort: bool,
}

/// `odx migrate` subcommands.
#[derive(Debug, Subcommand)]
pub enum MigrateCommand {
    /// Migrate one addon on the branch `<to>-mig-<addon>`: its history from the source
    /// series, the repository's pre-commit fixes, then odoo-lint's upgrade fixes as the
    /// `[MIG]` commit.
    #[command(after_long_help = "\
The steps, on a new branch `<to>-mig-<addon>` made from `<remote>/<to>` in the clone you run it in:
  1. stop when an open pull request on the forge already migrates the addon (--force, --offline)
  2. replay the addon's history from `<remote>/<from>` (git format-patch | git am -3 --keep)
  3. squash the administrative commits (bots, Weblate) into the commits they follow (--no-squash)
  4. `[IMP] <addon>: pre-commit auto fixes`, only when pre-commit changes something (--no-pre-commit)
  5. `[MIG] <addon>: Migration to <to>`: odoo-lint's upgrade fixes, version bump, old migrations/ removed
Then odx lists what odoo-lint leaves to fix by hand. Nothing is pushed: `odx migrate pr` does that.
No odx project (pyproject.toml with [tool.odx]) is needed.")]
    Addon(MigrateAddonArgs),
    /// Push the migration branch checked out and open its pull request: to the
    /// remote when you may push there, else to your fork (created when missing).
    Pr(MigratePrArgs),
    /// Bring an open migration up to date with what the addon got on the source series
    /// since: each new commit replayed with its author, bot commits left out, then
    /// odoo-lint's fixes for the target in a `[FW]` commit. On the migration branch checked
    /// out, or on someone else's migration pull request.
    Update(MigrateUpdateArgs),
    /// Pin the dependencies of the migration branch's addon that only exist as an open
    /// migration pull request upstream: lines in `test-requirements.txt` pointing at the pull
    /// request, in a commit `[DON'T MERGE] test-requirements.txt`, as OCA asks.
    Pin,
    /// The return trip of a pin: where the pull requests `test-requirements.txt` pins
    /// stand, and once every one is merged and released for the target, the `[DON'T
    /// MERGE]` commit dropped. `--push` sends the branch to its pull request, `--merge`
    /// merges that once its checks are green.
    Unpin(MigrateUnpinArgs),
    /// What moving the project to another series takes, addon by addon: available, an
    /// open migration pull request to review, or to migrate (dependencies first); own
    /// addons apart.
    ///
    /// In a plain clone of an addons repository (no `[tool.odx]` project), the addons of
    /// the repository instead: those on `<remote>/<from>` and not yet on `<remote>/<to>`.
    Plan {
        /// Target series [default: `[tool.odx.upgrade] target_odoo_version`; mandatory
        /// outside a project].
        #[arg(long, value_name = "SERIES")]
        to: Option<String>,
        /// Outside a project: the source series [default: the series before --to].
        #[arg(long, value_name = "SERIES")]
        from: Option<String>,
        /// Outside a project: the remote both series come from [default: `upstream` when
        /// it exists, else `origin`].
        #[arg(long, value_name = "NAME")]
        remote: Option<String>,
    },
    /// Migrate the project's own addons in place: odoo-lint's upgrade fixes and version
    /// bump, old migration scripts removed, one `[MIG]` commit per addon; no pull request.
    Own {
        /// Target series [default: `[tool.odx.upgrade] target_odoo_version`].
        #[arg(long, value_name = "SERIES")]
        to: Option<String>,
        /// Leave out the repository's pre-commit fixes.
        #[arg(long)]
        no_pre_commit: bool,
    },
}

/// Arguments of `odx migrate update`.
#[derive(Debug, Args)]
pub struct MigrateUpdateArgs {
    /// A migration pull request to update instead of the branch checked out, for example
    /// `OCA/contract#1234`; its head becomes the local branch `<to>-mig-<addon>`.
    pub pr: Option<String>,
    /// The source series [default: the one recorded on the branch, else the series before
    /// the target].
    #[arg(long, value_name = "SERIES")]
    pub from: Option<String>,
    /// The remote the source series comes from [default: the one recorded on the branch,
    /// the one pointing at the pull request's repository, else `upstream` or `origin`].
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// The last commit of the source series the branch has, when odx cannot tell (a
    /// migration made in one commit).
    #[arg(long, value_name = "COMMIT")]
    pub since: Option<String>,
    /// Keep the administrative commits as they are.
    #[arg(long)]
    pub no_squash: bool,
    /// Leave out the repository's pre-commit fixes.
    #[arg(long)]
    pub no_pre_commit: bool,
    /// Then push the branch where its pull request comes from.
    #[arg(long)]
    pub push: bool,
}

/// Arguments of `odx migrate unpin`.
#[derive(Debug, Args)]
pub struct MigrateUnpinArgs {
    /// The target series the dependencies must be released for [default: the one the
    /// branch name `<series>-mig-<addon>` starts with].
    #[arg(long, value_name = "SERIES")]
    pub to: Option<String>,
    /// The pull request of the branch, for example `OCA/contract#1234` [default: the one
    /// `odx migrate pr` or `odx migrate update` recorded].
    #[arg(long, value_name = "PR")]
    pub pr: Option<String>,
    /// Only say where the pins stand; change nothing.
    #[arg(long, conflicts_with_all = ["push", "merge"])]
    pub check: bool,
    /// Then push the branch where its pull request comes from.
    #[arg(long)]
    pub push: bool,
    /// Then merge the pull request, when no pin is left, the branch is pushed and its
    /// checks are green. Nothing is merged without this flag.
    #[arg(long)]
    pub merge: bool,
}

/// Arguments of `odx migrate pr`.
#[derive(Debug, Args)]
pub struct MigratePrArgs {
    /// Push to this remote instead of the migrated one or your fork.
    #[arg(long, value_name = "NAME")]
    pub push_remote: Option<String>,
    /// Open the pull request ready for review instead of as a draft.
    #[arg(long)]
    pub ready: bool,
}

/// Arguments of `odx migrate addon`.
#[derive(Debug, Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags are independent switches"
)]
pub struct MigrateAddonArgs {
    /// Addon name.
    pub addon: String,
    /// Target series.
    #[arg(long, value_name = "SERIES")]
    pub to: String,
    /// Source series [default: the series before --to].
    #[arg(long, value_name = "SERIES")]
    pub from: Option<String>,
    /// The remote both series come from [default: `upstream` when it exists, else `origin`].
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// Leave out the repository's pre-commit fixes.
    #[arg(long)]
    pub no_pre_commit: bool,
    /// Keep the administrative commits (bots, Weblate) of the history as they are instead
    /// of squashing them.
    #[arg(long)]
    pub no_squash: bool,
    /// Migrate even when an open pull request already migrates the addon.
    #[arg(long)]
    pub force: bool,
    /// Pin dependencies that only exist as an open migration pull request upstream in
    /// `test-requirements.txt`, the OCA way (`odx migrate pin`).
    #[arg(long)]
    pub pin_unreleased: bool,
    /// Then push the branch and open a draft pull request (`odx migrate pr`).
    #[arg(long)]
    pub open_pr: bool,
}

/// Arguments of `odx port`.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct PortArgs {
    /// What to port.
    #[command(subcommand)]
    pub command: Option<PortCommand>,
    /// Go on after resolving a conflict.
    #[arg(long = "continue", conflicts_with = "abort")]
    pub resume: bool,
    /// Give up the port in progress and delete its branch.
    #[arg(long)]
    pub abort: bool,
}

/// What `odx port pr` and `odx port addon` share.
#[derive(Debug, Args)]
pub struct PortOptions {
    /// Target series.
    #[arg(long, value_name = "SERIES")]
    pub to: String,
    /// The remote the series come from [default: the one pointing at the pull request's
    /// repository; for addons `upstream` when it exists, else `origin`].
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// Leave out odoo-lint's upgrade fixes.
    #[arg(long)]
    pub no_migrate: bool,
    /// Then push the branch and open a draft pull request.
    #[arg(long)]
    pub open_pr: bool,
    /// With --open-pr: push to this remote instead of the target or your fork.
    #[arg(long, value_name = "NAME", requires = "open_pr")]
    pub push_remote: Option<String>,
}

/// Arguments of `odx port sweep`.
#[derive(Debug, Args)]
pub struct SweepArgs {
    /// Target series.
    #[arg(long, value_name = "SERIES")]
    pub to: String,
    /// Source series, comma-separated or repeated.
    #[arg(long, value_name = "SERIES", required = true, value_delimiter = ',')]
    pub from: Vec<String>,
    /// The remote the series come from [default: `upstream` when it exists, else `origin`].
    #[arg(long, value_name = "NAME")]
    pub remote: Option<String>,
    /// Only the addons matching this glob.
    #[arg(long, value_name = "GLOB")]
    pub addon: Option<String>,
    /// Blacklist the false positives (oca-port files) in commits on the branch
    /// `<to>-sweep`.
    #[arg(long)]
    pub write_blacklist: bool,
    /// Then push the branch and open a draft pull request.
    #[arg(long, requires = "write_blacklist")]
    pub open_pr: bool,
    /// With --open-pr: push to this remote instead of the target or your fork.
    #[arg(long, value_name = "NAME", requires = "open_pr")]
    pub push_remote: Option<String>,
}

/// `odx port` subcommands.
#[derive(Debug, Subcommand)]
pub enum PortCommand {
    /// Port one pull request to another series: its commits cherry-picked with their
    /// authors onto the branch `<to>-port-<n>`, then odoo-lint's upgrade fixes.
    Pr {
        /// Pull request, for example `OCA/contract#1476`.
        pr: String,
        /// Where to, and how.
        #[command(flatten)]
        options: PortOptions,
    },
    /// Port the commits an addon has on another series and lacks on the target, grouped
    /// by pull request, on the branch `<to>-port-<addon>`.
    Addon {
        /// Addon name.
        addon: String,
        /// Source series.
        #[arg(long, value_name = "SERIES")]
        from: String,
        /// Where to, and how.
        #[command(flatten)]
        options: PortOptions,
    },
    /// Add an entry to an addon's oca-port blacklist and commit it on the branch checked
    /// out: `.oca/oca-port/blacklist/<addon>.json`, written as oca-port writes it.
    Blacklist {
        /// Addon name.
        addon: String,
        /// The pull request, for example `OCA/contract#1201`, or `orphaned` for the addon's
        /// orphaned commits.
        entry: String,
        /// Why it is not ported; mandatory.
        #[arg(long)]
        reason: String,
    },
    /// Check every addon of the target series against older series (maintainer mode):
    /// portable pull requests and false positives (migration commits, repo-wide changes,
    /// translations).
    Sweep(SweepArgs),
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
    Install {
        /// Time between syncs (default: `[sync] interval`, else 30m).
        #[arg(long, value_name = "DURATION")]
        interval: Option<String>,
        /// Print the job files and commands without installing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the scheduled sync job.
    Uninstall,
    /// Run the sync loop in the foreground.
    Run {
        /// Time between syncs (default: `[sync] interval`, else 30m).
        #[arg(long, value_name = "DURATION")]
        interval: Option<String>,
    },
    /// Show the scheduled job and last sync.
    Status,
}

/// `odx cache` subcommands.
#[derive(Debug, Subcommand)]
pub enum CacheCommand {
    /// Show cache location, size and freshness.
    Info,
    /// Remove repositories no registered workspace uses, least recently used first.
    Gc {
        /// Keep the cache under this size (default: `[cache] max_size`; without one, remove every unused repository).
        #[arg(long, value_name = "SIZE")]
        max_size: Option<String>,
        /// Show what would be removed.
        #[arg(long)]
        dry_run: bool,
    },
    /// Copy what cache repositories borrow from local clones into the cache, so the clones can go.
    Detach {
        /// Only this repository: a name in this project or a URL.
        repo: Option<String>,
    },
}

/// Arguments of `odx sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Register this workspace for `odx sync --all` and stop.
    #[arg(long, conflicts_with_all = ["unregister", "all", "workspace"])]
    pub register: bool,
    /// Remove this workspace from the registry and stop.
    #[arg(long, conflicts_with_all = ["all", "workspace"])]
    pub unregister: bool,
    /// Sync every registered workspace.
    #[arg(long, conflicts_with = "workspace")]
    pub all: bool,
    /// Sync these workspaces (default: the one here).
    #[arg(long, value_name = "DIR")]
    pub workspace: Vec<PathBuf>,
    /// Stop starting new workspaces after this long (default: `[sync] max_duration`, else 20m).
    #[arg(long, value_name = "DURATION")]
    pub max_duration: Option<String>,
}

/// Arguments of `odx status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Order of the pull requests.
    #[arg(long, value_enum, default_value_t = StatusSort::Name)]
    pub sort_by: StatusSort,
    /// Exit 1 when a pull request is merged upstream (for CI).
    #[arg(long)]
    pub fail_on_merged: bool,
}

/// How `odx status` orders pull requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StatusSort {
    /// By repository or package, then number.
    Name,
    /// Merged first, then closed, open and unknown.
    Status,
    /// By merge date, oldest first; unmerged last.
    Date,
}

/// Arguments of `odx check-deps`.
#[derive(Debug, Args)]
pub struct CheckDepsArgs {
    /// Add missing addons the package index has to `[tool.odx.groups] default`.
    #[arg(long)]
    pub fix: bool,
    /// Also report declared addon packages no scanned addon depends on.
    #[arg(long)]
    pub unused: bool,
    /// Also check licenses along `depends` (AGPL and GPL need compatible dependers).
    #[arg(long)]
    pub licenses: bool,
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
            Self::Status(_) => "status",
            Self::Suggest(_) => "suggest",
            Self::Verdict(_) => "verdict",
            Self::CheckDeps(_) => "check-deps",
            Self::Sniff(_) => "sniff",
            Self::Stats(_) => "stats",
            Self::Upgrade(_) => "upgrade",
            Self::Port(_) => "port",
            Self::Migrate(_) => "migrate",
            Self::Promote(_) => "promote",
            Self::Env(_) => "env",
            Self::Bot(_) => "bot",
            Self::Sync(_) => "sync",
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
            | Self::Sync(_)
            | Self::Daemon(_)
            | Self::Cache(_)
            | Self::Doctor
            | Self::Auth(_) => 6,
            Self::Dashboard(_)
            | Self::Status(_)
            | Self::Suggest(_)
            | Self::Verdict(_)
            | Self::CheckDeps(_)
            | Self::Upgrade(_) => 7,
            Self::Port(_) | Self::Migrate(_) => 8,
            Self::Lint(_) => 9,
            Self::Promote(_) | Self::Env(_) => 10,
            Self::Bot(_) => 11,
        }
    }
}
