//! The boundary between the compositor and a config runtime.
//!
//! ShojiWM does not know which language the user's config is written in. A
//! [`RuntimeLauncher`] turns the common command line into a running
//! [`ConfigRuntime`]; the compositor then talks to it with messages:
//!
//! ```text
//! compositor ── request(RuntimeRequest) ──▶ runtime   (answered: RuntimeReply)
//! compositor ── post(RuntimeEvent) ───────▶ runtime   (fire and forget)
//! compositor ◀── RuntimeHost::send(HostMessage) ── runtime (side effects, any thread)
//! ```
//!
//! The TypeScript runtime that ships with ShojiWM (the `shoji_wm` crate) is
//! one implementation; a runtime for another language implements the same two
//! traits in its own crate and calls [`crate::run`] with its launcher.

pub mod cli;
mod handle;
mod host;
mod message;

use std::{collections::BTreeMap, ffi::OsStr, path::PathBuf};

pub use handle::RuntimeHandle;
pub use host::{HostMessage, RuntimeConfigDelta, RuntimeHost, RuntimeSchedule};
pub use message::{
    CompositionPatch, DecorationRequest, OVERLAY_STAGE_INDEX, PAINT_STAGE_INDEX, SHADER_INPUT_STAGE_INDEX, EffectRequest, InputRequest, RuntimeEvent, RuntimeReply, RuntimeRequest,
    SchedulerTickRequest, WindowRequest, WorkspaceRequest,
};

/// Error type shared by every runtime.
pub type RuntimeError = crate::ssd::DecorationEvaluationError;

/// A running config runtime.
///
/// All methods are called from the compositor thread. A runtime that lives on
/// its own thread (TypeScript, .NET) forwards the message there and waits for
/// the answer; a Rust runtime handles it in place. Requests the runtime does
/// not care about should be answered with [`RuntimeReply::Unhandled`], which
/// makes the compositor fall back to its built-in behavior.
pub trait ConfigRuntime {
    /// Load the config ahead of the first request. Called once, early in
    /// startup; an error is shown as a config error and the compositor keeps
    /// running with its built-in defaults.
    fn preload(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Run the config's startup hooks once outputs and input devices are
    /// known. Config deltas (key bindings, outputs, ...) go through the host.
    fn enable(&mut self) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// First half of a hot reload, called when the user asks for one
    /// (`Super+Shift+R`). Must not block: a runtime whose config needs a slow
    /// build (compiling C#, a Rust dylib, ...) starts it in the background,
    /// answers [`ReloadPreparation::Pending`] and keeps serving the current
    /// config until it sends [`HostMessage::ReloadReady`]. The compositor
    /// then calls [`reload`](Self::reload) at a quiet point of its loop.
    ///
    /// The default answers [`ReloadPreparation::Ready`], which makes the
    /// compositor call `reload` right away. An error is shown as a hot
    /// reload error and the current config stays.
    fn prepare_reload(&mut self) -> Result<ReloadPreparation, RuntimeError> {
        Ok(ReloadPreparation::Ready)
    }

    /// Second half of a hot reload: replace the loaded config with the fresh
    /// one, carrying over whatever state the runtime persists across reloads.
    /// Runs on the compositor thread, so the slow part belongs in
    /// [`prepare_reload`](Self::prepare_reload). On success the compositor
    /// drops every cached evaluation and asks again; on error the runtime
    /// should keep (or fall back to) the config it had.
    fn reload(&mut self) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unsupported("hot reload"))
    }

    fn request(
        &mut self,
        now_ms: f64,
        request: RuntimeRequest<'_>,
    ) -> Result<RuntimeReply, RuntimeError>;

    fn post(&mut self, _now_ms: f64, _event: RuntimeEvent) {}

    /// The compositor is exiting.
    fn shutdown(&mut self) {}
}

/// Answer of [`ConfigRuntime::prepare_reload`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadPreparation {
    /// The new config can be swapped in now; the compositor calls
    /// [`ConfigRuntime::reload`] immediately.
    Ready,
    /// The new config is being prepared in the background. The runtime sends
    /// [`HostMessage::ReloadReady`] once it is done (or failed).
    Pending,
}

/// Starts a [`ConfigRuntime`]; one per config language.
pub trait RuntimeLauncher {
    /// Short identifier, e.g. `"typescript"`. Shown in logs and `--help`.
    fn name(&self) -> &'static str;

    /// Config entry point used when neither `--config` nor `SHOJI_CONFIG` is
    /// given. `None` for runtimes whose config is compiled in.
    fn default_config_path(&self, _dev: bool) -> Option<PathBuf> {
        None
    }

    /// Options specific to this runtime, accepted in addition to the common
    /// ones. Parsed values arrive in [`LaunchContext::extra`].
    fn extra_args(&self) -> &'static [cli::ArgSpec] {
        &[]
    }

    /// Construct the runtime. Must not block on loading the config; that
    /// happens in [`ConfigRuntime::preload`].
    fn launch(&self, context: LaunchContext) -> Box<dyn ConfigRuntime>;
}

/// Everything a launcher gets to build its runtime from.
#[derive(Debug, Clone)]
pub struct LaunchContext {
    /// `--config` / `SHOJI_CONFIG`, else the launcher's default.
    pub config_path: Option<PathBuf>,
    /// `--runtime-dir` / `SHOJI_RUNTIME_DIR`: where the runtime's own files live.
    pub runtime_dir: Option<PathBuf>,
    /// `--dev`: run from a source checkout.
    pub dev: bool,
    /// Values of [`RuntimeLauncher::extra_args`], keyed by option name
    /// without the leading dashes.
    pub extra: BTreeMap<String, String>,
    pub host: RuntimeHost,
    /// The compositor's own Wayland socket name (`wayland-1`), when it has one.
    /// Use it rather than `WAYLAND_DISPLAY`: the config is loaded before the
    /// compositor points `WAYLAND_DISPLAY` at itself, and nested in another
    /// compositor that variable names the parent until then.
    pub wayland_display: Option<String>,
}

/// Runtime used when no config runtime is available: every request falls back
/// to the compositor's built-in behavior.
#[derive(Debug, Default)]
pub struct NullRuntime;

impl ConfigRuntime for NullRuntime {
    fn request(
        &mut self,
        _now_ms: f64,
        _request: RuntimeRequest<'_>,
    ) -> Result<RuntimeReply, RuntimeError> {
        Ok(RuntimeReply::Unhandled)
    }
}

/// A launcher together with the parsed command line, carried from
/// [`crate::run`] to where the compositor state is built.
pub struct RuntimeBoot {
    launcher: Box<dyn RuntimeLauncher>,
    config_path: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
    dev: bool,
    extra: BTreeMap<String, String>,
}

impl RuntimeBoot {
    pub fn new(launcher: Box<dyn RuntimeLauncher>, args: &cli::CommonArgs) -> Self {
        Self {
            launcher,
            config_path: args.config_path.clone(),
            runtime_dir: args.runtime_dir.clone(),
            dev: args.dev,
            extra: args.extra.clone(),
        }
    }

    pub fn launch(&self, host: RuntimeHost) -> RuntimeHandle {
        self.launch_with_display(host, None)
    }

    /// [`launch`](Self::launch) for a compositor listening on `wayland_display`.
    pub fn launch_with_display(&self, host: RuntimeHost, wayland_display: Option<&OsStr>) -> RuntimeHandle {
        let context = LaunchContext {
            config_path: self
                .config_path
                .clone()
                .or_else(|| self.launcher.default_config_path(self.dev)),
            runtime_dir: self.runtime_dir.clone(),
            dev: self.dev,
            extra: self.extra.clone(),
            host: host.clone(),
            wayland_display: wayland_display.map(|display| display.to_string_lossy().into_owned()),
        };
        RuntimeHandle::new(self.launcher.name(), self.launcher.launch(context), host)
    }
}
