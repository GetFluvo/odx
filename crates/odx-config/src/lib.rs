//! Configuration for odx: `[tool.odx]`, dependency groups, uv sources, `odx.lock`, mutes and `repos.yaml`/`addons.yaml` compatibility.
//!
//! [`discover`] finds the project (`pyproject.toml` with a `[tool.odx]`
//! table, or an explicit `repos.yaml`), [`parse_pyproject`] reads and
//! validates the table into [`OdxConfig`]. Unknown keys inside `[tool.odx]`
//! are errors with line and column; consistency problems are collected and
//! reported together.

pub mod compat;
pub mod deps;
mod duration;
pub mod edit;
mod error;
mod load;
pub mod lock;
pub mod mutes;
pub mod packages;
pub mod schema;
pub mod user;
pub mod uvlock;

pub use duration::{Span, SpanError};
pub use error::{ConfigError, Problem};
pub use load::{Project, Source, discover, find_pyproject, parse_pyproject};
pub use schema::{MergeSource, OdxConfig};
