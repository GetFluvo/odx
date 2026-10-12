//! git-aggregator's `--dirmatch`: Python `fnmatch` against the target
//! directory.

use std::path::{Component, Path, PathBuf};

/// Whether `name` matches the shell-style `pattern`: `*` matches anything
/// (including `/`), `?` one character, `[seq]` / `[!seq]` a character (not)
/// in `seq`. An unclosed `[` is literal.
///
/// ```
/// use git_aggregate_rs::fnmatch;
///
/// assert!(fnmatch("./addons/sale-workflow", "*/sale-*"));
/// assert!(fnmatch("pos", "p[!a]s"));
/// assert!(!fnmatch("pos", "p?"));
/// ```
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let name = normcase(name);
    let pattern = normcase(pattern);
    matches(&name, &pattern)
}

/// Case and separators as Python's `os.path.normcase`: on Windows,
/// lowercase with `\` as separator; elsewhere unchanged.
#[cfg(windows)]
fn normcase(text: &str) -> Vec<char> {
    text.to_lowercase().replace('/', "\\").chars().collect()
}

#[cfg(not(windows))]
fn normcase(text: &str) -> Vec<char> {
    text.chars().collect()
}

fn matches(name: &[char], pattern: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => {
            (0..=name.len()).any(|skip| matches(name.get(skip..).unwrap_or_default(), rest))
        }
        Some((first, rest)) => {
            let Some((c, name_rest)) = name.split_first() else {
                return false;
            };
            let (ok, rest) = match first {
                '?' => (true, rest),
                '[' => match class(rest) {
                    Some((set, negated, after)) => (set.contains(c) != negated, after),
                    None => (*c == '[', rest),
                },
                literal => (c == literal, rest),
            };
            ok && matches(name_rest, rest)
        }
    }
}

/// The characters of a `[...]` class (after its `[`), whether it is
/// negated, and the pattern after its `]`; `None` when it is not closed.
fn class(pattern: &[char]) -> Option<(Vec<char>, bool, &[char])> {
    let (negated, body) = match pattern.split_first() {
        Some(('!', body)) => (true, body),
        _ => (false, pattern),
    };
    // A `]` right after `[` or `[!` is part of the set.
    let close = body.iter().skip(1).position(|c| *c == ']').map(|i| i + 1)?;
    let (items, after) = body.split_at(close);
    let mut set = Vec::new();
    let mut i = 0;
    while let Some(&c) = items.get(i) {
        if let (Some('-'), Some(&end)) = (items.get(i + 1), items.get(i + 2)) {
            set.extend(c..=end);
            i += 3;
        } else {
            set.push(c);
            i += 1;
        }
    }
    Some((set, negated, after.get(1..).unwrap_or_default()))
}

/// `path` without `.` components and with `..` applied, lexically.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Whether target directory `dir` (as written in `repos.yaml`, relative to
/// `cwd`) is selected by `pattern`, as git-aggregator decides: the pattern
/// matches the absolute path or the path relative to `cwd`, or names the
/// same relative path.
pub(crate) fn match_dir(cwd: &Path, dir: &str, pattern: &str) -> bool {
    let cwd = normalize(cwd);
    let absolute = normalize(&cwd.join(dir));
    let relative = absolute.strip_prefix(&cwd).unwrap_or(&absolute);
    let named = normalize(&cwd.join(pattern));
    fnmatch(&absolute.to_string_lossy(), pattern)
        || fnmatch(&relative.to_string_lossy(), pattern)
        || absolute == named
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(fnmatch("", ""));
        assert!(fnmatch("", "*"));
        assert!(!fnmatch("a", ""));
        assert!(!fnmatch("", "?"));
        assert!(fnmatch("a/b/c", "a*c"));
        assert!(fnmatch("abc", "a?c"));
        assert!(!fnmatch("abx", "a*c"));
    }

    #[test]
    fn classes() {
        assert!(fnmatch("b", "[abc]"));
        assert!(!fnmatch("d", "[abc]"));
        assert!(fnmatch("d", "[!abc]"));
        assert!(fnmatch("5", "[0-9]"));
        assert!(fnmatch("-", "[a-]"));
        assert!(fnmatch("]", "[]]"));
        assert!(!fnmatch("]", "[!]a]"));
        assert!(fnmatch("x", "[!]a]"));
        assert!(fnmatch("[a", "[a"));
        assert!(!fnmatch("b", "[a"));
        assert!(!fnmatch("", "[a]"));
    }

    #[cfg(not(windows))]
    #[test]
    fn directories() {
        let cwd = Path::new("/work/./project");
        assert!(match_dir(cwd, "./pos", "pos"));
        assert!(match_dir(cwd, "pos", "./pos"));
        assert!(match_dir(cwd, "./addons/pos", "addons/*"));
        assert!(match_dir(cwd, "./pos", "/work/project/p*"));
        assert!(match_dir(cwd, "../other", "/work/other"));
        assert!(match_dir(cwd, "/abs/web", "/abs/*"));
        assert!(!match_dir(cwd, "./web", "pos"));
        assert_eq!(normalize(Path::new("../a")), Path::new("../a"));
        assert_eq!(normalize(Path::new("./a")), Path::new("a"));
    }

    #[cfg(windows)]
    #[test]
    fn directories_ignore_case_on_windows() {
        let cwd = Path::new("C:\\work\\project");
        assert!(match_dir(cwd, "./Pos", "pos"));
        assert!(match_dir(cwd, "./addons/pos", "ADDONS/*"));
    }
}
