use std::fs;
use std::path::PathBuf;

use crate::{ForgeError, Http, HttpResponse};

/// [`Http`] with conditional requests.
///
/// An answer with an `ETag` is kept on disk and asked for again with
/// `If-None-Match`; a `304 Not Modified` (free on GitHub's rate limit)
/// returns the kept answer. Entries are keyed by URL and credentials, so
/// answers never cross tokens.
#[derive(Debug)]
pub struct CachedHttp<H> {
    inner: H,
    dir: PathBuf,
}

impl<H: Http> CachedHttp<H> {
    /// Cache `inner`'s answers in `dir` (created when needed).
    pub fn new(inner: H, dir: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            dir: dir.into(),
        }
    }
}

/// FNV-1a: a stable, dependency-free file name for a cache key.
fn key(url: &str, headers: &[(&str, String)]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let parts = std::iter::once(url).chain(headers.iter().flat_map(|(k, v)| [*k, v.as_str()]));
    for byte in parts.flat_map(|part| part.bytes().chain([0])) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl<H: Http> Http for CachedHttp<H> {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<HttpResponse, ForgeError> {
        let name = key(url, headers);
        let etag_file = self.dir.join(format!("{name}.etag"));
        let body_file = self.dir.join(format!("{name}.body"));
        let kept = fs::read_to_string(&etag_file)
            .ok()
            .zip(fs::read(&body_file).ok());
        let mut conditional = headers.to_vec();
        if let Some((etag, _)) = &kept {
            conditional.push(("if-none-match", etag.clone()));
        }
        let response = self.inner.get(url, &conditional)?;
        match (response.status, kept) {
            (304, Some((etag, body))) => Ok(HttpResponse {
                status: 200,
                headers: vec![("etag".to_owned(), etag)],
                body,
            }),
            (200, _) => {
                if let Some(etag) = response.header("etag") {
                    // A cache that cannot be written only costs requests.
                    let _ = fs::create_dir_all(&self.dir)
                        .and_then(|()| fs::write(&body_file, &response.body))
                        .and_then(|()| fs::write(&etag_file, etag));
                }
                Ok(response)
            }
            _ => Ok(response),
        }
    }

    /// Not cached: a POST changes something.
    fn post(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError> {
        self.inner.post(url, headers, body)
    }

    /// Not cached either.
    fn put(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError> {
        self.inner.put(url, headers, body)
    }

    /// Not cached either.
    fn patch(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &serde_json::Value,
    ) -> Result<HttpResponse, ForgeError> {
        self.inner.patch(url, headers, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scripted, reply};

    fn tagged(etag: &str, body: &str) -> HttpResponse {
        HttpResponse {
            headers: vec![("etag".to_owned(), etag.to_owned())],
            ..reply(200, body)
        }
    }

    #[test]
    fn revalidates_with_etags() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("http");
        let token = [("authorization", "Bearer t".to_owned())];
        let http = CachedHttp::new(
            Scripted::new(vec![
                Ok(tagged("\"v1\"", "first")),
                Ok(reply(304, "")),
                Ok(tagged("\"v2\"", "second")),
                Ok(reply(200, "untagged")),
                Ok(reply(304, "")),
                Ok(reply(500, "")),
            ]),
            &dir,
        );
        assert_eq!(http.get("https://x/a", &token).unwrap().body, b"first");
        let again = http.get("https://x/a", &token).unwrap();
        assert_eq!((again.status, again.body.as_slice()), (200, &b"first"[..]));
        assert_eq!(http.get("https://x/a", &token).unwrap().body, b"second");
        assert_eq!(http.get("https://x/b", &token).unwrap().body, b"untagged");
        // Nothing kept for this key: a 304 is passed on as it is.
        assert_eq!(http.get("https://x/b", &token).unwrap().status, 304);
        assert_eq!(http.get("https://x/a", &[]).unwrap().status, 500);
        let seen = http.inner.seen.lock().unwrap().clone();
        assert!(
            seen[1]
                .1
                .contains(&("if-none-match".to_owned(), "\"v1\"".to_owned()))
        );
        // Other credentials: no conditional request.
        assert_eq!(seen[5].1, []);
    }

    #[test]
    fn an_unwritable_cache_only_costs_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file");
        fs::write(&file, "").unwrap();
        let http = CachedHttp::new(
            Scripted::new(vec![Ok(tagged("\"v1\"", "body"))]),
            file.join("http"),
        );
        assert_eq!(http.get("https://x/a", &[]).unwrap().body, b"body");
        let posting = CachedHttp::new(Scripted::new(vec![Ok(reply(201, ""))]), tmp.path());
        assert_eq!(
            posting
                .post("https://x/a", &[], &serde_json::json!({}))
                .unwrap()
                .status,
            201
        );
        let putting = CachedHttp::new(Scripted::new(vec![Ok(reply(200, ""))]), tmp.path());
        assert_eq!(
            putting
                .put("https://x/a", &[], &serde_json::json!({}))
                .unwrap()
                .status,
            200
        );
        let patching = CachedHttp::new(Scripted::new(vec![Ok(reply(200, ""))]), tmp.path());
        assert_eq!(
            patching
                .patch("https://x/a", &[], &serde_json::json!({}))
                .unwrap()
                .status,
            200
        );
        let failing = CachedHttp::new(
            Scripted::new(vec![Err(ForgeError::Http("down".into()))]),
            tmp.path(),
        );
        assert!(failing.get("https://x/a", &[]).is_err());
        assert_eq!(key("a", &[]), key("a", &[]));
        assert_ne!(key("a", &[]), key("a", &[("k", "v".to_owned())]));
    }
}
