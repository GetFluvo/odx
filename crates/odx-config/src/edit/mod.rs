//! Editing `pyproject.toml`: `odx add`, `odx drop` and `odx import --write`.
//!
//! Edits go through `toml_edit`, so comments, ordering and formatting
//! outside the touched entries stay byte-for-byte identical.

mod doc;
mod spec;

pub use doc::{Added, EditError, PyprojectEdit};
pub use spec::{Spec, Target, parse_spec, repo_key};
