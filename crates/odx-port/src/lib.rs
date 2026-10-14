//! Porting engine for odx: port PRs and missing commits across Odoo series, maintainer sweeps and oca-port compatible blacklists.
//!
//! Part of [odx](https://github.com/getfluvo/odx). [`analysis`] finds the
//! commits an addon has on one series and not on another, grouped by pull
//! request and classified (portable, migration, repo-wide, translations,
//! blacklisted); [`blacklist`] reads and writes oca-port's blacklist files
//! byte for byte. `odx suggest` and `odx port` share both. [`migrate`]
//! migrates an addon to another series on a branch, the OCA way.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod analysis;
pub mod blacklist;
pub mod git;
pub mod migrate;
pub mod port;
pub mod squash;
#[cfg(test)]
mod testing;
