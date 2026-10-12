use std::fmt;
use std::io;
use std::path::PathBuf;

/// A problem found while validating a configuration, located by its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Dotted key path, e.g. `tool.odx.repositories[0].merges[1]`.
    pub key: String,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.message)
    }
}

/// Errors from locating, reading or validating a project configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("{file}: {source}", file = file.display())]
    Io {
        /// The file.
        file: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML, or a value has the wrong type or an unknown key.
    #[error("{file}:{line}:{column}: {message}", file = file.display())]
    Syntax {
        /// The file.
        file: PathBuf,
        /// 1-based line.
        line: usize,
        /// 1-based column.
        column: usize,
        /// The parser's message.
        message: String,
    },
    /// No `pyproject.toml` with a `[tool.odx]` table was found.
    #[error("no pyproject.toml with a [tool.odx] table in {start} or its parents", start = start.display())]
    NotFound {
        /// Where the search started.
        start: PathBuf,
    },
    /// The given `pyproject.toml` has no `[tool.odx]` table.
    #[error("{file}: no [tool.odx] table", file = file.display())]
    NoOdxTable {
        /// The file.
        file: PathBuf,
    },
    /// The configuration parsed but is inconsistent.
    #[error("{file}: invalid configuration:\n{list}", file = file.display(), list = list(problems))]
    Invalid {
        /// The file.
        file: PathBuf,
        /// Every problem found.
        problems: Vec<Problem>,
    },
}

impl ConfigError {
    /// The consistency problems of an [`ConfigError::Invalid`] error; empty
    /// for every other kind.
    pub fn problems(&self) -> &[Problem] {
        match self {
            Self::Invalid { problems, .. } => problems,
            _ => &[],
        }
    }
}

fn list(problems: &[Problem]) -> String {
    problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 1-based line and column of byte `offset` in `text`.
pub(crate) fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_column_counts_from_one() {
        let text = "a = 1\nbé = 2\n";
        assert_eq!(line_column(text, 0), (1, 1));
        assert_eq!(line_column(text, 6), (2, 1));
        assert_eq!(line_column(text, 9), (2, 3));
        assert_eq!(line_column(text, 999), (3, 1));
    }

    #[test]
    fn invalid_lists_every_problem() {
        let err = ConfigError::Invalid {
            file: PathBuf::from("pyproject.toml"),
            problems: vec![
                Problem {
                    key: "tool.odx.a".into(),
                    message: "bad".into(),
                },
                Problem {
                    key: "tool.odx.b".into(),
                    message: "worse".into(),
                },
            ],
        };
        assert_eq!(
            err.to_string(),
            "pyproject.toml: invalid configuration:\n  - tool.odx.a: bad\n  - tool.odx.b: worse"
        );
        assert_eq!(err.problems().len(), 2);
        let not_found = ConfigError::NotFound {
            start: PathBuf::from("/"),
        };
        assert_eq!(not_found.problems(), []);
    }
}
