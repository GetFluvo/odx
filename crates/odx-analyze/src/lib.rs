//! Odoo manifest analysis for odx: check-deps, stats, sniff, status, suggest and upgrade readiness.
//!
//! Part of [odx](https://github.com/getfluvo/odx). `stats` counts lines of
//! code with tokei; the other analyses land in roadmap phases 5 and 7.

pub mod core;
pub mod glue;
pub mod manifest;
pub mod sniff;
pub mod stats;
