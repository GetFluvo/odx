//! Domain types, exit codes and IO traits shared by the odx crates.
//!
//! Library crates in the odx workspace never touch the operating system
//! directly. They take the traits defined here ([`Clock`], [`Env`],
//! [`FileSystem`], [`ProcessRunner`]) so that every code path, including
//! error paths, can be exercised in tests. The binaries construct the real
//! implementations; tests use the in-memory fakes from [`fake`] (feature
//! `fakes`).
//!
//! Traits for git, forges, package indexes and secret storage are added in
//! the roadmap phases that introduce them.

mod clock;
mod env;
mod exit;
mod fs;
mod process;

#[cfg(feature = "fakes")]
pub mod fake;

pub use clock::Clock;
pub use env::Env;
pub use exit::Exit;
pub use fs::FileSystem;
pub use process::{Invocation, Output, ProcessRunner};
