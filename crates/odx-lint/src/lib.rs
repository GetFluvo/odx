//! odoo-lint integration and project-level rules for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). [`lint`] runs
//! [odoo-lint](https://github.com/bosd/odoo-lint) in-process over the given
//! paths with the project's context (target series, addons path), applying
//! its fixes when asked; [`render`] prints the findings in odoo-lint's
//! formats. Rule selection and configuration are odoo-lint's
//! (`[tool.odoo-lint]`).

use std::path::{Path, PathBuf};

pub mod project;
pub mod schema;

pub use odoo_lint::diagnostics::Violation;
pub use odoo_lint::output::OutputFormat;

/// The odoo-lint version odx embeds, as a literal for `concat!`.
#[macro_export]
macro_rules! odoo_lint_version {
    () => {
        "0.1.0-alpha.17"
    };
}

/// Which fixes to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fixes {
    /// Report only.
    None,
    /// Safe fixes, written to the files.
    Safe,
    /// Safe and unsafe fixes, written to the files.
    Unsafe,
}

/// What to lint, and how.
#[derive(Debug, Clone)]
pub struct Request {
    /// Where odoo-lint's configuration is looked for, and what `exclude`
    /// and `per-file-ignores` are relative to.
    pub root: PathBuf,
    /// Files and directories to lint.
    pub paths: Vec<PathBuf>,
    /// The Odoo series, overriding the configuration's.
    pub target: Option<String>,
    /// Folders with other addons, for the checks across modules; none
    /// keeps the configuration's.
    pub addons_path: Vec<PathBuf>,
    /// The languages PO105 checks the translations of, unless odoo-lint's
    /// configuration names them.
    pub languages: Vec<String>,
    /// Rules to select instead of the configuration's.
    pub select: Option<Vec<String>>,
    /// Rules to ignore on top of the configuration's.
    pub ignore: Vec<String>,
    /// Which fixes to apply.
    pub fixes: Fixes,
    /// Compute the fixes without writing them.
    pub diff: bool,
}

/// What a run found and changed.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Findings left, sorted.
    pub violations: Vec<Violation>,
    /// Fixes applied (or that would be, with `diff`).
    pub fixed: usize,
    /// Unified diffs of the changed files, with `diff`.
    pub diffs: Vec<String>,
    /// Files written.
    pub written: Vec<PathBuf>,
    /// odoo-lint's configuration file, when one was found.
    pub config: Option<PathBuf>,
    /// Selectors that match no rule, and the like.
    pub warnings: Vec<String>,
}

/// Lint `request.paths` with odoo-lint.
///
/// # Errors
///
/// When odoo-lint's configuration is invalid, or a fixed file cannot be
/// written.
pub fn lint(request: &Request) -> Result<Outcome, String> {
    let overrides = odoo_lint::settings::CliOverrides {
        target_version: request.target.clone(),
        select: request.select.clone(),
        ignore: request.ignore.clone(),
        force_exclude: false,
        addons_path: (!request.addons_path.is_empty()).then(|| {
            request
                .addons_path
                .iter()
                .map(|dir| dir.to_string_lossy().into_owned())
                .collect()
        }),
    };
    let mut loaded = odoo_lint::settings::Settings::load(&request.root, None, overrides)?;
    loaded
        .settings
        .config
        .rules
        .get_or_insert_with(Default::default)
        .po_translation_missing
        .get_or_insert_with(Default::default)
        .languages
        .get_or_insert_with(|| request.languages.clone());
    let mut outcome = Outcome {
        config: loaded.config_path,
        warnings: loaded.warnings,
        ..Outcome::default()
    };
    let settings = &loaded.settings;
    let mode = match request.fixes {
        Fixes::None if !request.diff => {
            outcome.violations = odoo_lint::linter::lint_paths(&request.paths, settings);
            return Ok(outcome);
        }
        Fixes::None | Fixes::Safe => odoo_lint::fix::FixMode::Safe,
        Fixes::Unsafe => odoo_lint::fix::FixMode::Unsafe,
    };
    let result = odoo_lint::fixer::fix_paths(&request.paths, settings, mode);
    outcome.fixed = result.fixed;
    outcome.violations = result.remaining;
    for (path, old, new) in &result.changed {
        if request.diff {
            outcome
                .diffs
                .push(odoo_lint::fixer::unified_diff(path, old, new));
        } else {
            std::fs::write(path, new).map_err(|e| format!("{}: {e}", path.display()))?;
            outcome.written.push(path.clone());
        }
    }
    Ok(outcome)
}

/// `violations` in `format`, as `odl check` prints them.
#[must_use]
pub fn render(format: OutputFormat, violations: &[Violation]) -> String {
    odoo_lint::output::render(format, violations)
}

/// The addon directory `file` belongs to: the nearest folder at or above it
/// with a manifest, below `root`.
#[must_use]
pub fn addon_of(root: &Path, file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .take_while(|dir| dir.starts_with(root) && *dir != root)
        .find(|dir| {
            odoo_lint::manifest::MANIFEST_FILE_NAMES
                .iter()
                .any(|name| dir.join(name).is_file())
        })
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// An addon whose manifest lacks a license (a safe fix adds none, so
    /// it stays) and whose model file has an unused import.
    fn project() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("addons/my_addon/__manifest__.py"),
            "{'name': 'My addon', 'version': '18.0.1.0.0', 'depends': ['base']}\n",
        );
        write(&tmp.path().join("addons/my_addon/__init__.py"), "");
        tmp
    }

    fn request(root: &Path) -> Request {
        Request {
            root: root.to_path_buf(),
            paths: vec![root.join("addons")],
            target: Some("18.0".to_owned()),
            addons_path: Vec::new(),
            languages: Vec::new(),
            select: None,
            ignore: Vec::new(),
            fixes: Fixes::None,
            diff: false,
        }
    }

    #[test]
    fn findings_and_formats() {
        let tmp = project();
        let outcome = lint(&request(tmp.path())).unwrap();
        assert_ne!(outcome.violations, []);
        assert_eq!(outcome.config, None);
        let text = render(OutputFormat::Text, &outcome.violations);
        assert!(text.contains("__manifest__.py"), "{text}");
        let github = render(OutputFormat::Github, &outcome.violations);
        assert!(github.starts_with("::"), "{github}");
        // Selecting nothing that exists finds nothing, and says so.
        let none = lint(&Request {
            select: Some(vec!["NOPE9999".to_owned()]),
            ..request(tmp.path())
        })
        .unwrap();
        assert_eq!(none.violations, []);
        assert_ne!(none.warnings, Vec::<String>::new());
        // The addons path is passed on.
        let pathed = lint(&Request {
            addons_path: vec![tmp.path().join("elsewhere")],
            ..request(tmp.path())
        })
        .unwrap();
        assert_eq!(pathed.violations, outcome.violations);
    }

    #[test]
    fn fixes_written_or_shown() {
        let tmp = tempfile::tempdir().unwrap();
        // A manifest key set to its default, which odoo-lint removes
        // safely.
        let manifest = tmp.path().join("addons/my_addon/__manifest__.py");
        write(
            &manifest,
            "{\n    'name': 'My addon',\n    'version': '18.0.1.0.0',\n    'depends': ['base'],\n    'application': False,\n}\n",
        );
        write(&tmp.path().join("addons/my_addon/__init__.py"), "");
        let before = std::fs::read_to_string(&manifest).unwrap();
        let shown = lint(&Request {
            diff: true,
            ..request(tmp.path())
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&manifest).unwrap(), before);
        for (fixes, expected) in [(Fixes::Safe, true), (Fixes::Unsafe, true)] {
            let tmp_copy = tempfile::tempdir().unwrap();
            let copy = tmp_copy.path().join("addons/my_addon/__manifest__.py");
            write(&copy, &before);
            write(&tmp_copy.path().join("addons/my_addon/__init__.py"), "");
            let fixed = lint(&Request {
                fixes,
                ..request(tmp_copy.path())
            })
            .unwrap();
            assert_eq!(!fixed.written.is_empty(), expected, "{fixes:?}");
            assert_eq!(fixed.fixed, shown.fixed);
        }
        assert!(shown.fixed > 0, "{shown:?}");
        assert!(shown.diffs[0].contains("application"), "{:?}", shown.diffs);
    }

    #[test]
    fn broken_configuration() {
        let tmp = project();
        write(&tmp.path().join("odoo-lint.toml"), "select = 3\n");
        assert!(lint(&request(tmp.path())).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn unwritable_fixed_files() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("addons/my_addon/__manifest__.py");
        write(
            &manifest,
            "{\n    'name': 'X',\n    'application': False,\n}\n",
        );
        write(&tmp.path().join("addons/my_addon/__init__.py"), "");
        std::fs::set_permissions(&manifest, std::fs::Permissions::from_mode(0o444)).unwrap();
        let error = lint(&Request {
            fixes: Fixes::Safe,
            ..request(tmp.path())
        })
        .unwrap_err();
        assert!(error.contains("__manifest__.py"), "{error}");
    }

    #[test]
    fn addons_of_files() {
        let tmp = project();
        let root = tmp.path();
        let addon = root.join("addons/my_addon");
        assert_eq!(
            addon_of(root, &addon.join("models/x.py")),
            Some(addon.clone())
        );
        assert_eq!(addon_of(root, &root.join("README.md")), None);
        assert_eq!(addon_of(&addon, &addon.join("__init__.py")), None);
        assert_eq!(odoo_lint_version!(), "0.1.0-alpha.17");
        // The version above is the one depended on.
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .unwrap();
        assert!(
            manifest.contains(concat!("\"=", odoo_lint_version!(), "\"")),
            "{manifest}"
        );
    }
}
