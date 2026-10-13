//! The `custom` platform profile's hooks: project commands odx runs to
//! prepare and verify a deployment, only when the user allows shell
//! commands for the project.

use odx_config::schema::PlatformProfile;
use odx_config::user::{load_user_config, trusted};
use odx_core::Invocation;
use odx_platform::{profile, requirements_path};

use super::aggregate::Prepared;
use super::run_checked;
use super::vendor::vendor_manifest;
use crate::Context;

#[cfg(windows)]
const SHELL: [&str; 2] = ["cmd", "/C"];
#[cfg(not(windows))]
const SHELL: [&str; 2] = ["sh", "-c"];

/// Which hook.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Hook {
    /// After `odx vendor`.
    Prepare,
    /// During `odx verify`.
    Verify,
}

/// What came of a hook.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ran {
    /// Not the `custom` profile, or no command configured.
    Nothing,
    /// Configured but not allowed: the message says how to allow it.
    NotAllowed(String),
    /// Ran and succeeded.
    Succeeded,
    /// Ran and failed, with why.
    Failed(String),
}

/// Run `hook` of the `custom` profile when it is configured and allowed
/// (`--allow-shell`, or the project is listed under `[trust] projects`).
pub(crate) fn run_hook(
    ctx: &Context<'_>,
    prepared: &Prepared,
    hook: Hook,
    allow_shell: bool,
) -> Ran {
    let config = &prepared.project.config;
    let root = &prepared.project.root;
    let (name, command) = match hook {
        Hook::Prepare => (
            "prepare",
            custom_command(config, |custom| custom.prepare.clone()),
        ),
        Hook::Verify => (
            "verify",
            custom_command(config, |custom| custom.verify.clone()),
        ),
    };
    let Some(command) = command.filter(|_| profile(config) == PlatformProfile::Custom) else {
        return Ran::Nothing;
    };
    let user = load_user_config(ctx.fs, ctx.env).unwrap_or_default();
    if !allow_shell && !trusted(&user, root, ctx.env) {
        return Ran::NotAllowed(format!(
            "the custom {name} hook `{command}` was not run; pass --allow-shell or list the project under [trust] projects"
        ));
    }
    let [shell, flag] = SHELL;
    let invocation = Invocation::new(shell)
        .arg(flag)
        .arg(&command)
        .current_dir(root)
        .env("ODX_PROJECT_ROOT", root.as_os_str())
        .env("ODX_ENVIRONMENT", "")
        .env("ODX_SERIES", &config.odoo_version)
        .env("ODX_LOCKFILE", prepared.lock_path.as_os_str())
        .env(
            "ODX_VENDOR_MANIFEST",
            root.join(vendor_manifest(config)).as_os_str(),
        )
        .env(
            "ODX_REQUIREMENTS",
            root.join(requirements_path(config)).as_os_str(),
        );
    run_checked(&invocation, &format!("the custom {name} hook `{command}`"))
        .map_or_else(Ran::Failed, |()| Ran::Succeeded)
}

fn custom_command(
    config: &odx_config::OdxConfig,
    pick: impl Fn(&odx_config::schema::CustomHooks) -> Option<String>,
) -> Option<String> {
    config
        .platform
        .as_ref()
        .and_then(|platform| platform.custom.as_ref())
        .and_then(pick)
}
