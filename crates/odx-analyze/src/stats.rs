//! `odx stats`: lines of code per language, counted by tokei.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokei::{Config, Language, Languages};

/// The totals of one language, one addon or everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Row {
    /// Language or addon name, or `Total`.
    pub name: String,
    /// Files counted.
    pub files: usize,
    /// All lines.
    pub lines: usize,
    /// Lines of code.
    pub code: usize,
    /// Comment lines.
    pub comments: usize,
    /// Blank lines.
    pub blanks: usize,
}

impl Row {
    fn of(name: &str, language: &Language) -> Self {
        Self {
            name: name.to_owned(),
            files: language.reports.len(),
            lines: language.lines(),
            code: language.code,
            comments: language.comments,
            blanks: language.blanks,
        }
    }

    /// The sum of `rows`, named `name`.
    pub fn sum<'a>(name: &str, rows: impl IntoIterator<Item = &'a Self>) -> Self {
        rows.into_iter().fold(
            Self {
                name: name.to_owned(),
                ..Self::default()
            },
            |total, row| Self {
                files: total.files + row.files,
                lines: total.lines + row.lines,
                code: total.code + row.code,
                comments: total.comments + row.comments,
                blanks: total.blanks + row.blanks,
                ..total
            },
        )
    }
}

/// Count every file under `dirs` the way the `tokei` command does:
/// `.gitignore`, `.ignore` and `.tokeignore` are respected and hidden files
/// are skipped. Paths in the reports are relative to `root` where possible.
pub fn count(dirs: &[PathBuf], root: &Path) -> Languages {
    let mut languages = Languages::new();
    if !dirs.is_empty() {
        languages.get_statistics(dirs, &[], &Config::default());
    }
    for (_, language) in &mut languages {
        let reports = language
            .reports
            .iter_mut()
            .chain(language.children.values_mut().flatten());
        for report in reports {
            if let Ok(relative) = report.name.strip_prefix(root) {
                report.name = relative.to_path_buf();
            }
        }
        language.sort_by(tokei::Sort::Files);
    }
    languages
}

/// One row per language with lines, embedded code (such as JavaScript in
/// HTML or code blocks in Markdown) counted with its host language, as
/// tokei's summary does; empty languages are left out.
pub fn rows(languages: &Languages) -> Vec<Row> {
    languages
        .into_iter()
        .map(|(kind, language)| Row::of(&kind.to_string(), &language.summarise()))
        .filter(|row| row.files > 0)
        .collect()
}

/// The addon directories under `dir`: `dir` itself when it holds a
/// `__manifest__.py`, else its subdirectories that do, sorted.
pub fn addon_dirs(dir: &Path) -> Vec<PathBuf> {
    if dir.join("__manifest__.py").is_file() {
        return vec![dir.to_path_buf()];
    }
    let mut addons: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_or_else(|_| Vec::new(), |entries| entries.flatten().collect())
        .iter()
        .map(std::fs::DirEntry::path)
        .filter(|path| path.join("__manifest__.py").is_file())
        .collect();
    addons.sort();
    addons
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn counts_languages_of_addons() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            &root.join("custom/a/__manifest__.py"),
            "{\"name\": \"A\"}\n",
        );
        write(
            &root.join("custom/a/models/a.py"),
            "# A model.\nimport os\n\n\nx = 1\n",
        );
        write(
            &root.join("custom/a/views/a.xml"),
            "<odoo>\n<!-- view -->\n</odoo>\n",
        );
        write(
            &root.join("custom/a/README.md"),
            "# A\n\n```python\nx = 1\n```\n",
        );
        write(&root.join("custom/b/__manifest__.py"), "{}\n");
        write(&root.join("custom/notes/x.py"), "x = 1\n");
        write(&root.join("single/__manifest__.py"), "{}\n");

        let addons = addon_dirs(&root.join("custom"));
        assert_eq!(addons, [root.join("custom/a"), root.join("custom/b")]);
        assert_eq!(addon_dirs(&root.join("single")), [root.join("single")]);
        assert_eq!(addon_dirs(&root.join("missing")), Vec::<PathBuf>::new());

        let languages = count(&addons, root);
        let rows = rows(&languages);
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Markdown", "Python", "XML"]);
        let python = &rows[1];
        assert_eq!(
            (python.files, python.code, python.comments, python.blanks),
            (3, 4, 1, 2)
        );
        let total = Row::sum("Total", &rows);
        let tokei_total = languages.total();
        assert_eq!(total.code, tokei_total.code);
        assert_eq!(total.comments, tokei_total.comments);
        assert_eq!(total.blanks, tokei_total.blanks);
        assert_eq!(total.lines, total.code + total.comments + total.blanks);
        let (_, python) = languages
            .into_iter()
            .find(|(kind, _)| *kind == tokei::LanguageType::Python)
            .unwrap();
        assert!(
            python
                .reports
                .iter()
                .any(|report| report.name == Path::new("custom/a/models/a.py"))
        );

        assert!(count(&[], root).total().is_empty());
        // A path outside the root keeps its full name.
        let elsewhere = tempfile::tempdir().unwrap();
        write(&elsewhere.path().join("x.py"), "x = 1\n");
        let outside = count(&[elsewhere.path().to_path_buf()], root);
        let (_, python) = outside.into_iter().next().unwrap();
        assert!(python.reports[0].name.is_absolute());
    }
}
