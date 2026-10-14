//! Fetching wheels with their hash checked, and extracting addon
//! directories from wheels and `git archive` output.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use odx_forge::{ForgeError, Http};
use sha2::{Digest, Sha256};

use crate::VendorError;
use crate::content::hex;

/// The wheel at `url` with sha256 `hash` (`sha256:<hex>`), from the wheel
/// cache `cache` or downloaded (`file://` URLs and plain paths are read
/// from disk) and cached. A download whose hash differs is refused.
///
/// # Errors
///
/// When the wheel cannot be read or downloaded, or its hash differs.
pub fn fetch(http: &dyn Http, cache: &Path, url: &str, hash: &str) -> Result<Vec<u8>, VendorError> {
    let expected = hash.strip_prefix("sha256:").unwrap_or(hash).to_owned();
    let cached = cache.join(format!("{expected}.whl"));
    if let Ok(bytes) = std::fs::read(&cached)
        && hex(&Sha256::digest(&bytes)) == expected
    {
        return Ok(bytes);
    }
    let bytes = get(http, url)?;
    let actual = hex(&Sha256::digest(&bytes));
    if actual != expected {
        return Err(VendorError::Hash {
            url: url.to_owned(),
            expected: format!("sha256:{expected}"),
            actual: format!("sha256:{actual}"),
        });
    }
    std::fs::create_dir_all(cache)
        .and_then(|()| std::fs::write(&cached, &bytes))
        .map_err(|source| VendorError::Io {
            path: cached,
            source,
        })?;
    Ok(bytes)
}

/// The bytes at `url`: over HTTP(S), or from disk for `file://` URLs and
/// plain paths.
///
/// # Errors
///
/// When the request fails, the server does not answer 200, or the file
/// cannot be read.
pub fn get(http: &dyn Http, url: &str) -> Result<Vec<u8>, VendorError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let response = http.get(url, &[]).map_err(download_error(url))?;
        if response.status == 200 {
            Ok(response.body)
        } else {
            Err(VendorError::Download {
                url: url.to_owned(),
                message: format!("HTTP {}", response.status),
            })
        }
    } else {
        let path = local_path(url);
        std::fs::read(&path).map_err(|source| VendorError::Io { path, source })
    }
}

/// Turns an HTTP failure for `what` (a URL) into a [`VendorError`].
pub fn download_error(what: &str) -> impl Fn(ForgeError) -> VendorError + '_ {
    move |error| VendorError::Download {
        url: what.to_owned(),
        message: error.to_string(),
    }
}

/// The file a `file://` URL (or a plain path) names.
pub fn local_path(url: &str) -> PathBuf {
    let path = url.strip_prefix("file://").unwrap_or(url);
    // file:///C:/x on Windows.
    let path = path
        .strip_prefix('/')
        .filter(|rest| rest.get(1..2) == Some(":"))
        .unwrap_or(path);
    PathBuf::from(path)
}

/// For a wheel entry `odoo/addons/<addon>/<path>`: the addon and the path.
pub fn wheel_entry(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("odoo/addons/")?;
    let (addon, path) = rest.split_once('/')?;
    (!addon.is_empty() && !path.is_empty()).then(|| (addon.to_owned(), path.to_owned()))
}

/// For a `git archive` entry `<subdirectory>/<path>`: the addon (the last
/// component of `subdirectory`) and the path.
pub fn subdirectory_entry(subdirectory: &str, name: &str) -> Option<(String, String)> {
    let subdirectory = subdirectory.trim_matches('/');
    let path = name.strip_prefix(subdirectory)?.strip_prefix('/')?;
    let addon = subdirectory.rsplit('/').next().unwrap_or(subdirectory);
    (!path.is_empty()).then(|| (addon.to_owned(), path.to_owned()))
}

/// Write the addon files of `zip` into `dest/<addon>/<path>`.
///
/// `select` maps an entry to its addon and path; entries `exclude` matches
/// (by path inside the addon) are skipped. Returns the addons written,
/// sorted. Entries that would land outside `dest` are refused.
///
/// # Errors
///
/// When `zip` is not a zip archive or a file cannot be written.
pub fn extract(
    zip: &[u8],
    what: &str,
    select: &dyn Fn(&str) -> Option<(String, String)>,
    exclude: &dyn Fn(&str) -> bool,
    dest: &Path,
) -> Result<Vec<String>, VendorError> {
    let broken = |message: String| VendorError::Archive {
        what: what.to_owned(),
        message,
    };
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| broken(e.to_string()))?;
    let mut addons = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|e| broken(e.to_string()))?;
        if file.is_dir() {
            continue;
        }
        let name = String::from_utf8_lossy(file.name_raw()).into_owned();
        let Some((addon, path)) = select(&name) else {
            continue;
        };
        let safe = Path::new(&addon)
            .join(&path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)));
        if !safe {
            return Err(broken(format!("{name} leaves the addon directory")));
        }
        if exclude(&path) {
            continue;
        }
        let target = dest.join(&addon).join(&path);
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| broken(e.to_string()))?;
        target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&target, &bytes))
            .map_err(|source| VendorError::Io {
                path: target,
                source,
            })?;
        addons.push(addon);
    }
    addons.sort();
    addons.dedup();
    Ok(addons)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Mutex;

    use odx_forge::HttpResponse;

    /// A zip with `files` (name, content).
    pub(crate) fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .last_modified_time(zip::DateTime::default());
        for (name, content) in files {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    /// Answers every GET with the next response.
    #[derive(Debug, Default)]
    pub(crate) struct Answers(pub(crate) Mutex<Vec<Result<HttpResponse, ForgeError>>>);

    impl Http for Answers {
        fn get(&self, _: &str, _: &[(&str, String)]) -> Result<HttpResponse, ForgeError> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop()
                .unwrap_or_else(|| Err(ForgeError::Http("no answer".to_owned())))
        }

        fn post(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _: &serde_json::Value,
        ) -> Result<HttpResponse, ForgeError> {
            self.get(url, headers)
        }

        #[cfg_attr(coverage_nightly, coverage(off))]
        fn put(
            &self,
            url: &str,
            headers: &[(&str, String)],
            _: &serde_json::Value,
        ) -> Result<HttpResponse, ForgeError> {
            self.get(url, headers)
        }
    }

    pub(crate) fn sha(bytes: &[u8]) -> String {
        format!("sha256:{}", hex(&Sha256::digest(bytes)))
    }

    #[test]
    fn fetches_and_caches_wheels() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let wheel = zip(&[("odoo/addons/a/__init__.py", "")]);
        let hash = sha(&wheel);
        let http = Answers(Mutex::new(vec![Ok(HttpResponse {
            status: 200,
            body: wheel.clone(),
            ..HttpResponse::default()
        })]));
        let url = "https://files.example/a.whl";
        assert_eq!(fetch(&http, &cache, url, &hash).unwrap(), wheel);
        // From the cache: no second request.
        assert_eq!(fetch(&http, &cache, url, &hash).unwrap(), wheel);
        // A corrupted cache entry is fetched again.
        let cached = cache.join(format!("{}.whl", hash.trim_start_matches("sha256:")));
        std::fs::write(&cached, "corrupt").unwrap();
        let err = fetch(&http, &cache, url, &hash).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("cannot download {url}: request failed: no answer")
        );

        let http = Answers(Mutex::new(vec![Ok(HttpResponse {
            status: 404,
            ..HttpResponse::default()
        })]));
        let err = fetch(&http, &cache, url, &hash).unwrap_err();
        assert_eq!(err.to_string(), format!("cannot download {url}: HTTP 404"));
        assert!(http.post(url, &[], &serde_json::json!({})).is_err());

        // Local wheels, and a hash mismatch.
        let local = tmp.path().join("a.whl");
        std::fs::write(&local, &wheel).unwrap();
        std::fs::remove_file(&cached).unwrap();
        let url = format!("file://{}", local.display().to_string().replace('\\', "/"));
        assert_eq!(fetch(&http, &cache, &url, &hash).unwrap(), wheel);
        let other = sha(b"other");
        let err = fetch(&http, &cache, &url, &other).unwrap_err();
        assert!(err.to_string().contains("hash mismatch"), "{err}");
        let err = fetch(&http, &cache, "/missing.whl", &other).unwrap_err();
        assert!(err.to_string().contains("missing.whl"), "{err}");
        // The cache cannot be written.
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        assert!(fetch(&http, &blocked, local.to_str().unwrap(), &hash).is_err());
    }

    #[test]
    fn local_paths() {
        assert_eq!(local_path("file:///tmp/a.whl"), Path::new("/tmp/a.whl"));
        assert_eq!(local_path("file:///C:/x/a.whl"), Path::new("C:/x/a.whl"));
        assert_eq!(local_path("C:/x/a.whl"), Path::new("C:/x/a.whl"));
        assert_eq!(local_path("rel/a.whl"), Path::new("rel/a.whl"));
    }

    #[test]
    fn entries() {
        assert_eq!(
            wheel_entry("odoo/addons/web_x/static/x.js"),
            Some(("web_x".to_owned(), "static/x.js".to_owned()))
        );
        for name in [
            "odoo/addons/web_x/",
            "odoo/addons//x",
            "odoo/addons/web_x",
            "odoo/__init__.py",
            "x.dist-info/RECORD",
        ] {
            assert_eq!(wheel_entry(name), None, "{name}");
        }
        assert_eq!(
            subdirectory_entry("setup/pos_x/", "setup/pos_x/models/a.py"),
            Some(("pos_x".to_owned(), "models/a.py".to_owned()))
        );
        assert_eq!(subdirectory_entry("pos_x", "pos_x/"), None);
        assert_eq!(subdirectory_entry("pos_x", "pos_xy/a.py"), None);
        assert_eq!(subdirectory_entry("pos_x", "other/a.py"), None);
    }

    #[test]
    fn extracts_addons() {
        let tmp = tempfile::tempdir().unwrap();
        let wheel = zip(&[
            ("odoo/", ""),
            ("odoo/addons/a/__manifest__.py", "{}"),
            ("odoo/addons/a/static/description/index.html", "<html/>"),
            ("odoo/addons/b/models/x.py", "x"),
            ("a-1.dist-info/RECORD", ""),
        ]);
        let exclude = |path: &str| path == "static/description/index.html";
        let addons = extract(&wheel, "a.whl", &wheel_entry, &exclude, tmp.path()).unwrap();
        assert_eq!(addons, ["a", "b"]);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a/__manifest__.py")).unwrap(),
            "{}"
        );
        assert!(!tmp.path().join("a/static/description/index.html").exists());
        assert!(!tmp.path().join("a-1.dist-info").exists());

        let evil = zip(&[("odoo/addons/a/../../../evil.py", "x")]);
        let err = extract(&evil, "evil.whl", &wheel_entry, &|_| false, tmp.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "evil.whl: odoo/addons/a/../../../evil.py leaves the addon directory"
        );
        // An entry compressed with a method odx cannot read, and one whose
        // content does not match its checksum.
        let stored = zip(&[("odoo/addons/a/x.py", "content-abc")]);
        let mut unsupported = stored.clone();
        for (signature, offset) in [(&b"PK\x03\x04"[..], 8), (&b"PK\x01\x02"[..], 10)] {
            let at = unsupported
                .windows(4)
                .position(|window| window == signature)
                .unwrap();
            unsupported[at + offset] = 99;
        }
        let err = extract(&unsupported, "m.whl", &wheel_entry, &|_| false, tmp.path()).unwrap_err();
        assert!(err.to_string().starts_with("m.whl: "), "{err}");
        let mut corrupt = stored;
        let at = corrupt
            .windows(11)
            .position(|window| window == b"content-abc")
            .unwrap();
        corrupt[at] = b'C';
        let err = extract(&corrupt, "c.whl", &wheel_entry, &|_| false, tmp.path()).unwrap_err();
        assert!(err.to_string().starts_with("c.whl: "), "{err}");
        let err = extract(b"not a zip", "x.whl", &wheel_entry, &|_| false, tmp.path()).unwrap_err();
        assert!(err.to_string().starts_with("x.whl: "), "{err}");
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        assert!(extract(&wheel, "a.whl", &wheel_entry, &|_| false, &blocked).is_err());
    }
}
