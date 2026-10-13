//! Platform profiles, provided-package sets, requirements export, wheel builds and installed-wheel verification for odx.
//!
//! Part of [odx](https://github.com/getfluvo/odx). A platform profile says
//! how artefacts reach the hosting platform; the provided-package set says
//! what the platform already installs, so the requirements odx writes leave
//! it out.

pub mod autoupdate;
pub mod export;
pub mod installed;
pub mod provided;
pub mod requirements;
pub mod wheel;

use std::path::{Path, PathBuf};

use odx_config::OdxConfig;
pub use odx_config::deps::normalize_name;
use odx_config::schema::PlatformProfile;

use crate::provided::{NAMED_SETS, ProvidedSet};

/// The active profile: `[tool.odx.platform] profile`, `generic-wheels` by
/// default.
pub fn profile(config: &OdxConfig) -> PlatformProfile {
    config
        .platform
        .as_ref()
        .and_then(|platform| platform.profile)
        .unwrap_or_default()
}

/// The profile as written in the configuration.
pub const fn profile_name(profile: PlatformProfile) -> &'static str {
    match profile {
        PlatformProfile::GenericWheels => "generic-wheels",
        PlatformProfile::VendoredSource => "vendored-source",
        PlatformProfile::OdooSh => "odoo-sh",
        PlatformProfile::Custom => "custom",
    }
}

/// The platform requirements file, relative to the project root.
///
/// `[tool.odx.layout] requirements`, else `requirements.txt` for Odoo.sh
/// (which reads it from the repository root) and `dist/requirements.txt`
/// otherwise.
pub fn requirements_path(config: &OdxConfig) -> PathBuf {
    config
        .layout
        .as_ref()
        .and_then(|layout| layout.requirements.clone())
        .unwrap_or_else(|| match profile(config) {
            PlatformProfile::OdooSh => PathBuf::from("requirements.txt"),
            _ => PathBuf::from("dist/requirements.txt"),
        })
}

/// The provided-package set of the project.
///
/// The named set (`odoo-sh` for that profile, `odoo-standard` otherwise)
/// for its series, or the constraints file `provided` names (read with
/// `read`, relative to `root`), plus `provided_extra`, minus
/// `provided_remove`.
///
/// # Errors
///
/// When the named set does not exist for the series or the file cannot be
/// read.
pub fn provided_set(
    config: &OdxConfig,
    root: &Path,
    read: impl Fn(&Path) -> std::io::Result<String>,
) -> Result<ProvidedSet, String> {
    let platform = config.platform.as_ref();
    let name = platform
        .and_then(|platform| platform.provided.clone())
        .unwrap_or_else(|| match profile(config) {
            PlatformProfile::OdooSh => "odoo-sh".to_owned(),
            _ => "odoo-standard".to_owned(),
        });
    let series = &config.odoo_version;
    let mut set = if NAMED_SETS.contains(&name.as_str()) {
        ProvidedSet::named(&name, series)
            .ok_or_else(|| format!("odx has no provided set {name} for Odoo {series}"))?
    } else {
        let file = root.join(&name);
        let text = read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        ProvidedSet::parse(&name, &text)
    };
    for line in platform
        .and_then(|platform| platform.provided_extra.as_deref())
        .unwrap_or_default()
    {
        set.add(line);
    }
    for name in platform
        .and_then(|platform| platform.provided_remove.as_deref())
        .unwrap_or_default()
    {
        set.remove(name);
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> OdxConfig {
        toml::from_str(&format!("odoo_version = \"17.0\"\n{text}")).unwrap()
    }

    #[test]
    fn profiles_and_paths() {
        let plain = config("");
        assert_eq!(profile(&plain), PlatformProfile::GenericWheels);
        assert_eq!(
            requirements_path(&plain),
            Path::new("dist/requirements.txt")
        );
        let sh = config("[platform]\nprofile = \"odoo-sh\"\n");
        assert_eq!(requirements_path(&sh), Path::new("requirements.txt"));
        let custom = config("[layout]\nrequirements = \"deploy/req.txt\"\n");
        assert_eq!(requirements_path(&custom), Path::new("deploy/req.txt"));
        let names: Vec<&str> = [
            PlatformProfile::GenericWheels,
            PlatformProfile::VendoredSource,
            PlatformProfile::OdooSh,
            PlatformProfile::Custom,
        ]
        .into_iter()
        .map(profile_name)
        .collect();
        assert_eq!(
            names,
            ["generic-wheels", "vendored-source", "odoo-sh", "custom"]
        );
    }

    #[test]
    fn provided_sets() {
        let unreadable = |_: &Path| Err(std::io::ErrorKind::NotFound.into());
        let root = Path::new("/p");
        let set = provided_set(&config(""), root, unreadable).unwrap();
        assert_eq!(set.label, "odoo-standard-17.0");
        let sh = config(
            "[platform]\nprofile = \"odoo-sh\"\nprovided_extra = [\"rq==1.0\"]\nprovided_remove = [\"lxml\"]\n",
        );
        let set = provided_set(&sh, root, unreadable).unwrap();
        assert_eq!(set.label, "odoo-sh-17.0");
        assert_eq!(set.versions("rq"), ["1.0"]);
        assert!(!set.contains("lxml"));
        let old: OdxConfig = toml::from_str("odoo_version = \"12.0\"").unwrap();
        assert_eq!(
            provided_set(&old, root, unreadable).unwrap_err(),
            "odx has no provided set odoo-standard for Odoo 12.0"
        );
        let file = config("[platform]\nprovided = \"constraints.txt\"\n");
        let set = provided_set(&file, root, |path| {
            assert_eq!(path, Path::new("/p/constraints.txt"));
            Ok("requests==2.31.0\n".to_owned())
        })
        .unwrap();
        assert_eq!((set.label.as_str(), set.len()), ("constraints.txt", 1));
        assert!(
            provided_set(&file, root, unreadable)
                .unwrap_err()
                .contains("constraints.txt")
        );
    }
}
