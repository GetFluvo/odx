//! Git object cache, fetching, worktrees, merges, cherry-picks and patch-ids for odx, on gix with a system git fallback.
//!
//! The cache keeps one bare repository per upstream URL (see [`Cache`]).
//! [`fetch()`] brings branches, pull requests and commits into it with one
//! `git fetch` per remote URL, under an advisory lock so concurrent odx
//! processes never fetch into the same repository at once. Fetched objects
//! are kept under `refs/odx/…` so they survive upstream force-pushes until
//! the cache is collected.
//!
//! Git access goes through [`GitBackend`]; [`SystemGit`] runs the system
//! `git` with a controlled environment.

mod backend;
mod cache;
mod fetch;
mod lock;
mod refs;
mod run;

pub use backend::{GitBackend, GitError, SystemGit};
pub use cache::{Cache, url_hash};
pub use fetch::{FetchPlan, Fetched, Via, fetch};
pub use lock::RepoLock;
pub use refs::{Forge, Source};
pub use run::{Limits, Outcome, fetch_all};
pub use tokio_util::sync::CancellationToken;
