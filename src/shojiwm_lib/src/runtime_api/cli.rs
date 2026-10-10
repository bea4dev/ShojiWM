//! Command line shared by every ShojiWM binary, whatever its config language.
//!
//! Each option also has an environment variable so session wrappers (Nix,
//! display managers) can set defaults; the command line wins.

use std::{collections::BTreeMap, path::PathBuf};

/// An option a runtime adds on top of the common ones (`--name <value>`).
#[derive(Debug, Clone, Copy)]
pub struct ArgSpec {
    /// Option name without dashes, e.g. `"decoration-runtime"`.
    pub name: &'static str,
    /// Environment variable consulted when the option is absent.
    pub env: Option<&'static str>,
    pub value_name: &'static str,
    pub help: &'static str,
}

#[derive(Debug, Clone, Default)]
pub struct CommonArgs {
    /// `--tty`: run on a DRM/KMS session instead of nested in another compositor.
    pub tty: bool,
    pub log_off: bool,
    pub no_log_rotate: bool,
    pub tty_outputs: Vec<String>,
    pub xwayland_satellite_path: Option<String>,
    pub xwayland_satellite_glamor: Option<String>,
    pub dev: bool,
    pub config_path: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub extra: BTreeMap<String, String>,
    pub help: bool,
    pub version: bool,
    /// `-q/--quit` or `-r/--reload`: tell the running compositor to do that
    /// instead of starting one.
    pub control_command: Option<crate::control_socket::Command>,
}

impl CommonArgs {
    pub fn from_env(extra_args: &[ArgSpec]) -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        Self::parse(&args, extra_args)
    }

    pub fn parse(args: &[String], extra_args: &[ArgSpec]) -> Self {
        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let env_flag_off = |name: &str| {
            std::env::var_os(name).is_some_and(|value| value == "off" || value == "0")
        };
        let flag = |name: &str| args.iter().any(|arg| arg == name);

        let extra = extra_args
            .iter()
            .filter_map(|spec| {
                parse_option_value(args, &format!("--{}", spec.name))
                    .or_else(|| spec.env.and_then(env))
                    .map(|value| (spec.name.to_string(), value))
            })
            .collect();

        Self {
            tty: flag("--tty"),
            log_off: flag("--log-off") || env_flag_off("SHOJI_LOG"),
            no_log_rotate: flag("--no-log-rotate") || env_flag_off("SHOJI_LOG_ROTATE"),
            tty_outputs: parse_tty_outputs(args),
            xwayland_satellite_path: parse_option_value(args, "--xwayland-satellite-path")
                .or_else(|| env("SHOJI_XWAYLAND_SATELLITE_PATH")),
            xwayland_satellite_glamor: parse_option_value(args, "--xwayland-satellite-glamor")
                .or_else(|| env("SHOJI_XWAYLAND_SATELLITE_GLAMOR"))
                .filter(|value| matches!(value.as_str(), "gl" | "es" | "none")),
            dev: flag("--dev"),
            config_path: parse_option_value(args, "--config")
                .or_else(|| env("SHOJI_CONFIG"))
                .map(PathBuf::from),
            runtime_dir: parse_option_value(args, "--runtime-dir")
                .or_else(|| env("SHOJI_RUNTIME_DIR"))
                .map(PathBuf::from),
            extra,
            help: flag("--help") || flag("-h"),
            version: flag("--version") || flag("-V"),
            control_command: if flag("--quit") || flag("-q") {
                Some(crate::control_socket::Command::Quit)
            } else if flag("--reload") || flag("-r") {
                Some(crate::control_socket::Command::Reload)
            } else {
                None
            },
        }
    }
}

const COMMON_HELP: &[(&str, &str)] = &[
    ("--tty", "Run on the console (DRM/KMS) instead of nested in another compositor"),
    ("--tty-output <NAME,...>", "Only drive these connectors [SHOJI_TTY_OUTPUT]"),
    ("--config <PATH>", "Config entry point [SHOJI_CONFIG]"),
    ("--runtime-dir <DIR>", "Directory with the config runtime's files [SHOJI_RUNTIME_DIR]"),
    ("--dev", "Run from a source checkout"),
    ("--log-off", "Disable the session log [SHOJI_LOG=off]"),
    ("--no-log-rotate", "Overwrite latest.log instead of rotating it [SHOJI_LOG_ROTATE=off]"),
    ("--xwayland-satellite-path <PATH>", "External xwayland-satellite binary [SHOJI_XWAYLAND_SATELLITE_PATH]"),
    ("--xwayland-satellite-glamor <gl|es|none>", "Glamor mode for Xwayland [SHOJI_XWAYLAND_SATELLITE_GLAMOR]"),
    ("-q, --quit", "Quit the running ShojiWM (same as Super+Shift+Q)"),
    ("-r, --reload", "Reload the running ShojiWM's config (same as Super+Shift+R)"),
    ("-h, --help", "Print this help"),
    ("-V, --version", "Print the version"),
];

pub fn help_text(binary: &str, runtime_name: &str, extra_args: &[ArgSpec]) -> String {
    let mut extra_rows = Vec::new();
    for spec in extra_args {
        let mut help = spec.help.to_string();
        if let Some(env) = spec.env {
            help.push_str(&format!(" [{env}]"));
        }
        extra_rows.push((format!("--{} <{}>", spec.name, spec.value_name), help));
    }
    let width = COMMON_HELP
        .iter()
        .map(|(option, _)| option.len())
        .chain(extra_rows.iter().map(|(option, _)| option.len()))
        .max()
        .unwrap_or(0);

    let mut text = format!("ShojiWM ({runtime_name} config runtime)\n\nUsage: {binary} [OPTIONS]\n\nOptions:\n");
    for (option, help) in COMMON_HELP {
        text.push_str(&format!("  {option:width$}  {help}\n"));
    }
    if !extra_rows.is_empty() {
        text.push_str(&format!("\n{runtime_name} runtime options:\n"));
        for (option, help) in &extra_rows {
            text.push_str(&format!("  {option:width$}  {help}\n"));
        }
    }
    text
}

fn parse_tty_outputs(args: &[String]) -> Vec<String> {
    let mut outputs = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix("--tty-output=") {
            outputs.extend(split_tty_outputs(value));
        } else if arg == "--tty-output"
            && let Some(value) = args.get(index + 1)
        {
            outputs.extend(split_tty_outputs(value));
            index += 1;
        }
        index += 1;
    }
    outputs
}

fn split_tty_outputs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_option_value(args: &[String], option: &str) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(value) = arg.strip_prefix(&format!("{option}=")) {
            if !value.is_empty() {
                return Some(value.to_string());
            }
        } else if arg == option
            && let Some(value) = args.get(index + 1).filter(|value| !value.is_empty())
        {
            return Some(value.clone());
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_common_and_extra_options() {
        const EXTRA: &[ArgSpec] = &[ArgSpec {
            name: "decoration-runtime",
            env: None,
            value_name: "PATH",
            help: "",
        }];
        let parsed = CommonArgs::parse(
            &args(&[
                "--tty",
                "--tty-output=DP-1, eDP-1",
                "--config",
                "/tmp/index.tsx",
                "--decoration-runtime=/tmp/runtime.ts",
            ]),
            EXTRA,
        );
        assert!(parsed.tty);
        assert_eq!(parsed.tty_outputs, ["DP-1", "eDP-1"]);
        assert_eq!(parsed.config_path, Some(PathBuf::from("/tmp/index.tsx")));
        assert_eq!(
            parsed.extra.get("decoration-runtime").map(String::as_str),
            Some("/tmp/runtime.ts")
        );
    }
}
