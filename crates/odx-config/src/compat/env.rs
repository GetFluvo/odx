/// Expand `$VAR` and `${VAR}` in `text` (git-aggregator's `--expand-env`).
///
/// Unknown variables are left as written, so a literal `$` that is not a
/// variable reference survives; `$$` is a literal `$`.
///
/// ```
/// use odx_config::compat::expand_env;
///
/// let lookup = |name: &str| (name == "ORG").then(|| "OCA".to_owned());
/// assert_eq!(
///     expand_env("https://github.com/${ORG}/pos.git $ORG $MISSING $$", &lookup),
///     "https://github.com/OCA/pos.git OCA $MISSING $"
/// );
/// ```
pub fn expand_env(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('$') {
        out.push_str(before);
        rest = expand_one(after, lookup, &mut out);
    }
    out.push_str(rest);
    out
}

/// Expand the reference that follows a `$`; returns the remaining input.
fn expand_one<'a>(
    after: &'a str,
    lookup: &dyn Fn(&str) -> Option<String>,
    out: &mut String,
) -> &'a str {
    if let Some(tail) = after.strip_prefix('$') {
        out.push('$');
        return tail;
    }
    let (name, consumed) = after
        .strip_prefix('{')
        .and_then(|braced| braced.split_once('}'))
        .map_or_else(
            || {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (after.split_at(end).0, end)
            },
            |(name, _)| (name, name.len() + 2),
        );
    let (raw, tail) = after.split_at(consumed);
    if let Some(value) = (!name.is_empty()).then(|| lookup(name)).flatten() {
        out.push_str(&value);
    } else {
        out.push('$');
        out.push_str(raw);
    }
    tail
}

/// Parse an env file: `KEY=VALUE` lines, `#` comments, optional `export `
/// prefix and optional matching quotes around the value.
///
/// ```
/// use odx_config::compat::parse_env_file;
///
/// let vars = parse_env_file("# org\nexport ORG=\"OCA\"\nBRANCH='17.0'\nEMPTY=\n\nnot a pair\n");
/// assert_eq!(vars, [("ORG".into(), "OCA".into()), ("BRANCH".into(), "17.0".into()), ("EMPTY".into(), String::new())]);
/// ```
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let unquoted = ['"', '\'']
                .into_iter()
                .find_map(|q| value.strip_prefix(q)?.strip_suffix(q))
                .unwrap_or(value);
            Some((key.trim().to_owned(), unquoted.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn edge_cases_are_left_alone() {
        assert_eq!(expand_env("price: 5$", &none), "price: 5$");
        assert_eq!(expand_env("${UNCLOSED", &none), "${UNCLOSED");
        assert_eq!(expand_env("${}", &none), "${}");
        assert_eq!(expand_env("a $ b", &none), "a $ b");
        assert_eq!(expand_env("no vars", &none), "no vars");
        assert_eq!(expand_env("$NOPE and ${NOPE}", &none), "$NOPE and ${NOPE}");
    }
}
