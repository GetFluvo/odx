//! Forge and package index clients for odx: GitHub, GitLab, Gitea/Forgejo, PyPI; tokens, rate limiting and HTTP caching.
//!
//! [`RepoUrl`] reads a clone URL, [`token`] finds the credentials for a
//! host and [`ForgeClient`] asks the forge's API, through the [`Http`]
//! trait ([`ReqwestHttp`] in production), which forge hosts a repository
//! and who can read it. Rate limits are waited out a bounded number of
//! times; tokens are sent only to their own host and never appear in
//! errors.

mod cache;
mod ci;
mod client;
mod http;
#[cfg(test)]
mod testing;
mod url;

pub use cache::CachedHttp;
pub use ci::{CiReport, CiState};
pub use client::{ForgeClient, ForgeKind, Visibility, token};
pub use http::{Http, HttpResponse, ReqwestHttp};
pub use url::RepoUrl;

/// Errors from forge APIs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForgeError {
    /// No response arrived.
    #[error("request failed: {0}")]
    Http(String),
    /// The forge answered with an unexpected status.
    #[error("{host} answered HTTP {status}")]
    Status {
        /// The host.
        host: String,
        /// The status code.
        status: u16,
    },
    /// The rate limit did not reset in time.
    #[error("{0}: rate limit exceeded; try again later or use a token")]
    RateLimited(String),
    /// The answer was not the expected JSON.
    #[error("unexpected answer from {0}")]
    Json(String),
    /// No forge API was found on the host.
    #[error("{0} does not look like GitHub, GitLab, Gitea or Forgejo")]
    UnknownForge(String),
    /// The repository is missing or the token cannot read it.
    #[error("cannot see {0}: it does not exist, or the token for its host cannot read it")]
    NotVisible(String),
}
