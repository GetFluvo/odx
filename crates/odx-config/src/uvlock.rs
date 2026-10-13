//! Reading `uv.lock`: resolved versions, sources, dependencies and wheel
//! hashes, as far as odx needs them.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::deps::normalize_name;

/// A parsed `uv.lock`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UvLock {
    /// Every resolved package.
    #[serde(default, rename = "package")]
    pub packages: Vec<UvPackage>,
}

/// One `[[package]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UvPackage {
    /// Normalized name, as uv writes it.
    pub name: String,
    /// Resolved version (absent for some local packages).
    pub version: Option<String>,
    /// Where it comes from.
    #[serde(default)]
    pub source: UvSource,
    /// Runtime dependencies.
    #[serde(default)]
    pub dependencies: Vec<UvDependency>,
    /// Extras (`[project.optional-dependencies]`) → dependencies.
    #[serde(default)]
    pub optional_dependencies: BTreeMap<String, Vec<UvDependency>>,
    /// Dependency groups (`[dependency-groups]`) → dependencies.
    #[serde(default)]
    pub dev_dependencies: BTreeMap<String, Vec<UvDependency>>,
    /// Built distributions with their hashes.
    #[serde(default)]
    pub wheels: Vec<UvWheel>,
    /// The project's declared requirements (only for the project itself).
    #[serde(default)]
    pub metadata: UvMetadata,
}

/// `source = { ... }` of a package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UvSource {
    /// Index URL.
    pub registry: Option<String>,
    /// `<url>?<ref>#<commit>`.
    pub git: Option<String>,
    /// The project itself, not built.
    #[serde(rename = "virtual")]
    pub virtual_path: Option<String>,
    /// An editable local package (the project, or develop mode).
    pub editable: Option<String>,
    /// A local directory.
    pub directory: Option<String>,
    /// A local file.
    pub path: Option<String>,
    /// A direct URL.
    pub url: Option<String>,
}

/// A dependency edge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UvDependency {
    /// Normalized name.
    pub name: String,
    /// Environment marker, if any.
    pub marker: Option<String>,
}

/// A wheel of a package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UvWheel {
    /// Download URL (or file name for local wheels).
    pub url: Option<String>,
    /// Local path, for path sources.
    pub path: Option<String>,
    /// `sha256:<hex>`.
    pub hash: Option<String>,
}

/// `[package.metadata]` of the project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct UvMetadata {
    /// `[project] dependencies` and `optional-dependencies` as declared.
    #[serde(default)]
    pub requires_dist: Vec<UvRequirement>,
    /// `[dependency-groups]` as declared.
    #[serde(default)]
    pub requires_dev: BTreeMap<String, Vec<UvRequirement>>,
}

/// A declared requirement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UvRequirement {
    /// Name as normalized by uv.
    pub name: String,
    /// Version specifier, if any.
    pub specifier: Option<String>,
    /// Marker, including `extra == '<group>'` for optional dependencies.
    pub marker: Option<String>,
}

impl UvRequirement {
    /// The extra (`[project.optional-dependencies]` group) this
    /// requirement belongs to, from its `extra == '<name>'` marker.
    pub fn extra(&self) -> Option<&str> {
        let marker = self.marker.as_deref()?;
        let (_, after) = marker.split_once("extra ==")?;
        let quoted = after.trim_start().strip_prefix(['\'', '"'])?;
        quoted.split(['\'', '"']).next()
    }
}

impl UvLock {
    /// Parse `uv.lock` text.
    ///
    /// # Errors
    ///
    /// When the text is not TOML in uv's lockfile layout.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.message().to_owned())
    }

    /// The project itself: the package whose source is the project
    /// directory (`virtual` or `editable` `"."`).
    pub fn root(&self) -> Option<&UvPackage> {
        self.packages.iter().find(|package| {
            package.source.virtual_path.as_deref() == Some(".")
                || package.source.editable.as_deref() == Some(".")
        })
    }

    /// The packages named `name` (more than one when uv resolved different
    /// versions per platform or Python version).
    pub fn packages_named(&self, name: &str) -> Vec<&UvPackage> {
        let name = normalize_name(name);
        self.packages
            .iter()
            .filter(|package| normalize_name(&package.name) == name)
            .collect()
    }

    /// Normalized name → resolved versions, for every package with a
    /// version.
    pub fn versions(&self) -> BTreeMap<String, Vec<String>> {
        let mut versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for package in &self.packages {
            if let Some(version) = &package.version {
                versions
                    .entry(normalize_name(&package.name))
                    .or_default()
                    .push(version.clone());
            }
        }
        versions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
version = 1
requires-python = ">=3.10"

[[package]]
name = "p"
version = "17.0.1.0.0"
source = { virtual = "." }
dependencies = [{ name = "requests" }]

[package.optional-dependencies]
oca = [{ name = "odoo-addon-web-responsive" }]

[package.dev-dependencies]
dev = [{ name = "pytest" }]

[package.metadata]
requires-dist = [
    { name = "odoo-addon-web-responsive", marker = "extra == 'oca'", specifier = "==17.0.1.1.9" },
    { name = "requests", specifier = ">=2.30" },
]

[package.metadata.requires-dev]
dev = [{ name = "pytest" }]

[[package]]
name = "odoo-addon-web-responsive"
version = "17.0.1.1.9"
source = { registry = "https://pypi.org/simple" }
dependencies = [{ name = "odoo" }, { name = "requests", marker = "python_version >= '3.10'" }]
wheels = [{ url = "https://files/x.whl", hash = "sha256:aa" }]

[[package]]
name = "requests"
version = "2.31.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "requests"
version = "2.32.0"
source = { registry = "https://pypi.org/simple" }

[[package]]
name = "odoo"
version = "17.0"
source = { git = "https://github.com/OCA/OCB.git?branch=17.0#0123" }
"#;

    #[test]
    fn reads_the_lock() {
        let lock = UvLock::parse(LOCK).unwrap();
        let root = lock.root().unwrap();
        assert_eq!(root.name, "p");
        assert_eq!(
            root.optional_dependencies["oca"][0].name,
            "odoo-addon-web-responsive"
        );
        assert_eq!(root.dev_dependencies["dev"][0].name, "pytest");
        let extras: Vec<Option<&str>> = root
            .metadata
            .requires_dist
            .iter()
            .map(UvRequirement::extra)
            .collect();
        assert_eq!(extras, [Some("oca"), None]);
        assert_eq!(root.metadata.requires_dev["dev"][0].name, "pytest");
        let addon = &lock.packages_named("odoo_addon.web_responsive")[0];
        assert_eq!(addon.wheels[0].hash.as_deref(), Some("sha256:aa"));
        assert_eq!(
            addon.dependencies[1].marker.as_deref(),
            Some("python_version >= '3.10'")
        );
        assert_eq!(lock.packages_named("requests").len(), 2);
        let versions = lock.versions();
        assert_eq!(versions["requests"], ["2.31.0", "2.32.0"]);
        assert!(lock.packages_named("odoo")[0].source.git.is_some());
        assert!(UvLock::parse("package = 1").is_err());
        assert_eq!(UvLock::parse("").unwrap().root(), None);
        let editable =
            UvLock::parse("[[package]]\nname = \"p\"\nsource = { editable = \".\" }\n").unwrap();
        assert_eq!(editable.root().unwrap().version, None);
        assert!(editable.versions().is_empty());
    }

    #[test]
    fn extras_from_markers() {
        let requirement = |marker: Option<&str>| UvRequirement {
            name: "x".to_owned(),
            specifier: None,
            marker: marker.map(str::to_owned),
        };
        assert_eq!(requirement(Some("extra == \"oca\"")).extra(), Some("oca"));
        assert_eq!(
            requirement(Some("python_version >= '3.10' and extra == 'b'")).extra(),
            Some("b")
        );
        assert_eq!(requirement(Some("python_version >= '3.10'")).extra(), None);
        assert_eq!(requirement(Some("extra ==")).extra(), None);
        assert_eq!(requirement(None).extra(), None);
    }
}
