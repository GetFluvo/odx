//! The user configuration: machine-wide settings that never live in a
//! project (`~/.config/odx/config.toml` on Linux).
//!
//! Only `[cache]`, `[clones]` and `[trust]` are interpreted so far; the other
//! documented tables are accepted and ignored. Project settings
//! (repositories, packages, environments) are rejected so a project cannot
//! be reconfigured from here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use odx_core::{Env, FileSystem};
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::error::line_column;
use crate::{ConfigError, Span};

/// The user configuration file.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// Shared cache settings.
    pub cache: Option<UserCache>,
    /// Local clones the cache can borrow objects from.
    pub clones: Option<Clones>,
    /// Background sync.
    pub sync: Option<SyncSettings>,
    /// Forge access defaults (not interpreted yet).
    pub http: Option<IgnoredAny>,
    /// Upstream mirrors (not interpreted yet).
    pub mirrors: Option<IgnoredAny>,
    /// Workspaces registered for `odx sync --all`, by path.
    #[serde(default)]
    pub workspaces: BTreeMap<String, Workspace>,
    /// The verdict repository (not interpreted yet).
    pub verdicts: Option<IgnoredAny>,
    /// Projects whose shell hooks may run without `--allow-shell`.
    pub trust: Option<Trust>,
}

/// `[sync]` of the user configuration.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncSettings {
    /// Whether background sync is installed (`odx daemon install` sets it).
    pub enabled: Option<bool>,
    /// Time between background syncs.
    pub interval: Option<Span>,
    /// Longest run of one sync.
    pub max_duration: Option<Span>,
    /// Share of the remaining API quota per host a sync may use.
    pub api_budget: Option<f64>,
    /// Concurrent fetches during a sync.
    pub jobs: Option<usize>,
    /// Low CPU and IO priority.
    pub nice: Option<bool>,
    /// Skip syncs on battery power (not interpreted yet).
    pub pause_on_battery: Option<bool>,
}

/// A registered workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    /// When it was registered (`YYYY-MM-DD`).
    pub registered: Option<String>,
    /// Its Odoo series.
    pub series: Option<String>,
}

/// `[trust]` of the user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    /// Project roots (`~/` expanded).
    #[serde(default)]
    pub projects: Vec<PathBuf>,
}

/// Whether the project at `root` is listed under `[trust] projects`.
pub fn trusted(user: &UserConfig, root: &Path, env: &dyn Env) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let root = canonical(root);
    user.trust.as_ref().is_some_and(|trust| {
        trust
            .projects
            .iter()
            .any(|project| canonical(&expand_home(project, env)) == root)
    })
}

/// `[cache]` of the user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserCache {
    /// Cache directory.
    pub dir: Option<PathBuf>,
    /// Size cap (not interpreted yet).
    pub max_size: Option<String>,
    /// Register workspaces for background sync (not interpreted yet).
    pub register_workspaces: Option<bool>,
    /// Partial-fetch filter, e.g. `"blob:none"` (not interpreted yet).
    pub filter: Option<String>,
}

/// `[clones]` of the user configuration.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clones {
    /// Directories holding one clone per subdirectory.
    #[serde(default)]
    pub dirs: Vec<PathBuf>,
    /// How the cache uses the clones.
    pub mode: Option<CloneMode>,
}

/// How the cache uses local clones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloneMode {
    /// Borrow objects only; clones are never written.
    #[default]
    Reference,
    /// Also fetch base branches into the clones' remote-tracking refs.
    Update,
}

/// Expand a leading `~/` with `HOME` (or `USERPROFILE`).
///
/// ```
/// use odx_config::user::expand_home;
/// use odx_core::fake::MapEnv;
/// use std::path::Path;
///
/// let env = MapEnv::default().with("HOME", "/home/dev");
/// assert_eq!(expand_home(Path::new("~/git/OCA"), &env), Path::new("/home/dev/git/OCA"));
/// assert_eq!(expand_home(Path::new("/srv/git"), &env), Path::new("/srv/git"));
/// ```
pub fn expand_home(path: &Path, env: &dyn Env) -> PathBuf {
    let home = env.var("HOME").or_else(|| env.var("USERPROFILE"));
    match (path.to_str().and_then(|p| p.strip_prefix("~/")), home) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => path.to_path_buf(),
    }
}

/// The shared cache directory outside any project: `ODX_CACHE_DIR`, else
/// `[cache].dir` of the user configuration, else the platform cache
/// directory.
pub fn cache_dir(user: &UserConfig, env: &dyn Env) -> Option<PathBuf> {
    if let Some(dir) = env.var("ODX_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = user.cache.as_ref().and_then(|cache| cache.dir.as_ref()) {
        return Some(expand_home(dir, env));
    }
    platform_cache_dir(env)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_cache_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env.var("HOME").map(|home| Path::new(&home).join(".cache")))
        .map(|dir| dir.join("odx"))
}

#[cfg(target_os = "macos")]
fn platform_cache_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("HOME")
        .map(|home| Path::new(&home).join("Library").join("Caches").join("odx"))
}

#[cfg(windows)]
fn platform_cache_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("LOCALAPPDATA")
        .map(|dir| Path::new(&dir).join("odx").join("cache"))
}

/// The user configuration directory: `ODX_CONFIG_DIR`, else the platform
/// configuration directory.
pub fn config_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("ODX_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| platform_config_dir(env))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform_config_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env.var("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|dir| dir.join("odx"))
}

#[cfg(target_os = "macos")]
fn platform_config_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("HOME")
        .map(|home| Path::new(&home).join("Library/Application Support/odx"))
}

#[cfg(windows)]
fn platform_config_dir(env: &dyn Env) -> Option<PathBuf> {
    env.var("APPDATA").map(|dir| Path::new(&dir).join("odx"))
}

/// Parse user configuration text.
///
/// # Errors
///
/// [`ConfigError::Syntax`] for invalid TOML and unknown keys, with position.
pub fn parse_user_config(text: &str, file: &Path) -> Result<UserConfig, ConfigError> {
    toml::from_str(text).map_err(|error| {
        let (line, column) = line_column(text, error.span().map_or(0, |span| span.start));
        ConfigError::Syntax {
            file: file.to_path_buf(),
            line,
            column,
            message: error.message().to_owned(),
        }
    })
}

/// Load `config.toml` from the user configuration directory; a missing
/// file (or no known directory) is an empty configuration.
///
/// # Errors
///
/// [`ConfigError::Io`] when the file exists but cannot be read, and the
/// errors of [`parse_user_config`].
pub fn load_user_config(fs: &dyn FileSystem, env: &dyn Env) -> Result<UserConfig, ConfigError> {
    let Some(file) = config_dir(env).map(|dir| dir.join("config.toml")) else {
        return Ok(UserConfig::default());
    };
    if !fs.exists(&file) {
        return Ok(UserConfig::default());
    }
    let bytes = fs.read(&file).map_err(|source| ConfigError::Io {
        file: file.clone(),
        source,
    })?;
    parse_user_config(&String::from_utf8_lossy(&bytes), &file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use odx_core::fake::{MapEnv, MemoryFs};
    use std::io::ErrorKind;

    const FULL: &str = r#"
[cache]
dir = "~/.cache/odx"
max_size = "20GB"
register_workspaces = true
filter = "blob:none"

[clones]
dirs = ["~/git/OCA"]
mode = "update"

[sync]
enabled = false

[http]
per_host_concurrency = 6

[mirrors]
"github.com" = { url = "https://forge.example/vault.git", prefer = true }

[workspaces]
"/home/dev/p" = { registered = "2026-10-12", series = "17.0" }

[verdicts]
repo = "https://forge.example/verdicts.git"

[trust]
projects = ["/home/dev/p"]
"#;

    #[test]
    fn trusted_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::default().with("HOME", tmp.path().display().to_string());
        let user = parse_user_config(
            "[trust]\nprojects = [\"/nowhere/at/all\", \"~/p\"]\n",
            Path::new("c"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("p")).unwrap();
        assert!(trusted(&user, &tmp.path().join("p"), &env));
        assert!(!trusted(&user, tmp.path(), &env));
        assert!(!trusted(&UserConfig::default(), tmp.path(), &env));
    }

    #[test]
    fn documented_file_parses() {
        let config = parse_user_config(FULL, Path::new("config.toml")).unwrap();
        let clones = config.clones.unwrap();
        assert_eq!(clones.dirs, [PathBuf::from("~/git/OCA")]);
        assert_eq!(clones.mode, Some(CloneMode::Update));
        assert_eq!(config.cache.unwrap().filter.as_deref(), Some("blob:none"));
        assert_eq!(CloneMode::default(), CloneMode::Reference);
        let reference =
            parse_user_config("[clones]\nmode = \"reference\"\n", Path::new("c")).unwrap();
        assert_eq!(reference.clones.unwrap().mode, Some(CloneMode::Reference));
    }

    #[test]
    fn project_settings_are_rejected() {
        let err = parse_user_config("[[repositories]]\nurl = \"u\"\n", Path::new("config.toml"))
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("config.toml:1:3: unknown field `repositories`"),
            "{err}"
        );
    }

    #[test]
    fn home_expansion_edge_cases() {
        let windows = MapEnv::default().with("USERPROFILE", "C:/Users/dev");
        assert_eq!(
            expand_home(Path::new("~/x"), &windows),
            Path::new("C:/Users/dev").join("x")
        );
        assert_eq!(
            expand_home(Path::new("~/x"), &MapEnv::default()),
            Path::new("~/x")
        );
    }

    #[test]
    fn loading() {
        let fs = MemoryFs::default();
        assert_eq!(
            load_user_config(&fs, &MapEnv::default()).unwrap(),
            UserConfig::default()
        );
        let env = MapEnv::default().with("ODX_CONFIG_DIR", "/cfg");
        assert_eq!(load_user_config(&fs, &env).unwrap(), UserConfig::default());
        fs.write(
            Path::new("/cfg/config.toml"),
            b"[clones]\ndirs = [\"/srv/git\"]\n",
        )
        .unwrap();
        assert_eq!(
            load_user_config(&fs, &env).unwrap().clones.unwrap().dirs,
            [PathBuf::from("/srv/git")]
        );
        fs.fail_next(ErrorKind::PermissionDenied);
        let err = load_user_config(&fs, &env).unwrap_err();
        let file = Path::new("/cfg").join("config.toml");
        assert!(
            err.to_string()
                .starts_with(&format!("{}: ", file.display())),
            "{err}"
        );
    }

    #[test]
    fn cache_dir_precedence() {
        let user = parse_user_config("[cache]\ndir = \"~/c\"\n", Path::new("c")).unwrap();
        let env = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(cache_dir(&user, &env), Some(PathBuf::from("/home/dev/c")));
        let env = env.with("ODX_CACHE_DIR", "/tmp/odx");
        assert_eq!(cache_dir(&user, &env), Some(PathBuf::from("/tmp/odx")));
        assert_eq!(cache_dir(&UserConfig::default(), &MapEnv::default()), None);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_cache_dir() {
        let user = UserConfig::default();
        let xdg = MapEnv::default().with("XDG_CACHE_HOME", "/x");
        assert_eq!(cache_dir(&user, &xdg), Some(PathBuf::from("/x/odx")));
        let home = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            cache_dir(&user, &home),
            Some(PathBuf::from("/home/dev/.cache/odx"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cache_dir() {
        let home = MapEnv::default().with("HOME", "/Users/dev");
        assert_eq!(
            cache_dir(&UserConfig::default(), &home),
            Some(PathBuf::from("/Users/dev/Library/Caches/odx"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_cache_dir() {
        let env = MapEnv::default().with("LOCALAPPDATA", "C:/Users/dev/AppData/Local");
        assert_eq!(
            cache_dir(&UserConfig::default(), &env),
            Some(
                Path::new("C:/Users/dev/AppData/Local")
                    .join("odx")
                    .join("cache")
            )
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn xdg_config_dir() {
        let xdg = MapEnv::default().with("XDG_CONFIG_HOME", "/x");
        assert_eq!(config_dir(&xdg), Some(PathBuf::from("/x/odx")));
        let home = MapEnv::default().with("HOME", "/home/dev");
        assert_eq!(
            config_dir(&home),
            Some(PathBuf::from("/home/dev/.config/odx"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_dir() {
        let home = MapEnv::default().with("HOME", "/Users/dev");
        assert_eq!(
            config_dir(&home),
            Some(PathBuf::from("/Users/dev/Library/Application Support/odx"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_config_dir() {
        let env = MapEnv::default().with("APPDATA", "C:/Users/dev/AppData/Roaming");
        assert_eq!(
            config_dir(&env),
            Some(Path::new("C:/Users/dev/AppData/Roaming").join("odx"))
        );
    }
}
