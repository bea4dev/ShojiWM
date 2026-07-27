use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use tracing::warn;

#[derive(Debug, Clone, Default)]
pub struct RuntimePathOptions {
    pub dev: bool,
    pub config_path: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub decoration_runtime: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePathMode {
    Development,
    Installed,
}

#[derive(Debug, Clone)]
pub struct DecorationRuntimePaths {
    pub mode: RuntimePathMode,
    pub working_dir: PathBuf,
    pub script_path: PathBuf,
    pub config_path: PathBuf,
}

static RUNTIME_PATH_OPTIONS: OnceLock<RuntimePathOptions> = OnceLock::new();

pub fn init_runtime_path_options(options: RuntimePathOptions) {
    if RUNTIME_PATH_OPTIONS.set(options).is_err() {
        warn!("runtime path options were already initialized");
    }
}

pub fn decoration_runtime_paths() -> DecorationRuntimePaths {
    let options = RUNTIME_PATH_OPTIONS.get().cloned().unwrap_or_default();
    if options.dev {
        development_paths(&options)
    } else {
        installed_paths(&options)
    }
}

fn development_paths(options: &RuntimePathOptions) -> DecorationRuntimePaths {
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let runtime_dir = options
        .runtime_dir
        .clone()
        .unwrap_or_else(|| repo_root.clone());
    DecorationRuntimePaths {
        mode: RuntimePathMode::Development,
        working_dir: runtime_dir.clone(),
        script_path: options
            .decoration_runtime
            .clone()
            .or_else(|| std::env::var_os("SHOJI_DECORATION_RUNTIME").map(PathBuf::from))
            .unwrap_or_else(|| runtime_dir.join("tools/decoration-runtime.ts")),
        config_path: options
            .config_path
            .clone()
            .or_else(|| std::env::var_os("SHOJI_CONFIG").map(PathBuf::from))
            .unwrap_or_else(|| repo_root.join("packages/config/src/index.tsx")),
    }
}

fn installed_paths(options: &RuntimePathOptions) -> DecorationRuntimePaths {
    let runtime_dir = options
        .runtime_dir
        .clone()
        .or_else(|| std::env::var_os("SHOJI_RUNTIME_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/usr/lib/shojiwm"));
    let config_path = options
        .config_path
        .clone()
        .or_else(|| std::env::var_os("SHOJI_CONFIG").map(PathBuf::from))
        .unwrap_or_else(default_installed_config_path);
    let working_dir = config_project_dir(&config_path);

    DecorationRuntimePaths {
        mode: RuntimePathMode::Installed,
        working_dir,
        script_path: options
            .decoration_runtime
            .clone()
            .or_else(|| std::env::var_os("SHOJI_DECORATION_RUNTIME").map(PathBuf::from))
            .unwrap_or_else(|| runtime_dir.join("tools/decoration-runtime.ts")),
        config_path,
    }
}

fn config_project_dir(config_path: &Path) -> PathBuf {
    let start = config_path.parent().unwrap_or_else(|| Path::new("."));
    for dir in start.ancestors() {
        if dir.join("tsconfig.json").exists() || dir.join("package.json").exists() {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

fn default_installed_config_path() -> PathBuf {
    let home = home_dir();
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));

    let preferred = xdg_config_home
        .as_ref()
        .map(|dir| dir.join("shojiwm/src/index.tsx"));
    let legacy = home
        .as_ref()
        .map(|home| home.join("shoji_wm/config/src/index.tsx"));

    for candidate in [preferred.as_deref(), legacy.as_deref()]
        .into_iter()
        .flatten()
    {
        if candidate.exists() {
            return candidate.to_path_buf();
        }
    }

    preferred.unwrap_or_else(|| Path::new("shojiwm/src/index.tsx").to_path_buf())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
