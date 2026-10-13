/// `text` with the credentials of every URL replaced by `***`, so tokens
/// written into remote URLs (`https://user:token@host/…`) never reach
/// output or logs.
///
/// ```
/// use odx_core::redact;
///
/// assert_eq!(
///     redact("fetch https://bot:ghp_secret@github.com/OCA/pos.git failed"),
///     "fetch https://***@github.com/OCA/pos.git failed"
/// );
/// assert_eq!(redact("git@github.com:OCA/pos.git"), "git@github.com:OCA/pos.git");
/// ```
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("://") {
        let (head, tail) = rest.split_at(start + 3);
        out.push_str(head);
        let authority_end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '"' | '\'' | '>' | ')'))
            .unwrap_or(tail.len());
        let (authority, after) = tail.split_at(authority_end);
        match authority.rsplit_once('@') {
            Some((_, host)) => {
                out.push_str("***@");
                out.push_str(host);
            }
            None => out.push_str(authority),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_url_is_redacted() {
        assert_eq!(
            redact("a ssh://u:p@h:22/x and 'http://t@h' (https://v@w) end"),
            "a ssh://***@h:22/x and 'http://***@h' (https://***@w) end"
        );
        assert_eq!(redact("https://h"), "https://h");
        assert_eq!(redact(""), "");
    }
}
