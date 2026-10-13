//! Dependency groups and `[tool.uv.sources]`.
//!
//! odx reads the groups named in `[tool.odx.groups]` from
//! `[project.optional-dependencies]` or `[dependency-groups]`, and the
//! `[tool.uv.sources]` table, to find the project's addon packages. It
//! rewrites them in place with `toml_edit`, so everything outside the
//! touched entries keeps its formatting.

use toml_edit::{DocumentMut, Item, Value};

/// A requirement from a dependency group, parsed as far as odx needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Distribution name as written.
    pub name: String,
    /// Extras inside `[...]`, as written.
    pub extras: Option<String>,
    /// Version specifier (`==17.0.1.2.0`), empty when none or a direct reference.
    pub specifier: String,
    /// PEP 508 direct reference (`name @ <url>`).
    pub direct: Option<DirectRef>,
    /// Environment markers after `;`, if any.
    pub markers: Option<String>,
}

/// A `git+<url>@<ref>#subdirectory=<dir>` direct reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectRef {
    /// The URL as written after `@`.
    pub raw: String,
    /// Repository URL without `git+`, ref and fragment.
    pub repository: String,
    /// Branch, tag, `refs/pull/N/head` or commit, if given.
    pub reference: Option<String>,
    /// `subdirectory=` from the fragment.
    pub subdirectory: Option<String>,
}

/// PEP 503 normalized distribution name.
///
/// ```
/// use odx_config::deps::normalize_name;
///
/// assert_eq!(normalize_name("Odoo_Addon.Web__Responsive"), "odoo-addon-web-responsive");
/// ```
pub fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
        } else {
            if separator && !out.is_empty() {
                out.push('-');
            }
            separator = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// Parse a PEP 508 requirement string.
///
/// ```
/// use odx_config::deps::parse_requirement;
///
/// let req = parse_requirement(
///     "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
/// ).unwrap();
/// let direct = req.direct.unwrap();
/// assert_eq!(direct.repository, "https://github.com/OCA/pos.git");
/// assert_eq!(direct.reference.as_deref(), Some("refs/pull/45/head"));
/// assert_eq!(direct.subdirectory.as_deref(), Some("pos_vouchers"));
/// ```
///
/// # Errors
///
/// A message when the string does not start with a distribution name.
pub fn parse_requirement(text: &str) -> Result<Requirement, String> {
    let (body, markers) = text
        .split_once(';')
        .map_or((text, None), |(body, markers)| {
            (body, Some(markers.trim().to_owned()))
        });
    let body = body.trim();
    let name_end = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(body.len());
    let (name, rest) = body.split_at(name_end);
    if name.is_empty() {
        return Err(format!("{text:?} does not start with a distribution name"));
    }
    let rest = rest.trim_start();
    let (extras, rest) = rest
        .strip_prefix('[')
        .and_then(|extras| extras.split_once(']'))
        .map_or((None, rest), |(extras, after)| {
            (Some(extras.to_owned()), after.trim_start())
        });
    let (specifier, direct) = rest.strip_prefix('@').map_or_else(
        || (rest.trim().to_owned(), None),
        |url| (String::new(), Some(parse_direct_ref(url.trim()))),
    );
    Ok(Requirement {
        name: name.to_owned(),
        extras,
        specifier,
        direct,
        markers,
    })
}

fn parse_direct_ref(raw: &str) -> DirectRef {
    let (url, fragment) = raw.split_once('#').unwrap_or((raw, ""));
    let subdirectory = fragment
        .split('&')
        .find_map(|param| param.strip_prefix("subdirectory="))
        .map(str::to_owned);
    let url = url.strip_prefix("git+").unwrap_or(url);
    let path_start = url.find("://").map_or(0, |i| i + 3);
    let authority_end = url
        .split_at(path_start)
        .1
        .find('/')
        .map_or(url.len(), |i| path_start + i);
    let (head, path) = url.split_at(authority_end);
    let (repository, reference) = path.rsplit_once('@').map_or_else(
        || (url.to_owned(), None),
        |(path, reference)| (format!("{head}{path}"), Some(reference.to_owned())),
    );
    DirectRef {
        raw: raw.to_owned(),
        repository,
        reference,
        subdirectory,
    }
}

/// Where a dependency group is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupTable {
    /// `[project.optional-dependencies]`.
    OptionalDependencies,
    /// `[dependency-groups]` (PEP 735).
    DependencyGroups,
}

impl GroupTable {
    const fn path(self) -> &'static [&'static str] {
        match self {
            Self::OptionalDependencies => &["project", "optional-dependencies"],
            Self::DependencyGroups => &["dependency-groups"],
        }
    }
}

/// A dependency group and its requirement strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Group name.
    pub name: String,
    /// Where it is declared.
    pub table: GroupTable,
    /// Requirements in order; strings that are not valid requirements are skipped.
    pub requirements: Vec<Requirement>,
}

impl Group {
    /// Whether the group lists packages as direct references.
    pub fn uses_direct_references(&self) -> bool {
        self.requirements.iter().any(|r| r.direct.is_some())
    }
}

fn get_path<'a>(doc: &'a DocumentMut, path: &[&str]) -> Option<&'a Item> {
    path.iter()
        .try_fold(doc.as_item(), |item, key| item.as_table_like()?.get(key))
}

fn get_path_mut<'a>(doc: &'a mut DocumentMut, path: &[&str]) -> Option<&'a mut Item> {
    path.iter().try_fold(doc.as_item_mut(), |item, key| {
        item.as_table_like_mut()?.get_mut(key)
    })
}

/// Read dependency group `name`, looking in `[project.optional-dependencies]`
/// first and then in `[dependency-groups]`.
pub fn read_group(doc: &DocumentMut, name: &str) -> Option<Group> {
    [
        GroupTable::OptionalDependencies,
        GroupTable::DependencyGroups,
    ]
    .into_iter()
    .find_map(|table| {
        let array = get_path(doc, table.path())?
            .as_table_like()?
            .get(name)?
            .as_array()?;
        Some(Group {
            name: name.to_owned(),
            table,
            requirements: array
                .iter()
                .filter_map(|v| parse_requirement(v.as_str()?).ok())
                .collect(),
        })
    })
}

/// Append a requirement string to group `name`, keeping the list's layout.
///
/// Returns `false` when the group does not exist.
pub fn add_to_group(doc: &mut DocumentMut, group: &Group, requirement: &str) -> bool {
    let Some(array) = get_path_mut(doc, group.table.path())
        .and_then(|item| item.as_table_like_mut()?.get_mut(&group.name))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    let mut value = Value::from(requirement);
    if let Some(last) = array.iter().last() {
        value.decor_mut().clone_from(last.decor());
    }
    array.push_formatted(value);
    true
}

/// Remove every direct-reference requirement for distribution `dist` from
/// the groups in `groups`; returns how many were removed.
pub fn remove_from_groups(doc: &mut DocumentMut, groups: &[Group], dist: &str) -> usize {
    let wanted = normalize_name(dist);
    let mut removed = 0;
    for group in groups {
        let Some(array) = get_path_mut(doc, group.table.path())
            .and_then(|item| item.as_table_like_mut()?.get_mut(&group.name))
            .and_then(Item::as_array_mut)
        else {
            continue;
        };
        let before = array.len();
        array.retain(|value| {
            value
                .as_str()
                .and_then(|text| parse_requirement(text).ok())
                .is_none_or(|req| normalize_name(&req.name) != wanted || req.direct.is_none())
        });
        removed += before - array.len();
    }
    removed
}

/// Replace the direct reference of `dist` in `group` with `url`, keeping
/// the name, extras position, markers and the string's quoting.
///
/// Returns `false` when no direct reference for `dist` exists in the group.
pub fn set_direct_ref(doc: &mut DocumentMut, group: &Group, dist: &str, url: &str) -> bool {
    let wanted = normalize_name(dist);
    let Some(array) = get_path_mut(doc, group.table.path())
        .and_then(|item| item.as_table_like_mut()?.get_mut(&group.name))
        .and_then(Item::as_array_mut)
    else {
        return false;
    };
    for value in array.iter_mut() {
        let Some(req) = value.as_str().and_then(|text| parse_requirement(text).ok()) else {
            continue;
        };
        if normalize_name(&req.name) != wanted || req.direct.is_none() {
            continue;
        }
        let extras = req.extras.map(|e| format!("[{e}]")).unwrap_or_default();
        let markers = req.markers.map(|m| format!(" ; {m}")).unwrap_or_default();
        let decor = value.decor().clone();
        *value = Value::from(format!("{}{extras} @ {url}{markers}", req.name));
        *value.decor_mut() = decor;
        return true;
    }
    false
}

/// A `[tool.uv.sources]` entry that points at git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSource {
    /// Distribution name as written.
    pub name: String,
    /// Repository URL.
    pub git: String,
    /// `branch`, if set.
    pub branch: Option<String>,
    /// `tag`, if set.
    pub tag: Option<String>,
    /// `rev`, if set.
    pub rev: Option<String>,
    /// `subdirectory`, if set.
    pub subdirectory: Option<String>,
}

/// Every git entry of `[tool.uv.sources]`, in file order. Other source kinds
/// (index, path, url) are not odx's business and are skipped.
pub fn read_uv_git_sources(doc: &DocumentMut) -> Vec<GitSource> {
    let Some(sources) = get_path(doc, &["tool", "uv", "sources"]).and_then(Item::as_table_like)
    else {
        return Vec::new();
    };
    sources
        .iter()
        .filter_map(|(name, item)| {
            let table = item.as_table_like()?;
            let text = |key: &str| table.get(key)?.as_str().map(str::to_owned);
            Some(GitSource {
                name: name.to_owned(),
                git: text("git")?,
                branch: text("branch"),
                tag: text("tag"),
                rev: text("rev"),
                subdirectory: text("subdirectory"),
            })
        })
        .collect()
}

/// The table at `key` in `item`, created (implicit) when missing; `None`
/// when `item`, or the value at `key`, is not a table.
fn child_table<'a>(item: &'a mut Item, key: &str) -> Option<&'a mut Item> {
    item.as_table_like_mut().and_then(|table| {
        if table.get(key).is_none() {
            let mut new = toml_edit::Table::new();
            new.set_implicit(true);
            table.insert(key, Item::Table(new));
        }
        table.get_mut(key).filter(|child| child.is_table_like())
    })
}

/// Point `[tool.uv.sources]` entry `dist` at tag `tag` of git repository `git`.
///
/// The table is created when needed and an existing entry for the same
/// distribution (in any spelling) is replaced. Returns `false` when `tool`,
/// `tool.uv` or `tool.uv.sources` is not a table.
///
/// ```
/// use odx_config::deps::{read_uv_git_sources, set_uv_git_source};
///
/// let mut doc: toml_edit::DocumentMut = "[project]\nname = \"p\"\n".parse().unwrap();
/// assert!(set_uv_git_source(&mut doc, "odoo-addon-x", "https://vault/v.git", "odx/abc", Some("x")));
/// let source = &read_uv_git_sources(&doc)[0];
/// assert_eq!((source.tag.as_deref(), source.subdirectory.as_deref()), (Some("odx/abc"), Some("x")));
/// ```
pub fn set_uv_git_source(
    doc: &mut DocumentMut,
    dist: &str,
    git: &str,
    tag: &str,
    subdirectory: Option<&str>,
) -> bool {
    remove_uv_source(doc, dist);
    let mut entry = toml_edit::InlineTable::new();
    entry.insert("git", Value::from(git));
    entry.insert("tag", Value::from(tag));
    if let Some(subdirectory) = subdirectory {
        entry.insert("subdirectory", Value::from(subdirectory));
    }
    let sources = child_table(doc.as_item_mut(), "tool")
        .and_then(|tool| child_table(tool, "uv"))
        .and_then(|uv| child_table(uv, "sources"))
        .and_then(Item::as_table_like_mut);
    sources.is_some_and(|sources| {
        sources.insert(dist, Item::Value(Value::InlineTable(entry)));
        true
    })
}

/// Remove the `[tool.uv.sources]` entry of `dist`; returns whether one existed.
pub fn remove_uv_source(doc: &mut DocumentMut, dist: &str) -> bool {
    let wanted = normalize_name(dist);
    let Some(sources) =
        get_path_mut(doc, &["tool", "uv", "sources"]).and_then(Item::as_table_like_mut)
    else {
        return false;
    };
    let key = sources
        .iter()
        .map(|(key, _)| key.to_owned())
        .find(|key| normalize_name(key) == wanted);
    key.is_some_and(|key| sources.remove(&key).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uv_sources_are_written_in_place() {
        let mut doc: DocumentMut = "[tool.uv.sources]\nodoo = { git = \"https://github.com/OCA/OCB.git\", branch = \"17.0\" }\nOdoo_Addon_X = { git = \"old\" }\n".parse().unwrap();
        assert!(set_uv_git_source(
            &mut doc,
            "odoo-addon-x",
            "v",
            "odx/a",
            None
        ));
        let sources = read_uv_git_sources(&doc);
        assert_eq!(sources.len(), 2);
        assert_eq!(
            (sources[1].name.as_str(), sources[1].git.as_str()),
            ("odoo-addon-x", "v")
        );
        assert_eq!(sources[1].subdirectory, None);
        assert!(doc.to_string().starts_with("[tool.uv.sources]\nodoo = "));
        for broken in ["tool = 1\n", "[tool]\nuv = 1\n", "[tool.uv]\nsources = 1\n"] {
            let mut doc: DocumentMut = broken.parse().unwrap();
            assert!(
                !set_uv_git_source(&mut doc, "x", "v", "t", None),
                "{broken}"
            );
        }
    }

    const PYPROJECT: &str = r#"[project]
name = "project-addons"
dynamic = ["dependencies"]

[project.optional-dependencies]
oca = [
  "odoo-addon-web_responsive==17.0.1.2.0",  # pinned
]
oca_unmerged = [
  "odoo-addon-pos_vouchers @ git+https://github.com/OCA/pos.git@refs/pull/45/head#subdirectory=pos_vouchers",
  "Odoo-Addon-Pos_Extra[test] @ git+ssh://git@github.com/OCA/pos.git@abc1234#subdirectory=pos_extra ; python_version >= '3.10'",
]

[dependency-groups]
private = ["odoo-addon-private-reports"]

[tool.uv.sources]
odoo = { git = "https://github.com/OCA/OCB.git", branch = "17.0" }
odoo-addon-x = { git = "https://forge.example/x.git", rev = "abc", subdirectory = "x" }
odoo-addon-y = { index = "private" }
"#;

    fn doc() -> DocumentMut {
        PYPROJECT.parse().unwrap()
    }

    #[test]
    fn requirement_forms() {
        let plain = parse_requirement("odoo-addon-web_responsive==17.0.1.2.0").unwrap();
        assert_eq!(
            (
                plain.name.as_str(),
                plain.specifier.as_str(),
                plain.direct,
                plain.markers
            ),
            ("odoo-addon-web_responsive", "==17.0.1.2.0", None, None)
        );
        let extras = parse_requirement("x[a,b] >=1 ; os_name == 'posix'").unwrap();
        assert_eq!(extras.specifier, ">=1");
        assert_eq!(extras.extras.as_deref(), Some("a,b"));
        assert_eq!(extras.markers.as_deref(), Some("os_name == 'posix'"));
        let bare = parse_requirement("odoo").unwrap();
        assert_eq!(bare.specifier, "");
        assert!(
            parse_requirement(">=1")
                .unwrap_err()
                .contains("distribution name")
        );
    }

    #[test]
    fn direct_reference_forms() {
        let ssh = parse_requirement("x @ git+ssh://git@github.com/OCA/pos.git@abc1234").unwrap();
        let direct = ssh.direct.unwrap();
        assert_eq!(direct.repository, "ssh://git@github.com/OCA/pos.git");
        assert_eq!(direct.reference.as_deref(), Some("abc1234"));
        assert_eq!(direct.subdirectory, None);
        let no_ref =
            parse_requirement("x @ https://example.com/x.whl#sha256=00&subdirectory=y").unwrap();
        let direct = no_ref.direct.unwrap();
        assert_eq!(direct.repository, "https://example.com/x.whl");
        assert_eq!(
            (direct.reference, direct.subdirectory.as_deref()),
            (None, Some("y"))
        );
        let host_only = parse_requirement("x @ git+https://example.com").unwrap();
        assert_eq!(host_only.direct.unwrap().repository, "https://example.com");
        let relative = parse_requirement("x @ file.whl").unwrap();
        assert_eq!(relative.direct.unwrap().repository, "file.whl");
    }

    #[test]
    fn reads_groups_from_both_tables() {
        let doc = doc();
        let oca = read_group(&doc, "oca").unwrap();
        assert_eq!(oca.table, GroupTable::OptionalDependencies);
        assert!(!oca.uses_direct_references());
        let unmerged = read_group(&doc, "oca_unmerged").unwrap();
        assert!(unmerged.uses_direct_references());
        assert_eq!(unmerged.requirements[1].name, "Odoo-Addon-Pos_Extra");
        let private = read_group(&doc, "private").unwrap();
        assert_eq!(private.table, GroupTable::DependencyGroups);
        assert_eq!(read_group(&doc, "missing"), None);
    }

    #[test]
    fn rewrites_touch_only_the_entry() {
        let mut doc = doc();
        let group = read_group(&doc, "oca_unmerged").unwrap();
        assert!(set_direct_ref(
            &mut doc,
            &group,
            "odoo-addon-pos-extra",
            "git+https://forge.example/vault.git@refs/tags/odx/abc1234#subdirectory=pos_extra"
        ));
        let expected = PYPROJECT.replace(
            "Odoo-Addon-Pos_Extra[test] @ git+ssh://git@github.com/OCA/pos.git@abc1234#subdirectory=pos_extra ; python_version >= '3.10'",
            "Odoo-Addon-Pos_Extra[test] @ git+https://forge.example/vault.git@refs/tags/odx/abc1234#subdirectory=pos_extra ; python_version >= '3.10'",
        );
        assert_eq!(doc.to_string(), expected);
        assert!(!set_direct_ref(&mut doc, &group, "odoo-addon-missing", "u"));
        let oca = read_group(&doc, "oca").unwrap();
        assert!(!set_direct_ref(
            &mut doc,
            &oca,
            "odoo-addon-web-responsive",
            "u"
        ));
    }

    #[test]
    fn add_and_remove_in_groups() {
        let mut doc = doc();
        let group = read_group(&doc, "oca_unmerged").unwrap();
        assert!(add_to_group(
            &mut doc,
            &group,
            "odoo-addon-z @ git+https://x/z.git@main"
        ));
        let text = doc.to_string();
        assert!(text.contains("pos_extra ; python_version >= '3.10'\",\n  \"odoo-addon-z @ git+https://x/z.git@main\",\n]"), "{text}");
        let groups = [group, read_group(&doc, "oca").unwrap()];
        assert_eq!(
            remove_from_groups(&mut doc, &groups, "odoo_addon.pos_vouchers"),
            1
        );
        assert_eq!(
            remove_from_groups(&mut doc, &groups, "odoo-addon-web-responsive"),
            0
        );
        let empty: DocumentMut = "[dependency-groups]\nnew = []\n".parse().unwrap();
        let mut empty = empty;
        let new = read_group(&empty, "new").unwrap();
        assert!(add_to_group(&mut empty, &new, "a @ git+https://x/a.git"));
        assert_eq!(
            empty.to_string(),
            "[dependency-groups]\nnew = [\"a @ git+https://x/a.git\"]\n"
        );
        let ghost = Group {
            name: "ghost".into(),
            table: GroupTable::DependencyGroups,
            requirements: vec![],
        };
        assert!(!add_to_group(&mut empty, &ghost, "x"));
        assert!(!set_direct_ref(&mut empty, &ghost, "x", "u"));
        assert_eq!(remove_from_groups(&mut empty, &[ghost], "x"), 0);
    }

    #[test]
    fn uv_sources() {
        let mut doc = doc();
        let sources = read_uv_git_sources(&doc);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[1].rev.as_deref(), Some("abc"));
        assert_eq!(sources[0].branch.as_deref(), Some("17.0"));
        assert!(remove_uv_source(&mut doc, "Odoo_Addon_X"));
        assert!(!remove_uv_source(&mut doc, "odoo-addon-x"));
        assert!(!doc.to_string().contains("odoo-addon-x ="));
        let mut bare = DocumentMut::new();
        assert_eq!(read_uv_git_sources(&bare), []);
        assert!(!remove_uv_source(&mut bare, "x"));
    }

    #[test]
    fn unexpected_shapes_are_skipped() {
        let doc: DocumentMut = "project = 1\n[dependency-groups]\nnot_a_list = 1\nmixed = [1, { include-group = \"x\" }, \"a @ git+https://x/a.git@main\"]\n[tool.uv.sources]\nweird = 1\n"
            .parse()
            .unwrap();
        assert_eq!(read_group(&doc, "not_a_list"), None);
        assert_eq!(read_group(&doc, "missing"), None);
        let mixed = read_group(&doc, "mixed").unwrap();
        assert_eq!(mixed.requirements.len(), 1);
        assert_eq!(read_uv_git_sources(&doc), []);

        let mut doc = doc;
        assert!(set_direct_ref(
            &mut doc,
            &mixed,
            "a",
            "git+https://y/a.git@v1"
        ));
        assert!(
            doc.to_string()
                .contains("mixed = [1, { include-group = \"x\" }, \"a @ git+https://y/a.git@v1\"]")
        );

        let optional = Group {
            name: "x".into(),
            table: GroupTable::OptionalDependencies,
            requirements: vec![],
        };
        assert!(!add_to_group(&mut doc, &optional, "a"));
        assert!(!set_direct_ref(&mut doc, &optional, "a", "u"));
        assert_eq!(remove_from_groups(&mut doc, &[optional], "a"), 0);

        let mut not_a_table: DocumentMut =
            "[project]\noptional-dependencies = 1\n".parse().unwrap();
        assert_eq!(read_group(&not_a_table, "x"), None);
        let group = Group {
            name: "x".into(),
            table: GroupTable::OptionalDependencies,
            requirements: vec![],
        };
        assert!(!add_to_group(&mut not_a_table, &group, "a"));
        assert!(!set_direct_ref(&mut not_a_table, &group, "a", "u"));
        assert_eq!(remove_from_groups(&mut not_a_table, &[group], "a"), 0);
    }

    #[test]
    fn names_normalize() {
        assert_eq!(normalize_name("_leading"), "leading");
        assert_eq!(normalize_name("a--b"), "a-b");
    }
}
