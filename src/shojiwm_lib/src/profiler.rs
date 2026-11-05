use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use tracing::{info, warn};

pub fn init() {
    timescope::register_thread_name("shoji_wm main");
    timescope::enable_profile(false);
}

pub fn set_enabled(enabled: bool) {
    let was_enabled = timescope::is_profile_enabled();
    if was_enabled == enabled {
        return;
    }

    if enabled {
        timescope::enable_profile(true);
        info!("timescope profile enabled");
    } else {
        timescope::enable_profile(false);
        dump("disabled");
    }
}

pub fn dump_if_enabled(reason: &'static str) {
    if timescope::is_profile_enabled() {
        timescope::enable_profile(false);
        dump(reason);
    }
}

fn dump(reason: &'static str) {
    let dir = rotate_latest_profile_dir();
    if let Err(error) = fs::create_dir_all(&dir) {
        warn!(?error, path = %dir.display(), "failed to create timescope profile directory");
        return;
    }

    let options = timescope::DumpOptions {
        summary_title: "ShojiWM Timescope Profile Summary".to_owned(),
        html_title: "ShojiWM Timescope Profile Report".to_owned(),
    };

    match timescope::dump_to_dir_with_options(&dir, &options) {
        Ok(()) => {
            info!(
                reason,
                path = %dir.display(),
                "timescope profile dumped"
            );
        }
        Err(error) => {
            warn!(?error, path = %dir.display(), "failed to dump timescope profile");
        }
    }
}

fn rotate_latest_profile_dir() -> PathBuf {
    let log_dir = crate::shoji_log_dir();
    let latest = log_dir.join("timescope-latest");
    if !latest.exists() {
        return latest;
    }

    let archived = log_dir.join(format!("timescope-{}", timestamp_millis()));
    if let Err(error) = fs::rename(&latest, &archived) {
        warn!(
            ?error,
            latest = %latest.display(),
            archived = %archived.display(),
            "failed to rotate previous timescope profile"
        );
        if latest.is_dir() {
            if let Err(error) = fs::remove_dir_all(&latest) {
                warn!(
                    ?error,
                    path = %latest.display(),
                    "failed to remove stale timescope profile directory"
                );
            }
        } else if let Err(error) = fs::remove_file(&latest) {
            warn!(
                ?error,
                path = %latest.display(),
                "failed to remove stale timescope profile file"
            );
        }
    }

    latest
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
