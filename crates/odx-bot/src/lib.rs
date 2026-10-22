//! The odx bot: the upstream dashboard as an issue, and pull requests for
//! the changes that are safe to propose, kept idempotent across runs by the
//! markers they carry.
//!
//! Part of [odx](https://github.com/getfluvo/odx).

pub mod issue;
pub mod pr;
