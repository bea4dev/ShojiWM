//! `COMPOSITOR`: the Rust counterpart of the TypeScript `COMPOSITOR` global.
//!
//! ```no_run
//! use shojiwm_rs::prelude::*;
//!
//! COMPOSITOR.key.bind("terminal", "Super+T", || {
//!     COMPOSITOR.process.spawn(Command::exec(["kitty"]));
//! });
//! COMPOSITOR.event.on_focus(|window, focused| {
//!     tracing::info!(?window, focused, "focus changed");
//! });
//! ```
//!
//! Every controller is a zero-sized handle onto the running config's state,
//! so the API can be used from anywhere on the compositor thread, including
//! inside callbacks.

use std::{
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{Arc, Mutex},
};

use shojiwm_lib::{
    activation_environment::RuntimeEnvOperation,
    config::RuntimeOutputConfig,
    cursor::RuntimeCursorConfigUpdate,
    keyboard_layout::KeyboardLayoutSnapshot,
    output_power::RuntimeOutputPowerRequest,
    runtime_api::HostMessage,
    runtime_input::{RuntimeInputConfig, RuntimeInputDeviceSnapshot},
    runtime_key_binding::{RuntimeKeyBindingEntry, RuntimeKeyBindingPhase},
    runtime_process::{
        RuntimeProcessAction, RuntimeProcessEntry, RuntimeProcessLaunch,
        RuntimeProcessReloadPolicy, RuntimeProcessRestartPolicy, RuntimeProcessRunPolicy,
    },
    runtime_workspace::{RuntimeWorkspaceActivateRequestSnapshot, RuntimeWorkspaceConfigUpdate},
    ssd::{
        GestureSwipeEventSnapshot,
        window_model::{LayerEdgeSnapshot, LayerExclusiveZoneSnapshot},
        PointerMoveEventSnapshot, PopupParentKindSnapshot, SurfacePolicy, WaylandLayerSnapshot,
        WaylandOutputSnapshot, WaylandPopupSnapshot, WaylandWindowSnapshot,
        WindowActivateRequestEventSnapshot, WindowDecorationModeSnapshot,
        WindowDecorationPolicyContextSnapshot, WindowFullscreenRequestEventSnapshot,
        WindowMaximizeRequestEventSnapshot, WindowMinimizeRequestEventSnapshot,
        WindowMoveEventSnapshot, WindowResizeEventSnapshot,
    },
};

pub use shojiwm_lib::frame_pacing::FramePacing;

use crate::{
    effect::{Effect, SurfaceEffects},
    input_grab::{InputGrab, InputGrabOptions},
    output_composition::OutputStack,
    overlay::{Overlay, OverlayHandle},
    reactive::{ReadSignal, untrack},
    runtime::{self, Listeners, with_registry},
    view::{Composition, Rect},
    window::Window,
};

/// The running compositor, as seen by the config.
pub struct Compositor {
    pub env: EnvController,
    pub cursor: CursorController,
    pub process: ProcessController,
    pub key: KeyController,
    pub pointer: PointerController,
    pub input: InputController,
    pub output: OutputController,
    pub workspace: WorkspaceController,
    pub layer: LayerController,
    pub window: WindowController,
    pub effect: EffectController,
    pub rendering: RenderingController,
    pub event: EventController,
    pub debug: DebugController,
}

/// The one compositor. Use it like the TypeScript global.
pub const COMPOSITOR: Compositor = Compositor {
    env: EnvController,
    cursor: CursorController,
    process: ProcessController,
    key: KeyController,
    pointer: PointerController,
    input: InputController,
    output: OutputController,
    workspace: WorkspaceController,
    layer: LayerController,
    window: WindowController,
    effect: EffectController,
    rendering: RenderingController,
    event: EventController,
    debug: DebugController,
};

/// Passed to [`Compositor::on_enable`] listeners.
#[derive(Debug, Clone)]
pub struct EnableEvent {
    /// `"initial"` for the first start.
    pub reason: String,
}

/// Passed to [`Compositor::on_disable`] listeners.
#[derive(Debug, Clone)]
pub struct DisableEvent {
    /// `"shutdown"` when the compositor exits.
    pub reason: String,
}

impl Compositor {
    /// Run once outputs and input devices are known.
    pub fn on_enable(&self, listener: impl Fn(&EnableEvent) + 'static) {
        with_registry(|registry| registry.listeners.enable.push(Rc::new(listener)));
    }

    pub fn on_disable(&self, listener: impl Fn(&DisableEvent) + 'static) {
        with_registry(|registry| registry.listeners.disable.push(Rc::new(listener)));
    }

    /// A [`Sender`] usable from any thread; every value is handed to
    /// `handler` on the compositor thread, which is woken for it. Use it to
    /// feed IPC servers or worker threads into the config.
    pub fn channel<T: Send + 'static>(&self, handler: impl Fn(T) + 'static) -> Sender<T> {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let sender = Sender {
            queue: queue.clone(),
            host: runtime::host(),
        };
        with_registry(|registry| {
            registry.channels.push(Rc::new(move || {
                let items: Vec<T> = match queue.lock() {
                    Ok(mut queue) => queue.drain(..).collect(),
                    Err(_) => Vec::new(),
                };
                let any = !items.is_empty();
                for item in items {
                    handler(item);
                }
                any
            }));
        });
        sender
    }
}

/// The sending half of [`Compositor::channel`].
pub struct Sender<T> {
    queue: Arc<Mutex<VecDeque<T>>>,
    host: Option<shojiwm_lib::runtime_api::RuntimeHost>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
            host: self.host.clone(),
        }
    }
}

impl<T> Sender<T> {
    pub fn send(&self, value: T) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(value);
        }
        if let Some(host) = &self.host {
            host.wake();
        }
    }
}

/// `COMPOSITOR.env`: environment of processes the compositor starts.
pub struct EnvController;

impl EnvController {
    pub fn set(&self, key: &str, value: impl Into<String>) {
        let value = value.into();
        with_registry(|registry| {
            registry.env.insert(key.to_owned(), value.clone());
            registry.pending.env_operations.push(RuntimeEnvOperation {
                key: key.to_owned(),
                value: Some(value),
            });
        });
    }

    pub fn unset(&self, key: &str) {
        with_registry(|registry| {
            registry.env.remove(key);
            registry.pending.env_operations.push(RuntimeEnvOperation {
                key: key.to_owned(),
                value: None,
            });
        });
    }

    pub fn get(&self, key: &str) -> Option<String> {
        with_registry(|registry| registry.env.get(key).cloned()).or_else(|| std::env::var(key).ok())
    }

    pub fn apply<K: AsRef<str>, V: Into<String>>(&self, values: impl IntoIterator<Item = (K, V)>) {
        for (key, value) in values {
            self.set(key.as_ref(), value);
        }
    }

    /// Export the variables set so far to the session (D-Bus / systemd
    /// activation environment).
    pub fn publish(&self) {
        with_registry(|registry| {
            let keys: Vec<String> = registry.env.keys().cloned().collect();
            registry.pending.env_publish.extend(keys);
        });
    }

    pub fn publish_keys(&self, keys: &[&str]) {
        with_registry(|registry| {
            registry
                .pending
                .env_publish
                .extend(keys.iter().map(|key| (*key).to_owned()));
        });
    }
}

/// `COMPOSITOR.cursor`.
pub struct CursorController;

impl CursorController {
    pub fn configure(&self, theme: &str, size: u32) {
        with_registry(|registry| {
            registry.pending.cursor = Some(RuntimeCursorConfigUpdate {
                theme: theme.to_owned(),
                size,
                reload: false,
            });
        });
    }
}

/// How to start a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    launch: RuntimeProcessLaunch,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
}

impl Command {
    /// A command line run by `/bin/sh -lc` (pipes, `~`, `$VAR` work).
    pub fn shell(command: impl Into<String>) -> Self {
        Self {
            launch: RuntimeProcessLaunch::Shell {
                command: command.into(),
            },
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    /// A program and its arguments, run without a shell.
    pub fn exec<S: Into<String>>(argv: impl IntoIterator<Item = S>) -> Self {
        Self {
            launch: RuntimeProcessLaunch::Command {
                command: argv.into_iter().map(Into::into).collect(),
            },
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    pub fn cwd(mut self, cwd: impl Into<String>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }
}

/// `COMPOSITOR.process`.
pub struct ProcessController;

impl ProcessController {
    fn register(&self, entry: RuntimeProcessEntry) {
        with_registry(|registry| {
            registry
                .processes
                .retain(|existing| existing.id() != entry.id());
            registry.processes.push(entry);
            registry.pending.processes = true;
        });
    }

    /// Run once per session (`process.once`).
    pub fn once(&self, id: &str, command: Command) {
        self.once_with(id, command, RuntimeProcessRunPolicy::OncePerSession);
    }

    pub fn once_with(&self, id: &str, command: Command, run_policy: RuntimeProcessRunPolicy) {
        self.register(RuntimeProcessEntry::Once {
            id: id.to_owned(),
            launch: command.launch,
            cwd: command.cwd,
            env: command.env,
            run_policy,
        });
    }

    /// Keep running, restarted per `restart` (`process.service`).
    pub fn service(&self, id: &str, command: Command, restart: RuntimeProcessRestartPolicy) {
        self.register(RuntimeProcessEntry::Service {
            id: id.to_owned(),
            launch: command.launch,
            cwd: command.cwd,
            env: command.env,
            restart,
            reload: RuntimeProcessReloadPolicy::KeepIfUnchanged,
        });
    }

    /// Start a process now (`process.spawn`).
    pub fn spawn(&self, command: Command) {
        with_registry(|registry| {
            registry.pending.process_actions.push(RuntimeProcessAction {
                launch: command.launch,
                cwd: command.cwd,
                env: command.env,
            });
        });
    }
}

/// `COMPOSITOR.key`.
pub struct KeyController;

impl KeyController {
    /// Bind `shortcut` (e.g. `"Super+Shift+Left"`, `"XF86AudioPlay"`) to
    /// `handler`, firing on press.
    pub fn bind(&self, id: &str, shortcut: &str, handler: impl Fn() + 'static) {
        self.bind_on(id, shortcut, RuntimeKeyBindingPhase::Press, handler);
    }

    /// Fire on release; required for modifier-only "tap" bindings such as
    /// `"Super"`.
    pub fn bind_release(&self, id: &str, shortcut: &str, handler: impl Fn() + 'static) {
        self.bind_on(id, shortcut, RuntimeKeyBindingPhase::Release, handler);
    }

    pub fn bind_on(
        &self,
        id: &str,
        shortcut: &str,
        phase: RuntimeKeyBindingPhase,
        handler: impl Fn() + 'static,
    ) {
        let entry = RuntimeKeyBindingEntry {
            id: id.to_owned(),
            shortcut: shortcut.to_owned(),
            on: phase,
        };
        if let Err(error) = entry.compile() {
            tracing::warn!(id, shortcut, %error, "ignoring invalid key binding");
            return;
        }
        with_registry(|registry| {
            registry.key_bindings.retain(|existing| existing.id != id);
            registry.key_bindings.push(entry);
            registry.key_handlers.insert(id.to_owned(), Rc::new(handler));
            registry.pending.key_bindings = true;
        });
    }

    pub fn unbind(&self, id: &str) {
        with_registry(|registry| {
            registry.key_bindings.retain(|existing| existing.id != id);
            registry.key_handlers.remove(id);
            registry.pending.key_bindings = true;
        });
    }
}

/// `COMPOSITOR.pointer`.
pub struct PointerController;

impl PointerController {
    /// Hold `modifier` (e.g. `"Super"`) and drag to move a window.
    pub fn bind_window_move_modifier(&self, modifier: &str) {
        with_registry(|registry| {
            registry.window_move_modifier = Some(modifier.to_owned());
            registry.pending.pointer = true;
        });
    }

    /// Hold `modifier` and right-drag to resize a window.
    pub fn bind_window_resize_modifier(&self, modifier: &str) {
        with_registry(|registry| {
            registry.window_resize_modifier = Some(modifier.to_owned());
            registry.pending.pointer = true;
        });
    }
}

/// Passed to [`EventController::on_input_device_change`].
#[derive(Debug, Clone)]
pub struct InputChangeEvent {
    pub devices: BTreeMap<String, RuntimeInputDeviceSnapshot>,
    pub added: Vec<RuntimeInputDeviceSnapshot>,
    pub removed: Vec<RuntimeInputDeviceSnapshot>,
}

/// `COMPOSITOR.input`.
pub struct InputController;

impl InputController {
    /// `configure(|input, devices| ...)`: fill in libinput / keyboard
    /// settings. Re-run whenever devices change.
    pub fn configure(
        &self,
        factory: impl Fn(&mut RuntimeInputConfig, &BTreeMap<String, RuntimeInputDeviceSnapshot>) + 'static,
    ) {
        with_registry(|registry| registry.input_configure = Some(Rc::new(factory)));
        reconfigure_input(true);
    }

    /// Connected input devices, by key.
    pub fn devices(&self) -> ReadSignal<BTreeMap<String, RuntimeInputDeviceSnapshot>> {
        runtime::global().inputs.read_only()
    }

    pub fn keyboard_layout(&self) -> ReadSignal<Option<KeyboardLayoutSnapshot>> {
        runtime::global().keyboard_layout.read_only()
    }

    /// Take all keyboard, pointer button, scroll and swipe input until
    /// [`InputGrab::release`]; see [`crate::input_grab`]. A new grab
    /// replaces (and cancels) the current one.
    pub fn grab(&self, options: InputGrabOptions) -> InputGrab {
        crate::input_grab::grab(options)
    }
}

pub(crate) fn reconfigure_input(force: bool) {
    let Some(factory) = with_registry(|registry| registry.input_configure.clone()) else {
        return;
    };
    let devices = untrack(|| runtime::global().inputs.get());
    let mut config = RuntimeInputConfig::default();
    untrack(|| factory(&mut config, &devices));
    with_registry(|registry| {
        if force || registry.desired_input.as_ref() != Some(&config) {
            registry.desired_input = Some(config.clone());
            registry.pending.input = Some(config);
        }
    });
}

/// What an output configure factory sees.
#[derive(Debug, Clone)]
pub struct OutputContext {
    /// Every output the compositor knows, enabled or not.
    pub connected: Vec<WaylandOutputSnapshot>,
    pub current: BTreeMap<String, WaylandOutputSnapshot>,
}

/// Passed to [`EventController::on_output_change`].
#[derive(Debug, Clone)]
pub struct OutputChangeEvent {
    pub current: BTreeMap<String, WaylandOutputSnapshot>,
    pub added: Vec<WaylandOutputSnapshot>,
    pub removed: Vec<WaylandOutputSnapshot>,
    pub changed: Vec<WaylandOutputSnapshot>,
}

/// One output's entry in a display configuration.
pub use shojiwm_lib::config::{
    RuntimeDisplayModePreference as OutputResolution, RuntimeOutputMode as OutputMode,
    RuntimeOutputPositionPreference as OutputPosition, RuntimeOutputTransform as OutputTransform,
};

/// Builder helpers for [`RuntimeOutputConfig`].
pub struct OutputConfig;

impl OutputConfig {
    /// `{ mode: "extend", resolution: "best", position: "auto", scale }`.
    pub fn extend(scale: f64) -> RuntimeOutputConfig {
        RuntimeOutputConfig {
            mode: Some(OutputMode::Extend),
            source: None,
            resolution: Some(OutputResolution::Best("best".into())),
            position: Some(OutputPosition::Auto("auto".into())),
            scale: Some(scale),
            transform: None,
            subpixel: None,
            hdr: None,
            hdr_max_luminance: None,
            hdr_min_luminance: None,
            hdr_sdr_luminance: None,
        }
    }

    /// `{ mode: "disabled" }`.
    pub fn disabled() -> RuntimeOutputConfig {
        RuntimeOutputConfig {
            mode: Some(OutputMode::Disabled),
            source: None,
            resolution: None,
            position: None,
            scale: None,
            transform: None,
            subpixel: None,
            hdr: None,
            hdr_max_luminance: None,
            hdr_min_luminance: None,
            hdr_sdr_luminance: None,
        }
    }
}

/// `COMPOSITOR.output`.
pub struct OutputController;

impl OutputController {
    /// `configure(|context| outputs)`: the display configuration, re-run
    /// whenever outputs change.
    pub fn configure(
        &self,
        factory: impl Fn(&OutputContext) -> BTreeMap<String, Option<RuntimeOutputConfig>> + 'static,
    ) {
        with_registry(|registry| registry.output_configure = Some(Rc::new(factory)));
        reconfigure_outputs(true);
    }

    pub fn reconfigure(&self) {
        reconfigure_outputs(true);
    }

    /// All outputs, by name (tracked).
    pub fn state(&self) -> ReadSignal<BTreeMap<String, WaylandOutputSnapshot>> {
        runtime::global().outputs.read_only()
    }

    pub fn get(&self, name: &str) -> Option<WaylandOutputSnapshot> {
        untrack(|| runtime::global().outputs.with(|outputs| outputs.get(name).cloned()))
    }

    /// Names of enabled outputs.
    pub fn list(&self) -> Vec<String> {
        untrack(|| {
            runtime::global().outputs.with(|outputs| {
                outputs
                    .values()
                    .filter(|output| output.enabled)
                    .map(|output| output.name.clone())
                    .collect()
            })
        })
    }

    /// The output's area in global logical coordinates.
    pub fn logical_rect(&self, name: &str) -> Option<Rect> {
        let output = self.get(name)?;
        output_logical_rect(&output)
    }

    /// `setPower(power, options)`: switch panels on or off (DPMS) without
    /// changing the layout. Only the TTY backend has panels to switch.
    pub fn set_power(&self, power: OutputPower, options: OutputPowerOptions) {
        runtime::send(HostMessage::OutputPower(RuntimeOutputPowerRequest {
            mode: power,
            output: options.output,
            wake_on_input: options.wake_on_input,
        }));
    }
}

pub use shojiwm_lib::output_power::OutputPowerMode as OutputPower;

/// Options of [`OutputController::set_power`].
#[derive(Debug, Clone, Default)]
pub struct OutputPowerOptions {
    /// Output name; every output when `None`.
    pub output: Option<String>,
    /// Only when switching off: switch back on at the next key press, click,
    /// pointer motion, scroll or touch.
    pub wake_on_input: bool,
}

pub(crate) fn output_logical_rect(output: &WaylandOutputSnapshot) -> Option<Rect> {
    let resolution = output.resolution?;
    let scale = if output.scale > 0.0 { output.scale } else { 1.0 };
    let (width, height) = match output.transform {
        shojiwm_lib::ssd::OutputTransformSnapshot::Rotate90
        | shojiwm_lib::ssd::OutputTransformSnapshot::Rotate270
        | shojiwm_lib::ssd::OutputTransformSnapshot::Flipped90
        | shojiwm_lib::ssd::OutputTransformSnapshot::Flipped270 => {
            (resolution.height, resolution.width)
        }
        _ => (resolution.width, resolution.height),
    };
    Some(Rect::new(
        output.position.x as f64,
        output.position.y as f64,
        width as f64 / scale,
        height as f64 / scale,
    ))
}

pub(crate) fn reconfigure_outputs(force: bool) {
    let Some(factory) = with_registry(|registry| registry.output_configure.clone()) else {
        return;
    };
    let current = untrack(|| runtime::global().outputs.get());
    if current.is_empty() {
        return;
    }
    let context = OutputContext {
        connected: current.values().cloned().collect(),
        current,
    };
    let desired = untrack(|| factory(&context));
    with_registry(|registry| {
        if force || registry.desired_outputs.as_ref() != Some(&desired) {
            registry.desired_outputs = Some(desired.clone());
            registry.pending.display = Some(desired);
        }
    });
}

/// `COMPOSITOR.workspace`: the `ext-workspace` model shown to bars.
pub struct WorkspaceController;

impl WorkspaceController {
    pub fn configure(&self, factory: impl Fn() -> RuntimeWorkspaceConfigUpdate + 'static) {
        with_registry(|registry| registry.workspace_configure = Some(Rc::new(factory)));
        self.reconfigure();
    }

    /// Re-run the factory and send the model if it changed.
    pub fn reconfigure(&self) {
        let Some(factory) = with_registry(|registry| registry.workspace_configure.clone()) else {
            return;
        };
        let model = untrack(|| factory());
        with_registry(|registry| {
            if registry.desired_workspaces.as_ref() != Some(&model) {
                registry.desired_workspaces = Some(model.clone());
                registry.pending.workspace = Some(model);
            }
        });
    }

    /// A bar asked to activate a workspace.
    pub fn on_activate(&self, listener: impl Fn(&RuntimeWorkspaceActivateRequestSnapshot) + 'static) {
        with_registry(|registry| registry.listeners.workspace_activate.push(Rc::new(listener)));
    }
}

/// `COMPOSITOR.layer`: layer-shell surfaces (bars, wallpapers, ...).
pub struct LayerController;

impl LayerController {
    /// All layers, by id (tracked).
    pub fn state(&self) -> ReadSignal<BTreeMap<String, WaylandLayerSnapshot>> {
        runtime::global().layers.read_only()
    }

    pub fn list(&self) -> Vec<WaylandLayerSnapshot> {
        untrack(|| runtime::global().layers.with(|layers| layers.values().cloned().collect()))
    }

    /// The part of `output` not reserved by exclusive zones (tracked).
    pub fn usable_area(&self, output: &str) -> Option<Rect> {
        let snapshot = runtime::global().outputs.with(|outputs| outputs.get(output).cloned())?;
        let area = output_logical_rect(&snapshot)?;
        let (mut top, mut right, mut bottom, mut left) = (0.0, 0.0, 0.0, 0.0);
        runtime::global().layers.with(|layers| {
            for layer in layers.values() {
                if layer.output_name != output {
                    continue;
                }
                let LayerExclusiveZoneSnapshot::Exclusive { size } = layer.exclusive_zone else {
                    continue;
                };
                let size = size as f64;
                match layer.exclusive_edge.or_else(|| edge_from_anchor(layer)) {
                    Some(LayerEdgeSnapshot::Top) => top += size,
                    Some(LayerEdgeSnapshot::Bottom) => bottom += size,
                    Some(LayerEdgeSnapshot::Left) => left += size,
                    Some(LayerEdgeSnapshot::Right) => right += size,
                    None => {}
                }
            }
        });
        Some(Rect::new(
            area.x + left,
            area.y + top,
            (area.width - left - right).max(0.0),
            (area.height - top - bottom).max(0.0),
        ))
    }
}

fn edge_from_anchor(layer: &WaylandLayerSnapshot) -> Option<LayerEdgeSnapshot> {
    let anchor = layer.anchor;
    let count = [anchor.top, anchor.bottom, anchor.left, anchor.right]
        .iter()
        .filter(|edge| **edge)
        .count();
    match count {
        1 if anchor.top => Some(LayerEdgeSnapshot::Top),
        1 if anchor.bottom => Some(LayerEdgeSnapshot::Bottom),
        1 if anchor.left => Some(LayerEdgeSnapshot::Left),
        1 => Some(LayerEdgeSnapshot::Right),
        3 if !anchor.bottom => Some(LayerEdgeSnapshot::Top),
        3 if !anchor.top => Some(LayerEdgeSnapshot::Bottom),
        3 if !anchor.right => Some(LayerEdgeSnapshot::Left),
        3 => Some(LayerEdgeSnapshot::Right),
        _ => None,
    }
}

/// `COMPOSITOR.window`.
pub struct WindowController;

impl WindowController {
    /// `window.composition = (window) => ...`: build a window's decoration.
    /// Runs once per window; see [`crate::view`].
    pub fn composition<C: Into<Composition>>(&self, f: impl Fn(Window) -> C + 'static) {
        with_registry(|registry| {
            registry.composition = Some(Rc::new(move |window| f(window).into()));
        });
    }

    /// `window.decoration.configure`: server- or client-side decorations.
    pub fn decoration(
        &self,
        f: impl Fn(&WaylandWindowSnapshot, &WindowDecorationPolicyContextSnapshot) -> WindowDecorationModeSnapshot
            + 'static,
    ) {
        with_registry(|registry| registry.decoration_policy = Some(Rc::new(f)));
    }

    /// Known windows, oldest first.
    pub fn list(&self) -> Vec<Window> {
        runtime::all_windows().iter().map(|entry| entry.handle).collect()
    }

    pub fn get(&self, id: &str) -> Option<Window> {
        runtime::window_entry(id).map(|entry| entry.handle)
    }

    pub fn focused(&self) -> Option<Window> {
        runtime::all_windows()
            .into_iter()
            .find(|entry| entry.signals.is_focused.get_untracked())
            .map(|entry| entry.handle)
    }
}

/// `COMPOSITOR.effect`.
pub struct EffectController;

impl EffectController {
    /// `effect.background_effect`: drawn behind the regions clients ask for
    /// through `ext-background-effect-v1` (not a full-screen backdrop).
    /// Uniforms may be signals; the compositor follows them.
    pub fn background(&self, effect: Effect) {
        self.background_with(move || Some(effect.clone()));
    }

    /// `effect.background_effect = computed(() => ...)`: re-run when a
    /// signal it reads changes; `None` turns the effect off.
    pub fn background_with(&self, f: impl Fn() -> Option<Effect> + 'static) {
        with_registry(|registry| registry.background_effect = Some(Rc::new(f)));
    }

    /// `effect.overlay(output, { effect, ... })`: draw an effect over a whole
    /// output; see [`crate::overlay`].
    pub fn overlay(&self, output: &str, overlay: Overlay) -> Result<OverlayHandle, String> {
        crate::overlay::create(output, overlay)
    }

    /// `effect.layer = (layer) => ...`.
    pub fn layer(&self, f: impl Fn(&WaylandLayerSnapshot) -> SurfaceEffects + 'static) {
        with_registry(|registry| registry.layer_effect = Some(Rc::new(f)));
        runtime::mark_layer_effects_dirty();
    }

    /// `effect.popup = (popup) => ...`.
    pub fn popup(&self, f: impl Fn(&WaylandPopupSnapshot) -> SurfaceEffects + 'static) {
        with_registry(|registry| registry.popup_effect = Some(Rc::new(f)));
        runtime::mark_layer_effects_dirty();
    }

    /// `effect.window = (window) => ...`: effects around client surfaces.
    pub fn window(&self, f: impl Fn(Window) -> SurfaceEffects + 'static) {
        with_registry(|registry| registry.window_effect = Some(Rc::new(f)));
    }
}

/// The surface a rendering policy is asked about.
#[derive(Clone, Copy)]
pub enum SurfaceRef<'a> {
    Toplevel(Window),
    Popup {
        popup: &'a WaylandPopupSnapshot,
        parent_kind: PopupParentKindSnapshot,
    },
}

/// `COMPOSITOR.rendering`.
pub struct RenderingController;

impl RenderingController {
    /// `rendering.surfacePolicy`: e.g. ignore a lying opaque region.
    pub fn surface_policy(&self, f: impl Fn(SurfaceRef<'_>) -> Option<SurfacePolicy> + 'static) {
        with_registry(|registry| registry.surface_policy = Some(Rc::new(f)));
    }

    /// `rendering.framePacing = "throughput"`: the frame pacing of every
    /// output.
    pub fn frame_pacing(&self, pacing: FramePacing) {
        self.frame_pacing_with(move |_| pacing);
    }

    /// `rendering.framePacing = (output) => ...`: per output, re-read at the
    /// end of every turn.
    pub fn frame_pacing_with(&self, f: impl Fn(&WaylandOutputSnapshot) -> FramePacing + 'static) {
        with_registry(|registry| registry.frame_pacing = Some(Rc::new(f)));
    }

    /// `rendering.composition = (output) => ...`: how each output is put
    /// together; see [`crate::output_composition`]. Re-run only when signals
    /// it reads (or the output) change.
    pub fn composition(&self, f: impl Fn(&WaylandOutputSnapshot) -> OutputStack + 'static) {
        with_registry(|registry| registry.output_composition = Some(Rc::new(f)));
    }
}

/// `COMPOSITOR.debug`.
pub struct DebugController;

impl DebugController {
    pub fn set_fps_counter(&self, enabled: bool) {
        with_registry(|registry| {
            registry.debug.fps_counter = enabled;
            registry.pending.debug = true;
        });
    }

    pub fn enable_profile(&self, enabled: bool) {
        with_registry(|registry| {
            registry.debug.profile = enabled;
            registry.pending.debug = true;
        });
    }
}

/// `COMPOSITOR.event`.
pub struct EventController;

macro_rules! listeners {
    ($($(#[$doc:meta])* $method:ident => $field:ident: $($arg:ty),*;)*) => {
        impl EventController {
            $(
                $(#[$doc])*
                pub fn $method(&self, listener: impl Fn($($arg),*) + 'static) {
                    with_registry(|registry| registry.listeners.$field.push(Rc::new(listener)));
                }
            )*
        }
    };
}

listeners! {
    /// A window appeared (before its first evaluation).
    on_open => open: Window;
    /// Before the first configure, so the config can pick the initial size.
    on_initial_configure => initial_configure: Window;
    /// The client committed its first buffer.
    on_first_commit => first_commit: Window;
    /// The window is gone.
    on_close => close: Window;
    /// The window started closing; set a close animation here.
    on_start_close => start_close: Window;
    on_focus => focus: Window, bool;
    on_window_resize => window_resize: Window, &WindowResizeEventSnapshot;
    on_window_move => window_move: Window, &WindowMoveEventSnapshot;
    /// Registering one replaces the compositor's default handling.
    on_window_maximize_request => maximize_request: Window, &WindowMaximizeRequestEventSnapshot;
    on_window_minimize_request => minimize_request: Window, &WindowMinimizeRequestEventSnapshot;
    on_window_fullscreen_request => fullscreen_request: Window, &WindowFullscreenRequestEventSnapshot;
    on_window_activate_request => activate_request: Window, &WindowActivateRequestEventSnapshot;
    on_output_change => output_change: &OutputChangeEvent;
    on_input_device_change => input_change: &InputChangeEvent;
    on_keyboard_layout_change => keyboard_layout: &KeyboardLayoutSnapshot;
    on_create_layer => create_layer: &WaylandLayerSnapshot;
    on_update_layer => update_layer: &WaylandLayerSnapshot;
    on_destroy_layer => destroy_layer: &WaylandLayerSnapshot;
}

impl EventController {
    /// Every pointer motion, answered before the compositor moves on.
    pub fn on_pointer_move(&self, listener: impl Fn(&PointerMoveEventSnapshot) + 'static) {
        add_filtered(|listeners| listeners.pointer_move.push(Rc::new(listener)));
    }

    /// Pointer motion delivered without blocking the compositor.
    pub fn on_pointer_move_async(&self, listener: impl Fn(&PointerMoveEventSnapshot) + 'static) {
        add_filtered(|listeners| listeners.pointer_move_async.push(Rc::new(listener)));
    }

    pub fn on_gesture_swipe(&self, listener: impl Fn(&GestureSwipeEventSnapshot) + 'static) {
        add_filtered(|listeners| listeners.gesture_swipe.push(Rc::new(listener)));
    }

    pub fn on_gesture_swipe_async(&self, listener: impl Fn(&GestureSwipeEventSnapshot) + 'static) {
        add_filtered(|listeners| listeners.gesture_swipe_async.push(Rc::new(listener)));
    }
}

/// Register a listener the compositor has to be told to deliver.
fn add_filtered(register: impl FnOnce(&mut Listeners)) {
    with_registry(|registry| {
        register(&mut registry.listeners);
        registry.pending.event_filter = true;
    });
}
