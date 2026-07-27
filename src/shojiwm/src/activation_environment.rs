use std::process::{Command, Output};

use tracing::{debug, warn};

const ACTIVATION_ENV_KEYS: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_TYPE",
    "DESKTOP_SESSION",
    // freedesktop menu-spec variable selecting ${prefix}applications.menu.
    // D-Bus/systemd-activated helpers that build application menus or
    // databases resolve the menu with THIS environment, not the session's;
    // without the prefix they fall back to a menu file the distro may not
    // ship. Filtered out below like every other key when the session does
    // not set it.
    "XDG_MENU_PREFIX",
];

pub fn publish_activation_environment(reason: &'static str) {
    if std::env::var_os("SHOJI_PUBLISH_ACTIVATION_ENV")
        .is_some_and(|value| value == "0" || value == "off")
    {
        return;
    }

    let keys = ACTIVATION_ENV_KEYS
        .iter()
        .copied()
        .filter(|key| std::env::var_os(key).is_some())
        .collect::<Vec<_>>();
    publish_activation_environment_keys(reason, &keys);
}

pub fn publish_activation_environment_keys(reason: &'static str, keys: &[&str]) {
    if std::env::var_os("SHOJI_PUBLISH_ACTIVATION_ENV")
        .is_some_and(|value| value == "0" || value == "off")
    {
        return;
    }

    let keys = keys
        .iter()
        .copied()
        .filter(|key| is_valid_env_key(key) && std::env::var_os(key).is_some())
        .collect::<Vec<_>>();
    if keys.is_empty() {
        return;
    }

    let systemd_result = run_dbus_update_activation_environment(&["--systemd"], &keys);
    match systemd_result {
        Ok(output) if output.status.success() => {
            debug!(?keys, reason, "published systemd activation environment");
        }
        Ok(output) => {
            debug!(
                ?keys,
                reason,
                status = ?output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "systemd activation environment update failed; trying D-Bus fallback"
            );
        }
        Err(error) => {
            debug!(
                ?keys,
                reason,
                ?error,
                "failed to run systemd activation environment update; trying D-Bus fallback"
            );
        }
    }

    match run_dbus_update_activation_environment(&[], &keys) {
        Ok(output) if output.status.success() => {
            debug!(?keys, reason, "published D-Bus activation environment");
        }
        Ok(output) => {
            warn!(
                ?keys,
                reason,
                status = ?output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "failed to publish D-Bus activation environment"
            );
        }
        Err(error) => {
            warn!(
                ?keys,
                reason,
                ?error,
                "failed to run dbus-update-activation-environment"
            );
        }
    }
}

fn run_dbus_update_activation_environment(
    flags: &[&str],
    keys: &[&str],
) -> std::io::Result<Output> {
    Command::new("dbus-update-activation-environment")
        .args(flags)
        .args(keys)
        .output()
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeEnvUpdates {
    #[serde(default)]
    pub operations: Vec<RuntimeEnvOperation>,
    #[serde(default)]
    pub publish: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct RuntimeEnvOperation {
    pub key: String,
    pub value: Option<String>,
}

pub fn apply_runtime_env_updates(updates: RuntimeEnvUpdates, reason: &'static str) {
    for operation in updates.operations {
        if !is_valid_env_key(&operation.key) {
            warn!(key = %operation.key, "ignoring invalid runtime environment key");
            continue;
        }
        match operation.value {
            Some(value) => unsafe { std::env::set_var(&operation.key, value) },
            None => unsafe { std::env::remove_var(&operation.key) },
        }
    }

    let publish_keys = updates
        .publish
        .iter()
        .filter(|key| is_valid_env_key(key))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !publish_keys.is_empty() {
        publish_activation_environment_keys(reason, &publish_keys);
    }
}

fn is_valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains('=') && !key.contains('\0')
}
