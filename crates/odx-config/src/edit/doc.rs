use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use super::spec::{Spec, Target, repo_key};
use crate::compat::repository_table;
use crate::deps::{self, Group};
use crate::schema::{Merge, Repository};

/// Errors from editing `pyproject.toml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The text is not valid TOML.
    #[error("{0}")]
    Toml(String),
    /// There is no `[tool.odx]` table and no series to create one with.
    #[error("no [tool.odx] table; pass the Odoo series to create one")]
    NoOdxTable,
    /// A key has an unexpected type.
    #[error("{0} has an unexpected type")]
    Malformed(&'static str),
    /// Nothing matched what should be dropped.
    #[error("{0}")]
    NotFound(String),
}

/// What an `add` changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Added {
    /// A merge was appended to an existing repository.
    Merge,
    /// A new repository with the merge was appended.
    Repository,
    /// The merge or package was already configured; nothing changed.
    AlreadyPresent,
    /// A package entry was written to `[tool.odx.packages]`.
    PackageEntry,
    /// A direct reference was appended to a dependency group.
    DirectReference,
}

/// A `pyproject.toml` opened for editing; formatting outside the touched
/// entries is preserved byte for byte.
#[derive(Debug, Clone, Default)]
pub struct PyprojectEdit {
    doc: DocumentMut,
}

impl std::fmt::Display for PyprojectEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.doc.fmt(f)
    }
}

impl PyprojectEdit {
    /// Parse `text` for editing.
    ///
    /// # Errors
    ///
    /// [`EditError::Toml`] when the text is not valid TOML.
    pub fn parse(text: &str) -> Result<Self, EditError> {
        text.parse()
            .map(|doc| Self { doc })
            .map_err(|e: toml_edit::TomlError| EditError::Toml(e.to_string()))
    }

    /// A new `pyproject.toml` with only a `[project]` table naming `name`.
    ///
    /// ```
    /// use odx_config::edit::PyprojectEdit;
    ///
    /// let edit = PyprojectEdit::new_project("crm");
    /// assert_eq!(edit.to_string(), "[project]\nname = \"crm\"\nversion = \"0.1.0\"\n");
    /// ```
    #[must_use]
    pub fn new_project(name: &str) -> Self {
        let mut project = Table::new();
        project.insert("name", toml_edit::value(name));
        project.insert("version", toml_edit::value("0.1.0"));
        let mut doc = DocumentMut::new();
        doc.insert("project", Item::Table(project));
        Self { doc }
    }

    /// The document being edited.
    ///
    /// ```
    /// use odx_config::edit::PyprojectEdit;
    ///
    /// let mut edit = PyprojectEdit::parse("[project]\nname = \"p\"\n").unwrap();
    /// assert!(edit.document().get("project").is_some());
    /// edit.document_mut().remove("project");
    /// assert_eq!(edit.to_string(), "");
    /// ```
    pub fn document(&self) -> &DocumentMut {
        &self.doc
    }

    /// The document being edited, for changes the methods do not cover.
    pub fn document_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    /// The `[tool.odx]` table, created with `odoo_version` when missing.
    fn odx_table(&mut self, odoo_version: Option<&str>) -> Result<&mut Table, EditError> {
        let exists = self
            .doc
            .get("tool")
            .and_then(|tool| tool.get("odx"))
            .is_some();
        if !exists {
            let series = odoo_version.ok_or(EditError::NoOdxTable)?;
            let tool = self
                .doc
                .entry("tool")
                .or_insert_with(|| {
                    let mut table = Table::new();
                    table.set_implicit(true);
                    Item::Table(table)
                })
                .as_table_mut()
                .ok_or(EditError::Malformed("[tool]"))?;
            let mut odx = Table::new();
            odx.insert("odoo_version", Item::Value(Value::from(series)));
            tool.insert("odx", Item::Table(odx));
        }
        self.doc
            .as_table_mut()
            .get_mut("tool")
            .and_then(Item::as_table_like_mut)
            .and_then(|tool| tool.get_mut("odx"))
            .and_then(Item::as_table_mut)
            .ok_or(EditError::Malformed("[tool.odx]"))
    }

    fn repositories(
        &mut self,
        odoo_version: Option<&str>,
    ) -> Result<&mut ArrayOfTables, EditError> {
        let odx = self.odx_table(odoo_version)?;
        odx.entry("repositories")
            .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or(EditError::Malformed("[tool.odx].repositories"))
    }

    /// Append `repos` as `[[tool.odx.repositories]]`, skipping URLs that are
    /// already configured; returns the skipped URLs.
    ///
    /// # Errors
    ///
    /// [`EditError::NoOdxTable`] when `[tool.odx]` is missing and no series is
    /// given; [`EditError::Malformed`] for unexpected types.
    pub fn add_repositories(
        &mut self,
        repos: &[Repository],
        odoo_version: Option<&str>,
    ) -> Result<Vec<String>, EditError> {
        let tables = self.repositories(odoo_version)?;
        let mut skipped = Vec::new();
        for repo in repos {
            let key = repo_key(&repo.url);
            if tables.iter().any(|table| has_key(table, &key)) {
                skipped.push(repo.url.clone());
            } else {
                tables.push(repository_table(repo));
            }
        }
        Ok(skipped)
    }

    /// Add a merge to the repository of `spec.url`, adding the repository
    /// when it is new.
    ///
    /// # Errors
    ///
    /// As [`PyprojectEdit::add_repositories`].
    pub fn add_merge(
        &mut self,
        spec: &Spec,
        odoo_version: Option<&str>,
    ) -> Result<Added, EditError> {
        let tables = self.repositories(odoo_version)?;
        let key = repo_key(&spec.url);
        let Some(table) = tables.iter_mut().find(|table| has_key(table, &key)) else {
            tables.push(repository_table(&Repository {
                name: Some(repo_name(&spec.url)),
                url: spec.url.clone(),
                target_dir: None,
                base: None,
                target_branch: None,
                forge: None,
                addons: None,
                merges: vec![merge_of(&spec.target)],
            }));
            return Ok(Added::Repository);
        };
        let merges = table
            .entry("merges")
            .or_insert_with(|| Item::Value(Value::Array(toml_edit::Array::new())))
            .as_array_mut()
            .ok_or(EditError::Malformed("merges"))?;
        if merges.iter().any(|value| matches(value, &spec.target)) {
            return Ok(Added::AlreadyPresent);
        }
        let mut value = Value::InlineTable(inline_merge(&spec.target));
        if let Some(last) = merges.iter().last() {
            value.decor_mut().clone_from(last.decor());
        }
        merges.push_formatted(value);
        Ok(Added::Merge)
    }

    /// Record in `[tool.odx.glue] ignore` that glue addon `addon` is not
    /// wanted, and why.
    ///
    /// # Errors
    ///
    /// [`EditError::NoOdxTable`] without `[tool.odx]`, and
    /// [`EditError::Malformed`] when `glue` or its `ignore` is no table.
    pub fn ignore_glue(&mut self, addon: &str, reason: &str) -> Result<(), EditError> {
        let glue = self
            .odx_table(None)?
            .entry("glue")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut()
            .ok_or(EditError::Malformed("[tool.odx.glue]"))?;
        glue.entry("ignore")
            .or_insert(Item::Value(Value::InlineTable(InlineTable::new())))
            .as_table_like_mut()
            .ok_or(EditError::Malformed("[tool.odx.glue] ignore"))?
            .insert(addon, Item::Value(Value::from(reason)));
        Ok(())
    }

    /// Remove the merge `spec` from its repository.
    ///
    /// # Errors
    ///
    /// [`EditError::NotFound`] when no repository or merge matches.
    pub fn drop_merge(&mut self, spec: &Spec) -> Result<(), EditError> {
        let not_found = || EditError::NotFound(format!("no merge {} on {}", spec.target, spec.url));
        let tables = self.repositories(None).map_err(|_| not_found())?;
        let key = repo_key(&spec.url);
        let merges = tables
            .iter_mut()
            .find(|table| has_key(table, &key))
            .and_then(|table| table.get_mut("merges"))
            .and_then(Item::as_array_mut)
            .ok_or_else(not_found)?;
        let position = merges
            .iter()
            .position(|value| matches(value, &spec.target))
            .ok_or_else(not_found)?;
        merges.remove(position);
        Ok(())
    }

    /// Read dependency group `name`.
    pub fn group(&self, name: &str) -> Option<Group> {
        deps::read_group(&self.doc, name)
    }

    /// Add package `dist` from `spec`. When `group` lists direct references,
    /// a PEP 508 direct reference is appended to it; otherwise a
    /// `[tool.odx.packages]` entry is written.
    ///
    /// # Errors
    ///
    /// As [`PyprojectEdit::add_repositories`].
    pub fn add_package(
        &mut self,
        dist: &str,
        spec: &Spec,
        subdirectory: Option<&str>,
        group: Option<&str>,
        odoo_version: Option<&str>,
    ) -> Result<Added, EditError> {
        if let Some(group) = group.and_then(|g| self.group(g))
            && group.uses_direct_references()
        {
            let wanted = deps::normalize_name(dist);
            if group
                .requirements
                .iter()
                .any(|r| deps::normalize_name(&r.name) == wanted)
            {
                return Ok(Added::AlreadyPresent);
            }
            let reference = match &spec.target {
                Target::Pr(pr) if spec.url.contains("gitlab") => {
                    format!("refs/merge-requests/{pr}/head")
                }
                Target::Pr(pr) => format!("refs/pull/{pr}/head"),
                Target::Sha(reference) | Target::Branch(reference) => reference.clone(),
            };
            let fragment = subdirectory
                .map(|s| format!("#subdirectory={s}"))
                .unwrap_or_default();
            let requirement = format!("{dist} @ git+{}@{reference}{fragment}", spec.url);
            deps::add_to_group(&mut self.doc, &group, &requirement);
            return Ok(Added::DirectReference);
        }
        let odx = self.odx_table(odoo_version)?;
        let packages = odx
            .entry("packages")
            .or_insert_with(|| Item::Table(Table::new()))
            .as_table_like_mut()
            .ok_or(EditError::Malformed("[tool.odx.packages]"))?;
        if packages.contains_key(dist) {
            return Ok(Added::AlreadyPresent);
        }
        let mut entry = InlineTable::new();
        entry.insert("url", Value::from(spec.url.as_str()));
        match &spec.target {
            Target::Pr(pr) => entry.insert("pr", Value::from(i64::from(*pr))),
            Target::Sha(rev) => entry.insert("rev", Value::from(rev.as_str())),
            Target::Branch(branch) => entry.insert("branch", Value::from(branch.as_str())),
        };
        for (key, value) in [("subdirectory", subdirectory), ("group", group)] {
            if let Some(value) = value {
                entry.insert(key, Value::from(value));
            }
        }
        packages.insert(dist, Item::Value(Value::InlineTable(entry)));
        Ok(Added::PackageEntry)
    }

    /// Remove package `dist` from `[tool.odx.packages]`, from direct
    /// references in `groups` and from `[tool.uv.sources]`.
    ///
    /// # Errors
    ///
    /// [`EditError::NotFound`] when the package is configured nowhere.
    pub fn drop_package(&mut self, dist: &str, groups: &[&str]) -> Result<(), EditError> {
        // Only non-inserting accessors: `Item::get_mut` would create the
        // missing tables.
        let mut found = self
            .doc
            .as_table_mut()
            .get_mut("tool")
            .and_then(Item::as_table_like_mut)
            .and_then(|tool| tool.get_mut("odx"))
            .and_then(Item::as_table_like_mut)
            .and_then(|odx| odx.get_mut("packages"))
            .and_then(Item::as_table_like_mut)
            .is_some_and(|packages| packages.remove(dist).is_some());
        let groups: Vec<Group> = groups.iter().filter_map(|g| self.group(g)).collect();
        found |= deps::remove_from_groups(&mut self.doc, &groups, dist) > 0;
        found |= deps::remove_uv_source(&mut self.doc, dist);
        if found {
            Ok(())
        } else {
            Err(EditError::NotFound(format!("no package {dist}")))
        }
    }
}

/// Whether a repository table's `url` has the comparison key `key`.
fn has_key(table: &Table, key: &str) -> bool {
    table
        .get("url")
        .and_then(Item::as_str)
        .is_some_and(|known| repo_key(known) == key)
}

fn repo_name(url: &str) -> String {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    path.rsplit(['/', ':']).next().unwrap_or(path).to_owned()
}

fn merge_of(target: &Target) -> Merge {
    let mut merge = Merge {
        pr: None,
        sha: None,
        branch: None,
        remote: None,
        on_conflict: None,
        on_head_change: None,
        track_upstream: None,
    };
    match target {
        Target::Pr(pr) => merge.pr = Some(*pr),
        Target::Sha(sha) => merge.sha = Some(sha.clone()),
        Target::Branch(branch) => merge.branch = Some(branch.clone()),
    }
    merge
}

fn inline_merge(target: &Target) -> InlineTable {
    let mut table = InlineTable::new();
    match target {
        Target::Pr(pr) => table.insert("pr", Value::from(i64::from(*pr))),
        Target::Sha(sha) => table.insert("sha", Value::from(sha.as_str())),
        Target::Branch(branch) => table.insert("branch", Value::from(branch.as_str())),
    };
    table
}

/// Whether an existing merge value is `target` (on the repository's own URL).
fn matches(value: &Value, target: &Target) -> bool {
    let Some(table) = value.as_inline_table() else {
        return false;
    };
    if table.contains_key("remote") {
        return false;
    }
    match target {
        Target::Pr(pr) => table
            .get("pr")
            .and_then(Value::as_integer)
            .is_some_and(|known| known == i64::from(*pr)),
        Target::Sha(sha) => table
            .get("sha")
            .and_then(Value::as_str)
            .is_some_and(|known| known.eq_ignore_ascii_case(sha)),
        Target::Branch(branch) => table
            .get("branch")
            .and_then(Value::as_str)
            .is_some_and(|known| known == branch),
    }
}

#[cfg(test)]
#[path = "doc_tests.rs"]
mod tests;
