//! Progress lines on standard error while repositories are fetched and
//! built. They are the one exception to commands writing through a
//! [`Report`](super::Report): a fetch of a large repository takes minutes,
//! and silence that long reads as a hang.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use odx_aggregate::Planned;
use odx_core::{redact, repo_key};
use odx_git::{Progress, StepOutcome, Via};

use crate::cli::GlobalArgs;

/// How much odx says while it works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Verbosity {
    /// Errors only (`--quiet`).
    Quiet,
    /// A line per repository fetched or built.
    Normal,
    /// Also when each starts, every source and every merge (`--verbose`).
    Verbose,
}

impl Verbosity {
    pub(crate) const fn of(global: &GlobalArgs) -> Self {
        if global.quiet {
            Self::Quiet
        } else if global.verbose > 0 {
            Self::Verbose
        } else {
            Self::Normal
        }
    }
}

/// Turns [`Progress`] events into lines for `sink`.
pub(crate) struct Reporter<'a> {
    verbosity: Verbosity,
    /// Repository names by URL; several repositories can share one.
    names: HashMap<&'a str, String>,
    /// Repository names by working tree.
    dirs: HashMap<&'a Path, &'a str>,
    started: Mutex<HashMap<String, Instant>>,
    sink: &'a (dyn Fn(&str) + Sync),
}

impl<'a> Reporter<'a> {
    /// A reporter that names repositories as `planned` does, and others by
    /// their URL.
    pub(crate) fn new(
        verbosity: Verbosity,
        planned: &'a [Planned],
        sink: &'a (dyn Fn(&str) + Sync),
    ) -> Self {
        let mut names: HashMap<&str, String> = HashMap::new();
        for repo in planned {
            names
                .entry(repo.url.as_str())
                .and_modify(|name| {
                    name.push_str(", ");
                    name.push_str(&repo.name);
                })
                .or_insert_with(|| repo.name.clone());
        }
        let dirs = planned
            .iter()
            .map(|repo| (repo.target_dir.as_path(), repo.name.as_str()))
            .collect();
        Self {
            verbosity,
            names,
            dirs,
            started: Mutex::new(HashMap::new()),
            sink,
        }
    }

    /// Report `event`.
    pub(crate) fn hear(&self, event: Progress<'_>) {
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let key = match event {
            Progress::Fetching(url) => url.to_owned(),
            Progress::Fetched(outcome) => outcome.url.clone(),
            Progress::Building(aggregation) | Progress::Built(aggregation, _) => {
                aggregation.target_dir.display().to_string()
            }
        };
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        let elapsed = match event {
            Progress::Fetching(_) | Progress::Building(_) => {
                started.insert(key, Instant::now());
                None
            }
            Progress::Fetched(_) | Progress::Built(..) => {
                started.remove(&key).map(|start| start.elapsed())
            }
        };
        drop(started);
        for line in self.lines(event, elapsed) {
            (self.sink)(&line);
        }
    }

    fn url_name(&self, url: &str) -> String {
        self.names
            .get(url)
            .cloned()
            .unwrap_or_else(|| repo_key(url))
    }

    fn dir_name(&self, dir: &Path) -> String {
        self.dirs
            .get(dir)
            .map_or_else(|| dir.display().to_string(), |name| (*name).to_owned())
    }

    /// The lines for `event`, which took `elapsed` when it is an end.
    fn lines(&self, event: Progress<'_>, elapsed: Option<Duration>) -> Vec<String> {
        let verbose = self.verbosity == Verbosity::Verbose;
        let took = elapsed.map_or_else(String::new, |d| format!(" in {:.1}s", d.as_secs_f64()));
        let mut lines = Vec::new();
        match event {
            Progress::Fetching(url) if verbose => {
                lines.push(format!("{:>9} {}", "Fetching", self.url_name(url)));
            }
            Progress::Building(aggregation) if verbose => {
                lines.push(format!(
                    "{:>9} {}",
                    "Building",
                    self.dir_name(&aggregation.target_dir)
                ));
            }
            Progress::Fetching(_) | Progress::Building(_) => {}
            Progress::Fetched(outcome) => match &outcome.result {
                Ok(fetched) if fetched.iter().all(|f| f.via == Via::Cache) => {
                    lines.push(format!("{:>9} {}", "Cached", self.url_name(&outcome.url)));
                }
                Ok(_) => {
                    lines.push(format!(
                        "{:>9} {}{took}",
                        "Fetched",
                        self.url_name(&outcome.url)
                    ));
                }
                Err(e) => {
                    lines.push(format!(
                        "{:>9} {}: {e}",
                        "Failed",
                        self.url_name(&outcome.url)
                    ));
                }
            },
            Progress::Built(aggregation, result) => {
                let name = self.dir_name(&aggregation.target_dir);
                match result {
                    Ok(built) => {
                        lines.push(format!("{:>9} {name}{took}: {}", "Built", built.describe()));
                    }
                    Err(e) => lines.push(format!("{:>9} {name}: {e}", "Failed")),
                }
            }
        }
        if verbose {
            lines.extend(details(event));
        }
        lines
    }
}

/// Every source fetched, or every merge made, indented under its line.
fn details(event: Progress<'_>) -> Vec<String> {
    let indent = " ".repeat(10);
    match event {
        Progress::Fetched(outcome) => outcome
            .result
            .iter()
            .flatten()
            .map(|fetched| {
                let via = match fetched.via {
                    Via::Upstream => "upstream",
                    Via::Mirror => "the mirror",
                    Via::Clone => "the local clone",
                    Via::Cache => "the cache",
                };
                let remote = fetched
                    .remote
                    .as_deref()
                    .map_or_else(String::new, |remote| format!("{} ", repo_key(remote)));
                let sha = fetched.sha.get(..12).unwrap_or(&fetched.sha);
                format!("{indent}{remote}{} {sha} from {via}", fetched.source)
            })
            .collect(),
        Progress::Built(_, Ok(built)) => built
            .steps
            .iter()
            .map(|(label, outcome)| {
                let what = match outcome {
                    StepOutcome::Merged => "merged".to_owned(),
                    StepOutcome::MergeCommit => "merged with a merge commit".to_owned(),
                    StepOutcome::UpToDate => "up to date".to_owned(),
                    StepOutcome::Skipped { files } => {
                        format!("skipped, conflicts in {}", files.join(", "))
                    }
                };
                format!("{indent}{label}: {what}")
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Write a progress line to standard error, without credentials.
#[expect(
    clippy::print_stderr,
    reason = "progress is for the terminal as it happens; tests capture eprintln"
)]
pub(crate) fn to_stderr(line: &str) {
    eprintln!("{}", redact(line));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use odx_aggregate::PlannedSource;
    use odx_git::{
        Aggregated, Aggregation, Fetched, Forge, GitError, MergeError, Outcome, Source, Strategy,
    };

    use super::*;

    fn planned(name: &str, url: &str) -> Planned {
        let source = PlannedSource {
            remote: None,
            source: Source::Branch("17.0".into()),
        };
        Planned {
            name: name.to_owned(),
            url: url.to_owned(),
            forge: Forge::Github,
            target_dir: PathBuf::from("/p/vendor").join(name),
            base_name: "17.0".to_owned(),
            base: source,
            steps: Vec::new(),
            pinned: false,
        }
    }

    fn aggregation(dir: &str) -> Aggregation {
        Aggregation {
            cache_repo: PathBuf::from("/c"),
            target_dir: PathBuf::from(dir),
            base: "b".repeat(40),
            steps: Vec::new(),
            strategy: Strategy::default(),
            mergiraf: None,
            keep_conflicts: false,
            force: false,
        }
    }

    fn fetched(remote: Option<&str>, source: Source, via: Via) -> Fetched {
        Fetched {
            remote: remote.map(str::to_owned),
            source,
            sha: "a".repeat(40),
            via,
        }
    }

    const URL: &str = "https://github.com/OCA/pos.git";

    #[test]
    fn verbosity_follows_the_flags() {
        let mut global = GlobalArgs::default();
        assert_eq!(Verbosity::of(&global), Verbosity::Normal);
        global.verbose = 2;
        assert_eq!(Verbosity::of(&global), Verbosity::Verbose);
        global.quiet = true;
        assert_eq!(Verbosity::of(&global), Verbosity::Quiet);
    }

    #[test]
    fn a_line_per_repository() {
        let planned = [planned("pos", URL), planned("pos-extra", URL)];
        let lines = Mutex::new(Vec::new());
        let sink = |line: &str| lines.lock().unwrap().push(line.to_owned());
        let reporter = Reporter::new(Verbosity::Normal, &planned, &sink);
        let upstream = Outcome {
            url: URL.to_owned(),
            result: Ok(vec![fetched(None, Source::Pr(1), Via::Upstream)]),
        };
        reporter.hear(Progress::Fetching(URL));
        reporter.hear(Progress::Fetched(&upstream));
        let cached = Outcome {
            url: "https://example.com/x.git".to_owned(),
            result: Ok(vec![fetched(None, Source::Pr(1), Via::Cache)]),
        };
        reporter.hear(Progress::Fetched(&cached));
        let failed = Outcome {
            url: URL.to_owned(),
            result: Err(GitError::Cancelled),
        };
        reporter.hear(Progress::Fetched(&failed));
        let tree = aggregation("/p/vendor/pos");
        let built = Aggregated {
            head: "h".into(),
            tree: "t".into(),
            steps: vec![("#1".into(), StepOutcome::Merged)],
        };
        reporter.hear(Progress::Building(&tree));
        reporter.hear(Progress::Built(&tree, &Ok(built)));
        let error = MergeError::Git(GitError::Cancelled);
        reporter.hear(Progress::Built(&aggregation("/elsewhere"), &Err(error)));
        let lines = lines.into_inner().unwrap();
        assert!(
            lines[0].starts_with("  Fetched pos, pos-extra in "),
            "{lines:?}"
        );
        assert!(lines[0].ends_with('s'));
        assert_eq!(lines[1], "   Cached example.com/x");
        assert_eq!(lines[2], "   Failed pos, pos-extra: cancelled");
        assert!(lines[3].starts_with("    Built pos in "));
        assert!(lines[3].ends_with("s: 1 merged"));
        assert_eq!(lines[4], "   Failed /elsewhere: cancelled");
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn verbose_says_when_work_starts_and_what_it_did() {
        let planned = [planned("pos", URL)];
        let lines = Mutex::new(Vec::new());
        let sink = |line: &str| lines.lock().unwrap().push(line.to_owned());
        let reporter = Reporter::new(Verbosity::Verbose, &planned, &sink);
        reporter.hear(Progress::Fetching(URL));
        let outcome = Outcome {
            url: URL.to_owned(),
            result: Ok(vec![
                fetched(None, Source::Branch("17.0".into()), Via::Upstream),
                fetched(
                    Some("https://github.com/me/pos.git"),
                    Source::Pr(2),
                    Via::Mirror,
                ),
                fetched(None, Source::Pr(3), Via::Clone),
                fetched(None, Source::Pr(4), Via::Cache),
            ]),
        };
        reporter.hear(Progress::Fetched(&outcome));
        let tree = aggregation("/p/vendor/pos");
        reporter.hear(Progress::Building(&tree));
        let skipped = StepOutcome::Skipped {
            files: vec!["a.xml".into(), "b.py".into()],
        };
        let built = Aggregated {
            head: "h".into(),
            tree: "t".into(),
            steps: vec![
                ("#1".into(), StepOutcome::Merged),
                ("#2".into(), StepOutcome::MergeCommit),
                ("#3".into(), StepOutcome::UpToDate),
                ("#4".into(), skipped),
            ],
        };
        reporter.hear(Progress::Built(&tree, &Ok(built)));
        let lines = lines.into_inner().unwrap();
        let sha = "a".repeat(12);
        assert_eq!(lines[0], " Fetching pos");
        assert_eq!(
            lines[2],
            format!("          branch 17.0 {sha} from upstream")
        );
        assert_eq!(
            lines[3],
            format!("          github.com/me/pos #2 {sha} from the mirror")
        );
        assert_eq!(lines[4], format!("          #3 {sha} from the local clone"));
        assert_eq!(lines[5], format!("          #4 {sha} from the cache"));
        assert_eq!(lines[6], " Building pos");
        assert_eq!(
            &lines[8..],
            [
                "          #1: merged",
                "          #2: merged with a merge commit",
                "          #3: up to date",
                "          #4: skipped, conflicts in a.xml, b.py",
            ]
        );
    }

    #[test]
    fn quiet_says_nothing() {
        let lines = Mutex::new(Vec::new());
        let sink = |line: &str| lines.lock().unwrap().push(line.to_owned());
        let reporter = Reporter::new(Verbosity::Quiet, &[], &sink);
        reporter.hear(Progress::Fetching(URL));
        sink("only this");
        assert_eq!(lines.into_inner().unwrap(), ["only this"]);
        to_stderr("odx progress test line");
    }
}
