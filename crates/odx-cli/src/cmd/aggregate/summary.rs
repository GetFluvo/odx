//! What a run of `odx aggregate` or `odx lock` reports, as text and as JSON.

use std::fmt::Write as _;
use std::path::Path;

use odx_aggregate::{Change, ChangeKind, Planned};
use odx_config::lock::{CiState as LockCi, RedClassification};
use odx_core::Exit;
use odx_git::{Aggregated, Fetched, StepOutcome, Via};
use schemars::JsonSchema;
use serde::Serialize;

use super::super::Report;
use super::super::sniff::{Sniffed, SniffedJson, describe as describe_sniffed, sniffed_json};
use super::{Built, Kept};

/// `dir` relative to `root`, with `/` between its parts on every platform.
pub(super) fn relative(dir: &Path, root: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .display()
        .to_string()
        .replace('\\', "/")
}

/// The outcome of a run: per repository URL (fetch), per repository
/// (build or already current), and the changes to `odx.lock`.
pub(super) struct Summary {
    pub(super) fetches: Vec<(String, Result<Vec<Fetched>, String>)>,
    pub(super) builds: Vec<(String, String, Result<Aggregated, String>)>,
    pub(super) current: Vec<(String, String)>,
    pub(super) changes: Vec<Change>,
    pub(super) vaulted: Vec<(String, usize)>,
    pub(super) kept: Vec<Kept>,
    pub(super) bundled: Vec<(String, usize, String)>,
    pub(super) notes: Vec<String>,
    pub(super) sniffed: Vec<Sniffed>,
    pub(super) rewritten: bool,
    pub(super) failures: Vec<String>,
    /// Built in scratch working trees (`odx lock`): directories are not
    /// shown.
    pub(super) scratch: bool,
}

impl Summary {
    pub(super) fn new(built: &Built, planned: &[Planned], root: &Path, scratch: bool) -> Self {
        let fetches = built
            .plans
            .iter()
            .zip(&built.outcomes)
            .map(|(plan, outcome)| {
                let result = outcome
                    .result
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(ToString::to_string);
                (plan.names.join(", "), result)
            })
            .collect();
        // Fetch failures are reported with the fetches; a loaded
        // configuration has only valid merges, so what was planned is all
        // that is built, one result each, in order.
        let builds = built
            .pairs(planned)
            .map(|(repo, aggregation, result)| {
                let result = match result {
                    Ok(aggregated) => Ok(aggregated.clone()),
                    Err(e) => Err(e.to_string()),
                };
                (
                    repo.name.clone(),
                    relative(&aggregation.target_dir, root),
                    result,
                )
            })
            .collect();
        Self {
            fetches,
            builds,
            current: Vec::new(),
            changes: Vec::new(),
            vaulted: Vec::new(),
            kept: Vec::new(),
            bundled: Vec::new(),
            notes: Vec::new(),
            sniffed: Vec::new(),
            rewritten: false,
            failures: Vec::new(),
            scratch,
        }
    }

    pub(super) fn failed(&self) -> bool {
        !self.failures.is_empty()
            || self.fetches.iter().any(|(_, r)| r.is_err())
            || self.builds.iter().any(|(_, _, r)| r.is_err())
    }

    pub(super) fn report(
        &self,
        json: bool,
        fetch_only: bool,
        warnings: Vec<odx_config::Problem>,
    ) -> Report {
        let mut report = if json {
            Report {
                out: super::super::to_json(&self.to_json(fetch_only)),
                ..Report::default()
            }
        } else {
            Report {
                warnings,
                ..self.to_text(fetch_only)
            }
        };
        if self.failed() {
            report.exit = Exit::Failure;
        }
        report
    }

    fn to_text(&self, fetch_only: bool) -> Report {
        let mut report = Report::default();
        let mut failures = Vec::new();
        for (names, result) in &self.fetches {
            match result {
                Ok(fetched) => {
                    let count = |via| fetched.iter().filter(|f| f.via == via).count();
                    let _ = write!(report.out, "fetched {names}: {} refs", fetched.len());
                    for (via, label) in [
                        (Via::Mirror, "from the mirror"),
                        (Via::Clone, "through the local clone"),
                        (Via::Cache, "already cached"),
                    ] {
                        let n = count(via);
                        if n > 0 {
                            let _ = write!(report.out, " ({n} {label})");
                        }
                    }
                    report.out.push('\n');
                }
                Err(e) => failures.push(format!("{names}: {e}")),
            }
        }
        if !fetch_only {
            for (name, dir) in &self.current {
                let _ = writeln!(report.out, "up to date {name} in {dir} (locked)");
            }
            for (name, dir, result) in &self.builds {
                match result {
                    Ok(built) if self.scratch => {
                        let _ = writeln!(
                            report.out,
                            "resolved {name}: {} ({})",
                            built.describe(),
                            short(&built.head)
                        );
                    }
                    Ok(built) => {
                        let _ = writeln!(
                            report.out,
                            "built {name} in {dir}: {} ({})",
                            built.describe(),
                            short(&built.head)
                        );
                    }
                    Err(e) => failures.push(format!("{name}: {e}")),
                }
            }
        }
        report.out.push_str(&describe_sniffed(&self.sniffed));
        for change in &self.changes {
            let _ = writeln!(report.out, "{}", describe_change(change));
        }
        if self.rewritten {
            report
                .out
                .push_str("pointed packages at the vault in pyproject.toml\n");
        }
        for (url, commits, file) in &self.bundled {
            let _ = writeln!(report.out, "bundled {commits} commits of {url} into {file}");
        }
        for keep in &self.kept {
            let _ = writeln!(report.out, "{}", describe_kept(keep));
        }
        for note in &self.notes {
            let _ = writeln!(report.out, "{note}");
        }
        for (name, commits) in &self.vaulted {
            let _ = writeln!(report.out, "vaulted {name}: {commits} commits");
        }
        failures.extend(self.failures.iter().cloned());
        if !failures.is_empty() {
            let what = if self.scratch {
                "locking"
            } else {
                "aggregation"
            };
            report.error = Some(format!("{what} failed for\n  {}", failures.join("\n  ")));
        }
        report
    }

    fn built_json(&self) -> Vec<BuiltJson<'_>> {
        self.builds
            .iter()
            .map(|(name, dir, result)| match result {
                Ok(built) => BuiltJson::Built {
                    name,
                    target_dir: (!self.scratch).then_some(dir.as_str()),
                    head: &built.head,
                    tree: &built.tree,
                    merges: built
                        .steps
                        .iter()
                        .map(|(label, outcome)| MergeJson {
                            label,
                            status: outcome.status(),
                            files: match outcome {
                                StepOutcome::Skipped { files } => Some(files),
                                _ => None,
                            },
                        })
                        .collect(),
                },
                Err(e) => BuiltJson::Failed {
                    name,
                    target_dir: (!self.scratch).then_some(dir.as_str()),
                    error: e,
                },
            })
            .collect()
    }

    fn to_json(&self, fetch_only: bool) -> AggregateJson<'_> {
        AggregateJson {
            schema: 1,
            fetched: self
                .fetches
                .iter()
                .map(|(names, result)| match result {
                    Ok(fetched) => FetchedJson::Fetched {
                        repositories: names,
                        refs: fetched.len(),
                        from_mirror: fetched.iter().filter(|f| f.via == Via::Mirror).count(),
                        through_clone: fetched.iter().filter(|f| f.via == Via::Clone).count(),
                        cached: fetched.iter().filter(|f| f.via == Via::Cache).count(),
                    },
                    Err(e) => FetchedJson::Failed {
                        repositories: names,
                        error: e,
                    },
                })
                .collect(),
            built: (!fetch_only).then(|| self.built_json()),
            current: (!fetch_only).then(|| {
                self.current
                    .iter()
                    .map(|(name, dir)| CurrentJson {
                        name,
                        target_dir: dir,
                    })
                    .collect()
            }),
            lock: self
                .changes
                .iter()
                .map(|change| LockChangeJson {
                    repository: &change.repository,
                    item: &change.item,
                    change: change.kind.as_str(),
                    old: change.old.as_deref(),
                    new: change.new.as_deref(),
                })
                .collect(),
            vaulted: self
                .vaulted
                .iter()
                .map(|(name, commits)| VaultedJson {
                    repository: name,
                    commits: *commits,
                })
                .collect(),
            kept: self
                .kept
                .iter()
                .map(|keep| KeptJson {
                    repository: &keep.repository,
                    item: &keep.label,
                    locked: &keep.locked,
                    upstream_head: &keep.head,
                    ci: keep.ci.map(LockCi::as_str),
                    classification: keep.classification.map(RedClassification::as_str),
                })
                .collect(),
            rewritten: self.rewritten,
            notes: &self.notes,
            migrations: sniffed_json(&self.sniffed),
            bundled: self
                .bundled
                .iter()
                .map(|(url, commits, file)| BundledJson {
                    url,
                    commits: *commits,
                    file,
                })
                .collect(),
            errors: &self.failures,
        }
    }
}

/// What `odx --json aggregate` and `odx --json lock` print.
#[derive(Debug, Serialize, JsonSchema)]
#[schemars(title = "odx aggregate")]
pub(crate) struct AggregateJson<'a> {
    /// The version of this layout.
    #[schemars(extend("const" = 1))]
    schema: u32,
    /// Per fetch, the repositories it served.
    fetched: Vec<FetchedJson<'a>>,
    /// Per repository, its working tree; absent with `--fetch-only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, with = "Vec<BuiltJson<'a>>")]
    built: Option<Vec<BuiltJson<'a>>>,
    /// Repositories whose working tree was the locked result already;
    /// absent with `--fetch-only`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, with = "Vec<CurrentJson<'a>>")]
    current: Option<Vec<CurrentJson<'a>>>,
    /// The changes to `odx.lock`.
    lock: Vec<LockChangeJson<'a>>,
    /// Repositories whose commits were pushed to the vault.
    vaulted: Vec<VaultedJson<'a>>,
    /// Pull requests kept at their locked commit although their head moved.
    kept: Vec<KeptJson<'a>>,
    /// Whether packages were pointed at the vault in `pyproject.toml`.
    rewritten: bool,
    /// Other things done, for people.
    notes: &'a [String],
    /// The migrations the locked repositories bring, as `odx sniff` reports
    /// them.
    migrations: Vec<SniffedJson<'a>>,
    /// Commits written to bundles.
    bundled: Vec<BundledJson<'a>>,
    /// Other failures, for people.
    errors: &'a [String],
}

/// One fetch.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
enum FetchedJson<'a> {
    /// It succeeded.
    Fetched {
        /// The repositories it served, comma-separated.
        repositories: &'a str,
        /// Refs fetched.
        refs: usize,
        /// Of which from the mirror.
        from_mirror: usize,
        /// Of which through a local clone.
        through_clone: usize,
        /// Of which already cached.
        cached: usize,
    },
    /// It failed.
    Failed {
        /// The repositories it served, comma-separated.
        repositories: &'a str,
        /// Why.
        error: &'a str,
    },
}

/// One repository built.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(untagged)]
enum BuiltJson<'a> {
    /// It was built.
    Built {
        /// The repository.
        name: &'a str,
        /// Its working tree, relative to the project root; null for
        /// `odx lock`, which builds in scratch working trees.
        target_dir: Option<&'a str>,
        /// The resulting commit.
        head: &'a str,
        /// Its tree.
        tree: &'a str,
        /// One outcome per merge, in order.
        merges: Vec<MergeJson<'a>>,
    },
    /// It failed.
    Failed {
        /// The repository.
        name: &'a str,
        /// Its working tree, relative to the project root; null for
        /// `odx lock`.
        target_dir: Option<&'a str>,
        /// Why.
        error: &'a str,
    },
}

/// How one merge went.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct MergeJson<'a> {
    /// The merge, such as `#123`.
    label: &'a str,
    /// `merged`, `merge-commit`, `up-to-date` or `skipped`.
    status: &'static str,
    /// The conflicting files; only when skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, with = "Vec<String>")]
    files: Option<&'a [String]>,
}

/// A working tree that was current already.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct CurrentJson<'a> {
    /// The repository.
    name: &'a str,
    /// Its working tree, relative to the project root.
    target_dir: &'a str,
}

/// One change to `odx.lock`.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct LockChangeJson<'a> {
    /// The repository.
    repository: &'a str,
    /// `base <branch>`, a merge label such as `#123`, or empty for the
    /// whole repository.
    item: &'a str,
    /// `added`, `removed`, `rebased (same patch)` or `changed`.
    change: &'static str,
    /// The old commit, if any.
    old: Option<&'a str>,
    /// The new commit, if any.
    new: Option<&'a str>,
}

/// A repository vaulted.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct VaultedJson<'a> {
    /// The repository.
    repository: &'a str,
    /// Commits pushed to the vault.
    commits: usize,
}

/// A pull request kept at its locked commit.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct KeptJson<'a> {
    /// The repository.
    repository: &'a str,
    /// The merge, such as `#123`.
    item: &'a str,
    /// The locked commit.
    locked: &'a str,
    /// The new upstream head.
    upstream_head: &'a str,
    /// Its CI: `green`, `red`, `pending` or `unknown`; null when it was
    /// kept by `on_head_change = "pin"`.
    ci: Option<&'static str>,
    /// Why it is red: `red`, `base-also-failing` or `other-addon`.
    classification: Option<&'static str>,
}

/// Commits written to a bundle.
#[derive(Debug, Serialize, JsonSchema)]
pub(super) struct BundledJson<'a> {
    /// The repository URL.
    url: &'a str,
    /// Commits bundled.
    commits: usize,
    /// The bundle file.
    file: &'a str,
}

/// `kept pos #1 at 1a2b3c4d5e6f: upstream head 6f5e4d3c2b1a is red (base also failing)`.
pub(super) fn describe_kept(keep: &Kept) -> String {
    let state = match (keep.ci, keep.classification) {
        (Some(LockCi::Red), Some(RedClassification::BaseAlsoFailing)) => {
            "is red, likely unrelated (base also failing)"
        }
        (Some(LockCi::Red), Some(RedClassification::OtherAddon)) => {
            "is red, likely unrelated (other addon)"
        }
        (Some(LockCi::Red), _) => "is red",
        (Some(_), _) => "is pending",
        (None, _) => "is newer (on_head_change = \"pin\")",
    };
    format!(
        "kept {} {} at {}: upstream head {} {state}",
        keep.repository,
        keep.label,
        short(&keep.locked),
        short(&keep.head)
    )
}

/// `locked pos #1 at 1a2b3c4d5e6f`, `updated pos #1: a -> b (rebased (same patch))`.
pub(super) fn describe_change(change: &Change) -> String {
    let what = if change.item.is_empty() {
        change.repository.clone()
    } else {
        format!("{} {}", change.repository, change.item)
    };
    let old = change.old.as_deref().map(short).unwrap_or_default();
    let new = change.new.as_deref().map(short).unwrap_or_default();
    match change.kind {
        ChangeKind::Added => format!("locked {what} at {new}"),
        ChangeKind::Removed => format!("unlocked {what} (was {old})"),
        ChangeKind::Rebased | ChangeKind::Changed => {
            format!("updated {what}: {old} -> {new} ({})", change.kind.as_str())
        }
    }
}

pub(super) fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}
