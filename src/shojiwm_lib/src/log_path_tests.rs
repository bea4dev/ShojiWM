use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use super::resolve_log_dir;

#[test]
fn logging_creates_file_in_selected_directory() {
    if let Some(expected) = std::env::var_os("SHOJI_LOG_PATH_TEST_EXPECTED") {
        let expected = PathBuf::from(expected);
        let selected = super::shoji_log_dir();
        let selected = if selected.is_absolute() {
            selected
        } else {
            std::env::current_dir().unwrap().join(selected)
        };
        assert_eq!(selected, expected);
        let mode = std::env::var("SHOJI_LOG_PATH_TEST_MODE").unwrap();
        let args = super::CommonArgs {
            no_log_rotate: mode == "overwrite",
            ..Default::default()
        };
        let guard = super::init_logging(&args).unwrap();
        tracing::info!(target: "shojiwm_lib", "session-log-path-proof");
        if mode == "standard" {
            super::profiler::init();
            super::profiler::set_enabled(true);
            {
                timescope::scope!("log_path_profile_probe");
                std::hint::black_box(42);
            }
            super::profiler::set_enabled(false);
            let profile = expected.join("timescope-latest");
            assert!(
                fs::read_to_string(profile.join("summary.txt"))
                    .unwrap()
                    .contains("log_path_profile_probe")
            );
            let report = fs::read_to_string(profile.join("report.html")).unwrap();
            assert!(report.contains("ShojiWM Timescope Profile Report"));
            assert!(report.contains("log_path_profile_probe"));
        }
        drop(guard);
        let log = fs::read_to_string(expected.join("latest.log")).unwrap();
        assert!(log.contains("session-log-path-proof"));
        assert!(!log.contains("previous-session"));
        return;
    }

    let root = test_root("paths");
    for name in [
        "xdg",
        "fallback",
        "empty-xdg",
        "relative-xdg",
        "empty-override",
        "absolute-override",
        "relative-override",
        "legacy-override",
        "no-home",
    ] {
        let case = root.join(name);
        fs::create_dir(&case).unwrap();
        let state = case.join("state");
        let legacy = case.join("home/shoji_wm/logs");
        fs::create_dir_all(legacy.join("timescope-latest")).unwrap();
        fs::write(legacy.join("latest.log"), "legacy-session").unwrap();
        fs::write(legacy.join("1.log"), "legacy-archive").unwrap();
        fs::write(legacy.join("timescope-latest/sentinel"), "legacy-profile").unwrap();
        let expected = match name {
            "fallback" | "empty-xdg" | "relative-xdg" => {
                case.join("home/.local/state/shoji_wm/logs")
            }
            "absolute-override" => case.join("explicit"),
            "relative-override" => case.join("relative-logs"),
            "legacy-override" => legacy.clone(),
            "no-home" => case.join(".local/state/shoji_wm/logs"),
            _ => state.join("shoji_wm/logs"),
        };
        if name != "legacy-override" {
            fs::create_dir_all(expected.join("timescope-latest")).unwrap();
            fs::write(
                expected.join("timescope-latest/sentinel"),
                "selected-profile",
            )
            .unwrap();
        }
        let mut command = logging_child(&case, &expected, "standard");
        match name {
            "fallback" => {
                command.env_remove("XDG_STATE_HOME");
            }
            "empty-xdg" => {
                command.env("XDG_STATE_HOME", "");
            }
            "relative-xdg" => {
                command.env("XDG_STATE_HOME", "relative-state");
            }
            "no-home" => {
                command.env_remove("XDG_STATE_HOME").env_remove("HOME");
            }
            _ => {
                command.env("XDG_STATE_HOME", &state);
            }
        }
        match name {
            "empty-override" => {
                command.env("SHOJI_LOG_DIR", "");
            }
            "absolute-override" | "legacy-override" => {
                command.env("SHOJI_LOG_DIR", &expected);
            }
            "relative-override" => {
                command.env("SHOJI_LOG_DIR", "relative-logs");
            }
            _ => {}
        }
        assert!(command.status().unwrap().success(), "scenario {name}");
        let archived_profiles: Vec<_> = fs::read_dir(&expected)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("timescope-")
                    && path.file_name().unwrap() != "timescope-latest"
            })
            .collect();
        assert_eq!(archived_profiles.len(), 1, "scenario {name}");
        assert_eq!(
            fs::read_to_string(archived_profiles[0].join("sentinel")).unwrap(),
            if name == "legacy-override" {
                "legacy-profile"
            } else {
                "selected-profile"
            }
        );
        assert_eq!(
            fs::read_to_string(legacy.join("1.log")).unwrap(),
            "legacy-archive"
        );
        if name != "legacy-override" {
            assert_eq!(
                fs::read_to_string(legacy.join("latest.log")).unwrap(),
                "legacy-session"
            );
            assert_eq!(
                fs::read_to_string(legacy.join("timescope-latest/sentinel")).unwrap(),
                "legacy-profile"
            );
        } else {
            assert!(
                numeric_logs(&legacy)
                    .iter()
                    .any(|path| fs::read_to_string(path).unwrap() == "legacy-session")
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}

fn test_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "shoji-log-{name}-{}-{}",
        std::process::id(),
        super::startup_timestamp_millis()
    ));
    fs::create_dir(&root).unwrap();
    root
}

fn logging_child(root: &Path, expected: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "log_path_tests::logging_creates_file_in_selected_directory",
            "--nocapture",
        ])
        .current_dir(root)
        .env("SHOJI_LOG_PATH_TEST_EXPECTED", expected)
        .env("SHOJI_LOG_PATH_TEST_MODE", mode)
        .env("HOME", root.join("home"))
        .env("RUST_LOG", "shojiwm_lib=info")
        .env("SHOJI_LOG_KEEP_DAYS", "off")
        .env("SHOJI_LOG_KEEP_BYTES", "off")
        .env("SHOJI_LOG_RATE_LIMIT", "0")
        .env_remove("XDG_STATE_HOME")
        .env_remove("SHOJI_LOG_DIR");
    command
}

fn numeric_logs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.parse::<u128>().is_ok())
                && path.extension().is_some_and(|extension| extension == "log")
        })
        .collect()
}

#[test]
fn selected_directory_preserves_rotation_and_retention() {
    let root = test_root("policies");
    for mode in ["rotate", "overwrite", "retention", "age-retention"] {
        let checks_retention = mode == "retention" || mode == "age-retention";
        let case = root.join(mode);
        let selected = case.join("selected");
        fs::create_dir_all(&selected).unwrap();
        fs::write(selected.join("latest.log"), "previous-session").unwrap();
        fs::write(selected.join("notes.txt"), "unrelated-file").unwrap();
        if checks_retention {
            for stamp in 1..=7 {
                fs::write(selected.join(format!("{stamp}.log")), "rotated-log").unwrap();
            }
        }
        let legacy = case.join("home/shoji_wm/logs");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("1.log"), "legacy-retained").unwrap();
        let mut command = logging_child(&case, &selected, mode);
        command.env("SHOJI_LOG_DIR", &selected);
        if mode == "retention" {
            command.env("SHOJI_LOG_KEEP_BYTES", "1");
        } else if mode == "age-retention" {
            command.env("SHOJI_LOG_KEEP_DAYS", "1");
        }
        assert!(command.status().unwrap().success(), "scenario {mode}");
        let rotated = numeric_logs(&selected);
        assert_eq!(
            fs::read_to_string(selected.join("notes.txt")).unwrap(),
            "unrelated-file"
        );
        assert_eq!(
            fs::read_to_string(legacy.join("1.log")).unwrap(),
            "legacy-retained"
        );
        if mode == "overwrite" {
            assert!(rotated.is_empty());
        } else {
            assert!(
                rotated
                    .iter()
                    .any(|path| fs::read_to_string(path).unwrap() == "previous-session")
            );
            assert_eq!(rotated.len(), if checks_retention { 5 } else { 1 });
            if checks_retention {
                for stamp in 1..=3 {
                    assert!(!selected.join(format!("{stamp}.log")).exists());
                }
                for stamp in 4..=7 {
                    assert_eq!(
                        fs::read_to_string(selected.join(format!("{stamp}.log"))).unwrap(),
                        "rotated-log"
                    );
                }
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_logs_follow_xdg_and_explicit_override() {
    let cases = [
        (
            None,
            None,
            Some("/home/test"),
            "/home/test/.local/state/shoji_wm/logs",
        ),
        (
            None,
            Some("/state"),
            Some("/home/test"),
            "/state/shoji_wm/logs",
        ),
        (Some("/logs"), Some("/state"), Some("/home/test"), "/logs"),
        (
            Some(""),
            Some("/state"),
            Some("/home/test"),
            "/state/shoji_wm/logs",
        ),
        (
            None,
            Some(""),
            Some("/home/test"),
            "/home/test/.local/state/shoji_wm/logs",
        ),
        (
            None,
            Some("relative"),
            Some("/home/test"),
            "/home/test/.local/state/shoji_wm/logs",
        ),
        (
            Some("relative-logs"),
            None,
            Some("/home/test"),
            "relative-logs",
        ),
        (None, None, None, "./.local/state/shoji_wm/logs"),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|(override_dir, state_home, home, _)| {
            resolve_log_dir(
                override_dir.map(OsString::from),
                state_home.map(OsString::from),
                home.map(OsString::from),
            )
        })
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|(_, _, _, expected)| PathBuf::from(expected))
        .collect();
    assert_eq!(actual, expected);
}
