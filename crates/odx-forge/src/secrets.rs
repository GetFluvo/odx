//! Stored forge tokens: the operating system's keyring (macOS Keychain,
//! Windows Credential Manager, the Secret Service on Linux and BSD), or a
//! file for machines without one.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use odx_core::Env;

/// The keyring service odx stores tokens under; the host is the user name.
pub const SERVICE: &str = "odx";

/// Where tokens are stored.
pub trait Secrets: fmt::Debug {
    /// The token stored for `host`.
    ///
    /// # Errors
    ///
    /// When the store cannot be read.
    fn get(&self, host: &str) -> Result<Option<String>, String>;

    /// Store `token` for `host`.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    fn set(&self, host: &str, token: &str) -> Result<(), String>;

    /// Remove the token of `host`; whether there was one.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    fn delete(&self, host: &str) -> Result<bool, String>;

    /// The store, for messages.
    fn describe(&self) -> String;
}

/// The store `env` selects: the file `ODX_SECRETS_FILE` names, else the
/// operating system's keyring.
pub fn secrets(env: &dyn Env) -> Box<dyn Secrets> {
    match env.var("ODX_SECRETS_FILE").filter(|path| !path.is_empty()) {
        Some(path) => Box::new(FileSecrets {
            path: PathBuf::from(path),
        }),
        None => Box::new(Keyring),
    }
}

/// The operating system's keyring.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keyring;

/// Make the platform's keyring the default store, once, unless another
/// store (such as a test's mock) was set first.
///
/// Excluded from coverage: it talks to the operating system's credential
/// service, which CI machines do not run.
#[cfg_attr(coverage_nightly, coverage(off))]
fn platform_store() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    if keyring_core::get_default_store().is_some() {
        return Ok(());
    }
    INIT.get_or_init(|| {
        #[cfg(target_os = "macos")]
        let store = apple_native_keyring_store::keychain::Store::new();
        #[cfg(target_os = "windows")]
        let store = windows_native_keyring_store::Store::new();
        #[cfg(all(unix, not(target_os = "macos")))]
        let store = zbus_secret_service_keyring_store::Store::new();
        let store = store.map_err(|e| format!("no keyring available: {e}"))?;
        keyring_core::set_default_store(store);
        Ok(())
    })
    .clone()
}

fn entry(host: &str) -> Result<keyring_core::Entry, String> {
    platform_store().and_then(|()| keyring_core::Entry::new(SERVICE, host).map_err(text))
}

#[expect(clippy::needless_pass_by_value, reason = "used as a map_err function")]
fn text(error: keyring_core::Error) -> String {
    error.to_string()
}

impl Secrets for Keyring {
    fn get(&self, host: &str) -> Result<Option<String>, String> {
        entry(host).and_then(|entry| match entry.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(text(e)),
        })
    }

    fn set(&self, host: &str, token: &str) -> Result<(), String> {
        entry(host).and_then(|entry| entry.set_password(token).map_err(text))
    }

    fn delete(&self, host: &str) -> Result<bool, String> {
        entry(host).and_then(|entry| match entry.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(e) => Err(text(e)),
        })
    }

    fn describe(&self) -> String {
        "the system keyring".to_owned()
    }
}

/// Tokens in a JSON file (host → token), readable only by its owner, for
/// machines without a keyring.
#[derive(Debug, Clone)]
pub struct FileSecrets {
    /// The file.
    pub path: PathBuf,
}

impl FileSecrets {
    fn read(&self) -> Result<BTreeMap<String, String>, String> {
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", self.path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    fn write(&self, tokens: &BTreeMap<String, String>) -> Result<(), String> {
        let text = serde_json::to_string_pretty(tokens).unwrap_or_default();
        write_private(&self.path, text.as_bytes())
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// Write `bytes` to `path`, readable and writable by its owner only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    path.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| options.open(path))
        .and_then(|mut file| std::io::Write::write_all(&mut file, bytes))
}

impl Secrets for FileSecrets {
    fn get(&self, host: &str) -> Result<Option<String>, String> {
        self.read().map(|tokens| tokens.get(host).cloned())
    }

    fn set(&self, host: &str, token: &str) -> Result<(), String> {
        let mut tokens = self.read()?;
        tokens.insert(host.to_owned(), token.to_owned());
        self.write(&tokens)
    }

    fn delete(&self, host: &str) -> Result<bool, String> {
        let mut tokens = self.read()?;
        let removed = tokens.remove(host).is_some();
        if removed {
            self.write(&tokens)?;
        }
        Ok(removed)
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::MapEnv;

    #[test]
    fn keyring_round_trip() {
        keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
        let keyring = Keyring;
        assert_eq!(keyring.get("forge.example"), Ok(None));
        assert_eq!(keyring.set("forge.example", "t1"), Ok(()));
        assert_eq!(keyring.get("forge.example"), Ok(Some("t1".to_owned())));
        assert_eq!(keyring.delete("forge.example"), Ok(true));
        assert_eq!(keyring.delete("forge.example"), Ok(false));
        assert_eq!(keyring.describe(), "the system keyring");
        assert_eq!(format!("{:?}", secrets(&MapEnv::default())), "Keyring");
        // The keyring refuses.
        let mock = keyring_core::Entry::new(SERVICE, "broken.example").unwrap();
        let cred: &keyring_core::mock::Cred = mock.as_any().downcast_ref().unwrap();
        for call in 0..3 {
            cred.set_error(keyring_core::Error::Invalid(
                "mock".to_owned(),
                "refused".to_owned(),
            ));
            let failed = match call {
                0 => keyring.get("broken.example").map(|_| ()),
                1 => keyring.set("broken.example", "t"),
                _ => keyring.delete("broken.example").map(|_| ()),
            };
            assert!(failed.unwrap_err().contains("refused"), "{call}");
        }
    }

    #[test]
    fn file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("odx").join("secrets.json");
        let env = MapEnv::default().with("ODX_SECRETS_FILE", path.display().to_string());
        let store = secrets(&env);
        assert_eq!(store.describe(), path.display().to_string());
        assert_eq!(store.get("a"), Ok(None));
        assert_eq!(store.delete("a"), Ok(false));
        store.set("a", "t1").unwrap();
        store.set("b", "t2").unwrap();
        assert_eq!(store.get("a"), Ok(Some("t1".to_owned())));
        assert_eq!(store.delete("a"), Ok(true));
        assert_eq!(store.get("b"), Ok(Some("t2".to_owned())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(&path, "not json").unwrap();
        assert!(store.get("b").unwrap_err().contains("secrets.json"));
        assert!(store.set("b", "x").is_err());
        assert!(store.delete("b").is_err());
        let unreadable = FileSecrets {
            path: tmp.path().to_path_buf(),
        };
        assert!(unreadable.get("b").is_err());
        let blocked = FileSecrets {
            path: path.join("below-a-file"),
        };
        std::fs::write(&path, "{}").unwrap();
        assert!(blocked.write(&BTreeMap::new()).is_err());
        assert!(unreadable.write(&BTreeMap::new()).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, "{\"a\": \"t\"}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400)).unwrap();
            let store = FileSecrets { path: path.clone() };
            assert!(store.delete("a").is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}
