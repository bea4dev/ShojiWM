//! The default ShojiWM config (`packages/config/src/index.tsx`) ported to
//! Rust on top of `shojiwm_rs`'s reactive API.
//!
//! ```sh
//! cargo run -p shojiwm_rs --example default_config -- --dev          # nested
//! cargo run -p shojiwm_rs --example default_config --release -- --tty
//! ```
//!
//! Shaders and icons are shared with the TypeScript config: relative asset
//! paths resolve against `packages/config`.

mod flip_3d;
mod island_glass;
mod window_animation;
mod window_manager;
mod workspace;

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use serde_json::{Value, json};
use shojiwm_rs::{
    ipc::IpcServer,
    prelude::*,
    runtime_input::{
        RuntimeInputAccelProfile, RuntimeInputConfig, RuntimeInputDeviceConfig, RuntimeInputMotionSpace,
        RuntimeInputScrollMethod, RuntimeKeyboardInputConfig, RuntimePointerInputConfig,
        RuntimeTouchpadInputConfig,
    },
    runtime_process::RuntimeProcessRestartPolicy,
    runtime_workspace::{RuntimeWorkspaceConfigUpdate, RuntimeWorkspaceEntry, RuntimeWorkspaceGroupConfig},
    ssd::{
        OpaqueRegionPolicy, PointerMoveEventSnapshot, PopupParentKindSnapshot, SurfacePolicy,
        WindowDecorationModeSnapshot,
    },
};

use crate::{
    flip_3d::Flip3D,
    island_glass::island_glass,
    window_manager::{
        TITLEBAR_HEIGHT, WINDOW_BORDER_PX, WINDOW_STATE_FULLSCREEN, WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
        WINDOW_STATE_MINIMIZED, WINDOW_STATE_RECT, WINDOW_STATE_TILE_DRAGGING, WINDOW_STATE_TILE_REORDERING,
        WINDOW_STATE_TILED, WINDOW_STATE_VISIBLE_OUTPUTS, WINDOW_STATE_WORKSPACE_OFFSET_Y,
        WINDOW_STATE_WORKSPACE_OPACITY, WINDOW_STATE_WORKSPACE_TILED, WINDOW_STATE_WORKSPACE_VISIBLE,
        WindowManager, WorkspaceGestureSpeed, output_rect,
    },
};

const FULLSCREEN_Z_INDEX: i32 = 2_000_000_000;
const FLOATING_WINDOW_Z_INDEX_BASE: i32 = 1_500_000_000;
const WINDOW_STACK_Z_INDEX_RANGE: i32 = 100_000_000;
const FOCUSED_TILED_WINDOW_Z_INDEX: i32 = 1_000_000_000;
const REORDERING_TILED_WINDOW_Z_INDEX: i32 = -2_000_000_000;

// Dock proximity, with hysteresis: the pointer has to reach the bottom 10px
// to reveal the dock, and leave the bottom 120px to dismiss it.
const DOCK_SHOW_ZONE_PX: f64 = 10.0;
const DOCK_HIDE_ZONE_PX: f64 = 120.0;

fn main() -> std::process::ExitCode {
    ConfigBuilder::new(setup)
        .name("rust-default-config")
        .asset_root(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/config"))
        .run()
}

/// The client geometry plus the frame the decoration adds around it.
fn natural_root_rect(window: Window) -> Rect {
    let client = window.position().get_untracked();
    Rect::new(
        client.x - WINDOW_BORDER_PX,
        client.y - TITLEBAR_HEIGHT - WINDOW_BORDER_PX,
        client.width + WINDOW_BORDER_PX * 2.0,
        client.height + TITLEBAR_HEIGHT + WINDOW_BORDER_PX * 2.0,
    )
}

fn setup() {
    COMPOSITOR.env.apply([
        ("QT_QPA_PLATFORM", "wayland;xcb"),
        ("QT_QPA_PLATFORMTHEME", "qt6ct"),
        ("QT_IM_MODULE", "fcitx"),
        ("XMODIFIERS", "@im=fcitx"),
        ("SDL_IM_MODULE", "fcitx"),
        ("GLFW_IM_MODULE", "ibus"),
        ("ELECTRON_OZONE_PLATFORM_HINT", "wayland"),
    ]);
    COMPOSITOR.env.publish();
    COMPOSITOR.cursor.configure("Bibata-Modern-Ice", 24);

    COMPOSITOR.process.once(
        "GTK-CSD-control-buttons",
        Command::shell("gsettings set org.gnome.desktop.wm.preferences button-layout ':minimize,maximize,close'"),
    );

    COMPOSITOR.window.decoration(|_window, context| {
        context.client_preference.unwrap_or(WindowDecorationModeSnapshot::Server)
    });

    // Triple buffering: render the next frame while the previous one waits
    // for its vblank.
    COMPOSITOR.rendering.frame_pacing(FramePacing::Throughput);

    let wm = WindowManager::new(natural_root_rect);
    // Flip 3D window switcher (Super+Tab, see flip_3d.rs).
    let flip = {
        let wm = wm.clone();
        Flip3D::new(wm.clone(), move |window| {
            if window.state(&WINDOW_STATE_FULLSCREEN).get_untracked() {
                FULLSCREEN_Z_INDEX
            } else {
                untrack(|| window_z_index(&wm, window))
            }
        })
    };
    {
        let flip = flip.clone();
        COMPOSITOR
            .rendering
            .composition(move |output| flip.compose(output).unwrap_or_else(OutputStack::default_stacking));
    }
    let ipc = setup_ipc(&wm);
    setup_processes();
    setup_key_bindings(&wm, &ipc, &flip);
    setup_outputs_and_input();
    wm.with(|wm| {
        wm.configure_workspace_gesture_speed(WorkspaceGestureSpeed {
            workspace_scroll_factor: 1.5,
            workspace_scroll_kinetic_factor: 1.0,
            workspace_switch_factor: 1.0,
            workspace_switch_velocity_factor: 1.0,
            // At or below this scroll speed (logical px/s) the workspace
            // scroll catches on tile snap positions. 0 disables snapping.
            workspace_scroll_snap_max_velocity: 600.0,
            // Finger travel (logical px) needed to break out of a catch.
            workspace_scroll_snap_breakout_px: 48.0,
        })
    });
    setup_effects(&flip);
    setup_events(&wm, &ipc);
    setup_composition(&wm, &flip);
}

/// External IPC for the bar:
///
/// - `workspaces.get` -> WorkspacesView (request/response)
/// - `workspaces.switch { direction: -1 | 1 }`
/// - `workspaces.activate { monitor, index }`
/// - `workspaces.toggleTiling { monitor? }`
/// - `windows.activate { windowId }`
/// - broadcasts: `workspaces.changed`, `dock.proximity`, `snap.preview`
#[derive(Clone)]
struct Ipc {
    server: Option<IpcServer>,
    wm: WindowManager,
    last_workspaces: Rc<RefCell<String>>,
    last_snap: Rc<RefCell<String>>,
    dock_proximity: Rc<RefCell<BTreeMap<String, bool>>>,
}

impl Ipc {
    fn broadcast(&self, event: &str, payload: Value) {
        if let Some(server) = &self.server {
            server.broadcast(event, payload);
        }
    }

    /// Stage the protocol workspace model (before the reply goes out, so bars
    /// see it on this turn) and push the diffed view over IPC.
    fn schedule_workspace_broadcast(&self) {
        COMPOSITOR.workspace.reconfigure();
        let view = self.wm.read(|wm| wm.view_for_ipc()).to_json();
        let encoded = view.to_string();
        if *self.last_workspaces.borrow() == encoded {
            return;
        }
        *self.last_workspaces.borrow_mut() = encoded;
        self.broadcast("workspaces.changed", view);
    }

    fn update_dock_proximity(&self, monitor: &str, inside: bool) {
        if self.dock_proximity.borrow().get(monitor) == Some(&inside) {
            return;
        }
        self.dock_proximity
            .borrow_mut()
            .insert(monitor.to_owned(), inside);
        self.broadcast("dock.proximity", json!({ "monitor": monitor, "inside": inside }));
    }

    fn next_dock_proximity(&self, monitor: &str, x: f64, y: f64, on_tracked_monitor: bool) -> bool {
        if !on_tracked_monitor {
            return false;
        }
        let was_inside = self.dock_proximity.borrow().get(monitor) == Some(&true);
        let strip = if was_inside { DOCK_HIDE_ZONE_PX } else { DOCK_SHOW_ZONE_PX };
        output_rect(monitor).is_some_and(|rect| {
            x >= rect.x && x < rect.right() && y >= rect.bottom() - strip && y < rect.bottom()
        })
    }
}

fn setup_ipc(wm: &WindowManager) -> Ipc {
    let server = match IpcServer::new() {
        Ok(server) => Some(server),
        Err(error) => {
            tracing::warn!(%error, "workspace IPC socket unavailable");
            None
        }
    };
    let ipc = Ipc {
        server,
        wm: wm.clone(),
        last_workspaces: Rc::default(),
        last_snap: Rc::default(),
        dock_proximity: Rc::default(),
    };

    {
        let wm = wm.clone();
        COMPOSITOR.workspace.configure(move || {
            let view = wm.read(|wm| wm.view_for_ipc());
            RuntimeWorkspaceConfigUpdate {
                groups: view
                    .monitors
                    .iter()
                    .map(|monitor| RuntimeWorkspaceGroupConfig {
                        id: monitor.name.clone(),
                        outputs: vec![monitor.name.clone()],
                        workspaces: monitor
                            .workspaces
                            .iter()
                            .map(|workspace| RuntimeWorkspaceEntry {
                                id: format!("{}:{}", monitor.name, workspace.index),
                                name: workspace.index.to_string(),
                                coordinates: vec![workspace.index.saturating_sub(1)],
                                active: workspace.active,
                                urgent: false,
                                hidden: !workspace.active && workspace.window_count == 0,
                            })
                            .collect(),
                    })
                    .collect(),
            }
        });
    }

    {
        let ipc = ipc.clone();
        COMPOSITOR.workspace.on_activate(move |event| {
            let Some((monitor, index)) = event.workspace_id.split_once(':') else {
                return;
            };
            let Ok(index) = index.parse::<u32>() else {
                return;
            };
            if index >= 1 {
                ipc.wm.with(|wm| wm.activate(monitor, index));
                ipc.schedule_workspace_broadcast();
            }
        });
    }

    if let Some(server) = &ipc.server {
        {
            let wm = wm.clone();
            server.handle("workspaces.get", move |_| Ok(wm.read(|wm| wm.view_for_ipc()).to_json()));
        }
        {
            let ipc = ipc.clone();
            server.handle("workspaces.switch", move |params| {
                let direction = if params["direction"].as_i64() == Some(-1) { -1 } else { 1 };
                ipc.wm.with(|wm| wm.switch_workspace(direction));
                ipc.schedule_workspace_broadcast();
                Ok(Value::Null)
            });
        }
        {
            let ipc = ipc.clone();
            server.handle("workspaces.activate", move |params| {
                if let (Some(monitor), Some(index)) = (params["monitor"].as_str(), params["index"].as_u64()) {
                    ipc.wm.with(|wm| wm.activate(monitor, index as u32));
                    ipc.schedule_workspace_broadcast();
                }
                Ok(Value::Null)
            });
        }
        {
            let ipc = ipc.clone();
            server.handle("workspaces.toggleTiling", move |params| {
                match params["monitor"].as_str() {
                    Some(monitor) => ipc.wm.with(|wm| wm.toggle_workspace_tiling_for_monitor(monitor)),
                    None => ipc.wm.with(|wm| wm.toggle_current_workspace_tiling()),
                }
                ipc.schedule_workspace_broadcast();
                Ok(Value::Null)
            });
        }
        {
            let ipc = ipc.clone();
            server.handle("windows.activate", move |params| {
                if let Some(id) = params["windowId"].as_str() {
                    ipc.wm.with(|wm| wm.activate_window_by_id(id));
                    ipc.schedule_workspace_broadcast();
                }
                Ok(Value::Null)
            });
        }
    }

    // Snap-zone preview: the bar draws the rounded overlay.
    {
        let ipc = ipc.clone();
        wm.set_snap_preview_broadcaster(move |preview| {
            let payload = json!({
                "monitor": preview.monitor,
                "kind": preview.kind,
                "rect": preview.rect.map(|rect| json!({
                    "x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height,
                })),
            });
            let encoded = payload.to_string();
            if *ipc.last_snap.borrow() == encoded {
                return;
            }
            *ipc.last_snap.borrow_mut() = encoded;
            ipc.broadcast("snap.preview", payload);
        });
    }
    {
        let ipc = ipc.clone();
        wm.set_workspace_change_broadcaster(move || ipc.schedule_workspace_broadcast());
    }
    {
        let ipc = ipc.clone();
        COMPOSITOR.on_disable(move |_| {
            if let Some(server) = &ipc.server {
                server.close();
            }
        });
    }
    ipc
}

fn setup_processes() {
    COMPOSITOR.process.once("fcitx5", Command::shell("fcitx5 -d"));
    // shoji-bar-3, the session's shell. The shader is compiled first; `;`
    // rather than `&&` so a failed build still starts the previous one.
    COMPOSITOR.process.once(
        "shell",
        Command::shell("cd ~/.config/shoji-bar-3 && bash build.sh; exec quickshell --path ."),
    );
    // cliphist history watchers, restarted if they ever exit.
    COMPOSITOR.process.service(
        "cliphist-text",
        Command::exec(["wl-paste", "--type", "text", "--watch", "cliphist", "store"]),
        RuntimeProcessRestartPolicy::OnExit,
    );
    COMPOSITOR.process.service(
        "cliphist-image",
        Command::exec(["wl-paste", "--type", "image", "--watch", "cliphist", "store"]),
        RuntimeProcessRestartPolicy::OnExit,
    );
}

fn spawn(command: &str) {
    COMPOSITOR.process.spawn(Command::shell(command));
}

fn setup_key_bindings(wm: &WindowManager, ipc: &Ipc, flip: &Flip3D) {
    COMPOSITOR.key.bind("terminal", "Super+T", || {
        COMPOSITOR.process.spawn(Command::exec(["kitty"]));
    });
    // With kwallet6 as the password store, add --password-store=kwallet6.
    COMPOSITOR.key.bind("chrome", "Super+B", || {
        spawn("google-chrome-stable --enable-features=OzonePlatform --ozone-platform=wayland");
    });
    COMPOSITOR.key.bind("discord", "Super+D", || {
        spawn(
            "discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime --disable-gpu",
        );
    });
    COMPOSITOR.key.bind("dolphin", "Super+E", || spawn("dolphin"));
    COMPOSITOR.key.bind("play", "XF86AudioPlay", || spawn("playerctl play-pause"));
    COMPOSITOR.key.bind("pause", "XF86AudioPause", || spawn("playerctl play-pause"));
    COMPOSITOR.key.bind("next", "XF86AudioNext", || spawn("playerctl next"));
    COMPOSITOR.key.bind("prev", "XF86AudioPrev", || spawn("playerctl previous"));

    // shoji-bar-3's launcher on the monitor under the cursor (Quickshell's
    // IPC is addressed by config path, so a hand-started bar works too).
    let toggle_launcher = {
        let wm = wm.clone();
        move || {
            let monitor = wm.with(|wm| wm.current_monitor_name());
            spawn(&format!(
                "quickshell -p ~/.config/shoji-bar-3 ipc call launcher toggleOn \"{monitor}\""
            ));
        }
    };
    COMPOSITOR.key.bind("launcher", "Super+A", toggle_launcher.clone());
    // A Super tap: fires on release when nothing else was pressed meanwhile.
    COMPOSITOR.key.bind_release("launcher-tap", "Super", toggle_launcher);
    {
        let wm = wm.clone();
        COMPOSITOR.key.bind("clipboard", "Super+V", move || {
            let monitor = wm.with(|wm| wm.current_monitor_name());
            spawn(&format!(
                "quickshell -p ~/.config/shoji-bar-3 ipc call launcher clipboardOn \"{monitor}\""
            ));
        });
    }
    // A lock covers every screen; the shell refuses to unlock over IPC.
    COMPOSITOR.key.bind("lock", "Super+L", || {
        spawn("quickshell -p ~/.config/shoji-bar-3 ipc call session lock");
    });
    COMPOSITOR.key.bind("screenshot", "Super+P", || spawn("hyprshot -m region --raw | swappy -f -"));
    COMPOSITOR.key.bind("screenshot-freeze", "Super+Ctrl+P", || {
        spawn("hyprshot -m region --freeze --raw | swappy -f -");
    });

    let bind = |id: &str, shortcut: &str, broadcast: bool, f: fn(&mut window_manager::HybridWindowManager)| {
        let ipc = ipc.clone();
        COMPOSITOR.key.bind(id, shortcut, move || {
            ipc.wm.with(f);
            if broadcast {
                ipc.schedule_workspace_broadcast();
            }
        });
    };
    bind("toggle-tiling-mode", "Super+S", true, |wm| wm.toggle_current_workspace_tiling());
    {
        let (wm, flip) = (wm.clone(), flip.clone());
        COMPOSITOR.key.bind("flip-3d", "Super+Tab", move || {
            let monitor = wm.with(|wm| wm.current_monitor_name());
            flip.open(&monitor);
        });
    }
    bind("close-focused-window", "Super+Q", false, |wm| wm.close_focused_window());
    bind("toggle-focused-window-maximize", "Super+M", false, |wm| wm.toggle_focused_window_maximize());
    bind("toggle-focused-window-fullscreen", "Super+F", false, |wm| {
        wm.toggle_focused_window_fullscreen()
    });
    bind("tile-focus-left-quick", "Super+Left", false, |wm| wm.focus_tile(-1));
    bind("tile-focus-right-quick", "Super+Right", false, |wm| wm.focus_tile(1));
    bind("tile-focus-left", "Super+Ctrl+Left", false, |wm| wm.focus_tile(-1));
    bind("tile-focus-right", "Super+Ctrl+Right", false, |wm| wm.focus_tile(1));
    bind("tile-move-left", "Super+Shift+Left", true, |wm| wm.move_focused_tile(-1));
    bind("tile-move-right", "Super+Shift+Right", true, |wm| wm.move_focused_tile(1));
    bind("window-move-workspace-prev", "Super+Shift+Up", true, |wm| {
        wm.move_focused_window_to_workspace(-1)
    });
    bind("window-move-workspace-next", "Super+Shift+Down", true, |wm| {
        wm.move_focused_window_to_workspace(1)
    });
    bind("workspace-prev", "Super+Ctrl+Up", true, |wm| wm.switch_workspace(-1));
    bind("workspace-next", "Super+Ctrl+Down", true, |wm| wm.switch_workspace(1));

    let fps_counter = Rc::new(std::cell::Cell::new(false));
    COMPOSITOR.key.bind("fps", "Super+Shift+F", move || {
        fps_counter.set(!fps_counter.get());
        COMPOSITOR.debug.set_fps_counter(fps_counter.get());
    });
    let profile = Rc::new(std::cell::Cell::new(false));
    COMPOSITOR.key.bind("profile", "Super+Shift+T", move || {
        profile.set(!profile.get());
        COMPOSITOR.debug.enable_profile(profile.get());
    });
}

fn setup_outputs_and_input() {
    COMPOSITOR.output.configure(|context| {
        let mut display = BTreeMap::new();
        let mut edp = OutputConfig::extend(1.8);
        edp.transform = Some(shojiwm_rs::compositor::OutputTransform::Normal);
        display.insert("eDP-1".to_owned(), Some(edp));
        display.insert("eDP-2".to_owned(), Some(OutputConfig::extend(1.8)));
        display.insert("HDMI-A-1".to_owned(), Some(OutputConfig::extend(1.5)));
        display.insert("DP-1".to_owned(), Some(OutputConfig::extend(1.5)));
        display.insert("DP-4".to_owned(), Some(OutputConfig::extend(1.5)));
        display.insert("DP-2".to_owned(), Some(OutputConfig::extend(1.6)));
        let docked = context.connected.iter().any(|output| output.name == "HDMI-A-1");
        if docked {
            display.insert("eDP-1".to_owned(), Some(OutputConfig::disabled()));
            display.insert("eDP-2".to_owned(), Some(OutputConfig::disabled()));
        }
        display
    });

    COMPOSITOR.input.configure(|input: &mut RuntimeInputConfig, _devices| {
        input.global = Some(RuntimeInputDeviceConfig {
            touchpad: Some(RuntimeTouchpadInputConfig {
                tap_to_click: Some(true),
                natural_scroll: Some(true),
                scroll_method: Some(RuntimeInputScrollMethod::TwoFinger),
                disable_while_typing: Some(true),
                scroll_factor: Some(0.3),
                ..Default::default()
            }),
            pointer: Some(RuntimePointerInputConfig {
                pointer_accel: Some(0.0),
                accel_profile: Some(RuntimeInputAccelProfile::Flat),
                motion_space: Some(RuntimeInputMotionSpace::Logical),
                ..Default::default()
            }),
            keyboard: Some(RuntimeKeyboardInputConfig {
                options: Some("caps:ctrl_modifier".into()),
                repeat_rate: Some(60),
                repeat_delay: Some(250),
                ..Default::default()
            }),
        });
        input.device.insert(
            "Razer Razer Blade Keyboard".into(),
            Some(RuntimeInputDeviceConfig {
                keyboard: Some(RuntimeKeyboardInputConfig {
                    layout: Some("us".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
    });

    COMPOSITOR.pointer.bind_window_move_modifier("Super");
    COMPOSITOR.pointer.bind_window_resize_modifier("Super");
}

/// Window backdrop effects end with this stage so they can fade back in
/// after the Flip 3D switcher closes.
fn backdrop_fade(flip: &Flip3D) -> ShaderStage {
    shader_stage("./src/effect/backdrop-fade.frag").uniform("strength", flip.backdrop_strength())
}

fn backdrop_blur(flip: &Flip3D) -> Effect {
    Effect::new(backdrop_source())
        .capture_padding(24)
        .invalidate(Invalidate::on_source_damage_box(8))
        .preserve_alpha()
        .stage(dual_kawase_blur(4, 2))
        .stage(backdrop_fade(flip))
}

/// Where a (non-fullscreen) window sits in the stack; higher is on top.
fn window_z_index(wm: &WindowManager, window: Window) -> i32 {
    let stack_z_index = wm.window_z_index(window).get();
    if !window.state(&WINDOW_STATE_WORKSPACE_TILED).get() {
        return stack_z_index;
    }
    let offset = stack_z_index.clamp(-WINDOW_STACK_Z_INDEX_RANGE, WINDOW_STACK_Z_INDEX_RANGE);
    if !window.state(&WINDOW_STATE_TILED).get() {
        FLOATING_WINDOW_Z_INDEX_BASE + offset
    } else if window.state(&WINDOW_STATE_TILE_REORDERING).get() {
        REORDERING_TILED_WINDOW_Z_INDEX
    } else if window.is_focused().get() {
        FOCUSED_TILED_WINDOW_Z_INDEX
    } else {
        offset
    }
}

/// The blur clipped to the surface's own alpha, so its transparency has to
/// survive the finish pass.
fn masked_blur(mask: Source, capture_padding: i32) -> Effect {
    Effect::new(backdrop_source())
        .capture_padding(capture_padding)
        .invalidate(Invalidate::on_source_damage_box(8))
        .preserve_alpha()
        .stage(dual_kawase_blur(4, 2))
        .stage(
            shader_stage("./src/effect/layer-blur-mask.frag")
                .texture("layer_mask", mask)
                .uniform("opacity_threshold", 0.25)
                .uniform("mask_feather", 0.04),
        )
}

fn setup_effects(flip: &Flip3D) {
    // Blur behind the regions clients ask for (ext-background-effect). Off
    // while Flip 3D is shown, then faded back in like the window backdrops.
    let background_blur = backdrop_blur(flip);
    let blur_suspended = flip.blur_suspended();
    COMPOSITOR
        .effect
        .background_with(move || (!blur_suspended.get()).then(|| background_blur.clone()));

    let layer_blur_mask = SurfaceEffect::new(masked_blur(layer_source(), 24));
    let island = island_glass();
    COMPOSITOR.effect.layer(move |layer| match layer.namespace.as_deref() {
        // Both draw only a translucent silhouette; the compositor recovers the
        // shape from its alpha.
        Some("liquid-island-qs" | "shoji-bar-3") => SurfaceEffects::behind(island.clone()),
        Some("no_blur") => SurfaceEffects::none(),
        _ => SurfaceEffects::behind(layer_blur_mask.clone()),
    });

    let popup_blur = SurfaceEffect::new(masked_blur(popup_source(), 4 * 2 * 2 + 24 + 32));
    COMPOSITOR.effect.popup(move |popup| {
        if popup.parent_kind == PopupParentKindSnapshot::Window {
            SurfaceEffects::none()
        } else {
            SurfaceEffects::behind(popup_blur.clone())
        }
    });

    // GTK3 tooltips declare their whole rect opaque despite rounded corners;
    // Chromium-family clients repaint their shadow margins transparent black
    // once minimized while still declaring them opaque.
    COMPOSITOR.rendering.surface_policy(|surface| {
        let ignore = Some(SurfacePolicy {
            opaque_region: OpaqueRegionPolicy::Ignore,
        });
        match surface {
            SurfaceRef::Popup {
                parent_kind: PopupParentKindSnapshot::Layer,
                ..
            } => ignore,
            SurfaceRef::Toplevel(window) => {
                let app_id = window.app_id().get().unwrap_or_default().to_lowercase();
                let chromium = ["chrome", "chromium", "electron"]
                    .iter()
                    .any(|name| app_id.contains(name));
                let minimized = window.state(&WINDOW_STATE_MINIMIZED).get()
                    || window.state(&WINDOW_STATE_MINIMIZE_VISUAL_IDLE).get();
                (chromium && minimized).then_some(ignore).flatten()
            }
            SurfaceRef::Popup { .. } => None,
        }
    });
}

fn setup_events(wm: &WindowManager, ipc: &Ipc) {
    let on = |f: fn(&mut window_manager::HybridWindowManager, Window), broadcast: bool| {
        let ipc = ipc.clone();
        move |window: Window| {
            ipc.wm.with(|wm| f(wm, window));
            if broadcast {
                ipc.schedule_workspace_broadcast();
            }
        }
    };
    COMPOSITOR.event.on_open(on(|wm, window| wm.on_open(window), false));
    COMPOSITOR
        .event
        .on_initial_configure(on(|wm, window| wm.on_initial_configure(window), false));
    COMPOSITOR.event.on_first_commit(on(|wm, window| wm.on_first_commit(window), true));
    COMPOSITOR.event.on_start_close(on(|wm, window| wm.on_start_close(window), true));
    COMPOSITOR.event.on_close(on(|wm, window| wm.on_close(window), true));

    {
        let ipc = ipc.clone();
        COMPOSITOR.event.on_focus(move |window, focused| {
            ipc.wm.with(|wm| {
                wm.on_focus(window, focused);
                if focused {
                    wm.record_focus(window);
                }
            });
            if focused {
                ipc.schedule_workspace_broadcast();
            }
        });
    }

    {
        let ipc = ipc.clone();
        COMPOSITOR.event.on_pointer_move(move |event: &PointerMoveEventSnapshot| {
            ipc.wm.with(|wm| wm.on_pointer_move(event));
            // Dock proximity: only the monitor under the pointer can be
            // "inside"; the others report leave.
            for monitor in COMPOSITOR.output.list() {
                let on_monitor = event.output_name.as_deref() == Some(monitor.as_str());
                let inside = ipc.next_dock_proximity(&monitor, event.position.x, event.position.y, on_monitor);
                ipc.update_dock_proximity(&monitor, inside);
            }
        });
    }
    {
        let ipc = ipc.clone();
        COMPOSITOR.event.on_gesture_swipe(move |event| {
            ipc.wm.with(|wm| wm.on_gesture_swipe(event));
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let ipc = ipc.clone();
        COMPOSITOR.event.on_output_change(move |event| {
            ipc.wm.with(|wm| wm.on_output_change(event));
            ipc.schedule_workspace_broadcast();
        });
    }
    for register in [
        |f: Box<dyn Fn(&shojiwm_rs::ssd::WaylandLayerSnapshot)>| COMPOSITOR.event.on_create_layer(f),
        |f: Box<dyn Fn(&shojiwm_rs::ssd::WaylandLayerSnapshot)>| COMPOSITOR.event.on_update_layer(f),
        |f: Box<dyn Fn(&shojiwm_rs::ssd::WaylandLayerSnapshot)>| COMPOSITOR.event.on_destroy_layer(f),
    ] {
        let wm = wm.clone();
        register(Box::new(move |_| wm.with(|wm| wm.refresh_usable_area_layouts())));
    }

    let wm_event = wm.clone();
    COMPOSITOR
        .event
        .on_window_resize(move |window, event| wm_event.with(|wm| wm.on_window_resize(window, event)));
    let wm_event = wm.clone();
    COMPOSITOR
        .event
        .on_window_move(move |window, event| wm_event.with(|wm| wm.on_window_move(window, event)));
    let wm_event = wm.clone();
    COMPOSITOR.event.on_window_maximize_request(move |window, event| {
        wm_event.with(|wm| wm.on_window_maximize_request(window, event))
    });
    let wm_event = wm.clone();
    COMPOSITOR.event.on_window_minimize_request(move |window, event| {
        wm_event.with(|wm| wm.on_window_minimize_request(window, event))
    });
    let wm_event = wm.clone();
    COMPOSITOR.event.on_window_fullscreen_request(move |window, event| {
        wm_event.with(|wm| wm.on_window_fullscreen_request(window, event))
    });
    let ipc = ipc.clone();
    COMPOSITOR.event.on_window_activate_request(move |window, event| {
        ipc.wm.with(|wm| wm.on_window_activate_request(window, event));
        ipc.schedule_workspace_broadcast();
    });
}

/// `COMPOSITOR.window.composition`: frame, titlebar and placement.
fn setup_composition(wm: &WindowManager, flip: &Flip3D) {
    let wm = wm.clone();
    let flip = flip.clone();
    COMPOSITOR.window.composition(move |window| {
        // Read in the body on purpose: these switch the whole structure, so a
        // change re-runs this function (like the TSX version).
        let decoration = window.decoration().get();
        let use_client_decoration = decoration.mode == WindowDecorationModeSnapshot::Client
            && !(decoration.client_preference == Some(WindowDecorationModeSnapshot::Server)
                && decoration.configured_mode == WindowDecorationModeSnapshot::Server);
        let fullscreen = window.state(&WINDOW_STATE_FULLSCREEN).get();
        let is_terminal = matches!(window.app_id().get().as_deref(), Some("kitty" | "ghostty"));

        let rect = window.state(&WINDOW_STATE_RECT);
        let workspace_offset_y = window.state(&WINDOW_STATE_WORKSPACE_OFFSET_Y);
        let managed_rect = memo(move || {
            let rect = rect.get();
            Rect {
                y: rect.y + workspace_offset_y.get(),
                ..rect
            }
        });
        let force_rect_size =
            memo(move || window.is_resizable().get() && !window.is_transient().get());
        // Force no corner rounding by CSD.
        let tiled = true;

        let z_index = {
            let wm = wm.clone();
            memo(move || window_z_index(&wm, window))
        };
        let minimize_visual_idle = window.state(&WINDOW_STATE_MINIMIZE_VISUAL_IDLE);
        let workspace_visible = window.state(&WINDOW_STATE_WORKSPACE_VISIBLE);
        let tile_dragging = window.state(&WINDOW_STATE_TILE_DRAGGING);
        let inactive =
            memo(move || minimize_visual_idle.get() || (!workspace_visible.get() && !tile_dragging.get()));

        let managed = ManagedWindow::new()
            .rect(managed_rect)
            .visible_outputs(window.state(&WINDOW_STATE_VISIBLE_OUTPUTS))
            .opacity(window.state(&WINDOW_STATE_WORKSPACE_OPACITY))
            .force_rect_size(force_rect_size)
            .tiled(tiled)
            .idle(inactive)
            .interactive(inactive.map(|inactive| !inactive));

        // Fullscreen: no chrome at all, so the tty backend can scan the
        // client buffer out directly. Tearing only happens on that direct
        // scanout path while the client outruns the refresh rate (games).
        if fullscreen {
            return managed
                .z_index(FULLSCREEN_Z_INDEX)
                .allow_tearing(true)
                .child(ClientWindow::new());
        }

        let border_color = window
            .is_focused()
            .map(|focused| if *focused { hex("#d7ba7d") } else { hex("#4f5666") });
        // A soft drop shadow under the frame; the focused window floats a bit
        // higher. The window in front of the Flip 3D stack glows white instead
        // (it fits in the switcher's texture margin).
        let selected_window_id = flip.selected_window_id();
        let window_shadow = memo(move || {
            if selected_window_id.with(|id| id.as_deref() == Some(&*window.id())) {
                vec![BoxShadow {
                    spread: 4.0,
                    ..shadow(0.0, 0.0, 28.0, hex("#ffffffd0"))
                }]
            } else if window.is_focused().get() {
                vec![BoxShadow {
                    spread: -2.0,
                    ..shadow(0.0, 10.0, 32.0, hex("#00000090"))
                }]
            } else {
                vec![BoxShadow {
                    spread: -2.0,
                    ..shadow(0.0, 4.0, 16.0, hex("#00000060"))
                }]
            }
        });
        let frame = || {
            WindowBorder::new()
                .style(
                    Style::new()
                        .border(WINDOW_BORDER_PX, border_color)
                        .border_radius(10.0)
                        .box_shadow(window_shadow)
                        .background(hex("#10131900"))
                        .padding(0.0),
                )
                .resize_hit_area(8, 14)
        };

        // Client-side decorations: just the border.
        if use_client_decoration {
            return managed
                .z_index(z_index)
                .child(frame().child(ClientWindow::new()));
        }

        // No backdrop effects while the Flip 3D switcher shows the windows as
        // textures: there is nothing under a window there to blur. Read here
        // on purpose: it switches the structure.
        let blur_suspended = flip.blur_suspended().get();
        managed.z_index(z_index).child(frame().child(Flex::row().child(decorated_contents(
            window,
            is_terminal,
            (!blur_suspended).then_some(&flip),
        ))))
    });
}

/// Titlebar and client area of a server-side decorated window. Without
/// `blur` (Flip 3D is open) there are no backdrop effects.
fn decorated_contents(window: Window, is_terminal: bool, blur: Option<&Flip3D>) -> Element {
    let titlebar_background = window
        .is_focused()
        .map(|focused| if *focused { hex("#1f243080") } else { hex("#2a2f3a80") });
    let title_color = window
        .is_focused()
        .map(|focused| if *focused { hex("#f5f7fa") } else { hex("#c9d1d9") });
    let titlebar_style = Style::new()
        .height(TITLEBAR_HEIGHT)
        .padding_x(8.0)
        .gap(8.0)
        .align_items(AlignItems::Center)
        .background(titlebar_background);

    let titlebar_children = || {
        vec![
            AppIcon::new().style(Style::new().size(16.0, 16.0)),
            Label::new(window.title()).style(
                Style::new()
                    .color(title_color)
                    .fonts(["Noto Sans CJK JP", "Noto Color Emoji"])
                    .font_size(13.0)
                    .font_weight(600)
                    .flex_grow(1.0)
                    .flex_shrink(1.0)
                    .min_width(0.0),
            ),
            minimize_button(window),
            maximize_button(window),
            close_button(window),
        ]
    };

    if is_terminal {
        let titlebar = Flex::row().style(titlebar_style).children(titlebar_children());
        let Some(flip) = blur else {
            return Flex::column().child(titlebar).child(ClientWindow::new());
        };
        // Terminals are translucent: the whole window sits on liquid glass.
        let background = Effect::new(backdrop_source())
            .capture_padding(24)
            .invalidate(Invalidate::on_source_damage_box(8))
            .preserve_alpha()
            .stage(dual_kawase_blur(4, 2))
            .stage(
                shader_stage("./src/effect/liquid-glass.frag")
                    // Follow the window's rounded corners.
                    .uniform("glass_radius_px", -1.0)
                    .uniform("distortion_depth", 0.2)
                    .uniform("distortion_strength", 0.15)
                    .uniform("chromatic_shift_px", 3.0)
                    .uniform("glass_tint", 0.9),
            )
            .stage(backdrop_fade(flip));
        return ShaderEffect::new(background)
            .direction(Direction::Column)
            .child(titlebar)
            .child(ClientWindow::new());
    }

    let titlebar = match blur {
        Some(flip) => ShaderEffect::new(backdrop_blur(flip))
            .direction(Direction::Row)
            .style(titlebar_style)
            .children(titlebar_children()),
        None => Flex::row().style(titlebar_style).children(titlebar_children()),
    };
    Flex::column().child(titlebar).child(ClientWindow::new())
}

/// How long the pointer has to rest on a title bar button before its tooltip shows.
const TOOLTIP_DELAY_MS: f64 = 500.0;

/// A small label under a title bar button, drawn above every window. It
/// opens once the pointer has rested on the button for [`TOOLTIP_DELAY_MS`]
/// and closes as soon as it leaves.
fn titlebar_tooltip(text: impl Into<Prop<String>>) -> Element {
    Popup::new()
        .trigger(PopupTrigger::hover(TOOLTIP_DELAY_MS, 0.0))
        .placement(PopupPlacement::Bottom)
        .offset(8.0)
        .child(
            Flex::column()
                .style(
                    Style::new()
                        .background(hex("#1f2430e6"))
                        .border_radius(6.0)
                        .padding_x(8.0)
                        .padding_y(4.0)
                        .border(1.0, hex("#ffffff1f"))
                        .box_shadow(shadow(0.0, 4.0, 12.0, hex("#00000070"))),
                )
                .child(
                    Label::new(text).style(
                        Style::new()
                            .color(hex("#e6e9ef"))
                            .fonts(["Noto Sans CJK JP", "Noto Color Emoji"])
                            .font_size(12.0),
                    ),
                ),
        )
}

/// A round titlebar button whose icon appears while hovered.
fn title_button(
    hover_border: Memo<Color>,
    hover: Signal<bool>,
    icon: impl Fn() -> Option<Element> + 'static,
    tooltip: impl Fn() -> Option<Element> + 'static,
    on_click: impl Fn() + 'static,
) -> Element {
    Flex::column()
        .style(Style::new().relative().flex_shrink(0.0))
        .child(
            Button::new()
                .on_hover_change(move |value| hover.set(value))
                .on_click(on_click)
                .style(
                    Style::new()
                        .size(16.0, 16.0)
                        .border_radius(8.0)
                        .background(hex("#FFFFFF20"))
                        .border(1.0, hover_border),
                ),
        )
        .child_dyn(icon)
        .child_dyn(tooltip)
}

fn button_icon(src: impl Into<Prop<String>>) -> Element {
    Image::new(src).style(
        Style::new()
            .size(16.0, 16.0)
            .absolute()
            .z_index(1)
            .pointer_events_none(),
    )
}

fn close_button(window: Window) -> Element {
    let hover = signal(false);
    let border = hover.map(|hover| if *hover { hex("#00000000") } else { hex("#F0808030") });
    title_button(
        border,
        hover,
        move || hover.get().then(|| button_icon("./assets/x.svg")),
        || None,
        move || window.close(),
    )
}

fn maximize_button(window: Window) -> Element {
    let hover = signal(false);
    let border = memo(move || {
        if !window.is_resizable().get() || hover.get() {
            hex("#00000000")
        } else {
            hex("#00BFFF30")
        }
    });
    let should_hover = memo(move || hover.get() && window.is_resizable().get());
    title_button(
        border,
        hover,
        move || {
            should_hover.get().then(|| {
                button_icon(window.is_maximized().map(|maximized| {
                    if *maximized {
                        "./assets/minimize-2.svg"
                    } else {
                        "./assets/maximize-2.svg"
                    }
                }))
            })
        },
        move || {
            window.is_resizable().get().then(|| {
                titlebar_tooltip(window.is_maximized().map(|maximized| {
                    if *maximized { "Restore" } else { "Maximize" }.to_string()
                }))
            })
        },
        move || {
            if !window.is_resizable().get_untracked() {
                return;
            }
            if window.is_maximized().get_untracked() {
                window.unmaximize();
            } else {
                window.maximize();
            }
        },
    )
}

fn minimize_button(window: Window) -> Element {
    let hover = signal(false);
    let border = hover.map(|hover| if *hover { hex("#00000000") } else { hex("#F8FF7530") });
    title_button(
        border,
        hover,
        move || hover.get().then(|| button_icon("./assets/minus.svg")),
        || Some(titlebar_tooltip("Minimize")),
        move || window.minimize(),
    )
}

#[cfg(test)]
mod tests {
    //! Drives the ported config through the compositor's `RuntimeHandle`,
    //! the same calls `shojiwm_lib` makes, to exercise the window manager.

    use std::collections::BTreeMap;

    use shojiwm_rs::{
        RuntimeBoot, RuntimeHandle, RuntimeHost,
        cli::CommonArgs,
        ssd::{
            OutputModeSnapshot, OutputPositionSnapshot, WaylandOutputSnapshot, WaylandWindowSnapshot,
            WindowPositionSnapshot,
        },
    };

    use super::*;

    fn output(name: &str, x: i32) -> WaylandOutputSnapshot {
        WaylandOutputSnapshot {
            name: name.into(),
            description: None,
            make: None,
            model: None,
            serial: None,
            connector: Some(name.into()),
            enabled: true,
            resolution: Some(OutputModeSnapshot {
                width: 1920,
                height: 1080,
                refresh_rate: 60.0,
                clock_khz: None,
            }),
            position: OutputPositionSnapshot { x, y: 0 },
            scale: 1.0,
            transform: Default::default(),
            available_modes: Vec::new(),
            subpixel: Default::default(),
            detected_subpixel: Default::default(),
            hdr_supported: false,
            hdmi: None,
        }
    }

    fn window(id: &str, focused: bool) -> WaylandWindowSnapshot {
        WaylandWindowSnapshot {
            output_name: None,
            id: id.into(),
            title: format!("window {id}"),
            app_id: Some("org.example.App".into()),
            position: WindowPositionSnapshot {
                x: 100.0,
                y: 100.0,
                width: 800.0,
                height: 600.0,
            },
            rect: Default::default(),
            is_focused: focused,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: Default::default(),
        }
    }

    fn start() -> RuntimeHandle {
        let dir = std::env::temp_dir().join(format!("shojiwm-default-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: tests in this module run one at a time on their own thread
        // before anything else reads the environment.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            std::env::set_var("WAYLAND_DISPLAY", "test-display");
        }
        let args = CommonArgs::parse(&[], &[]);
        let launcher = ConfigBuilder::new(setup)
            .asset_root(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/config"));
        let mut runtime = RuntimeBoot::new(Box::new(launcher), &args).launch(RuntimeHost::detached());
        runtime.preload().unwrap();
        runtime.enable().unwrap();
        runtime.sync_display_state(BTreeMap::from([("DP-1".to_owned(), output("DP-1", 0))]));
        runtime
    }

    /// Run timers and animations until nothing is pending.
    fn settle(runtime: &mut RuntimeHandle, now: &mut f64, ids: &[&str]) {
        for _ in 0..200 {
            *now += 16.0;
            let tick = runtime.scheduler_tick(*now).unwrap();
            for id in ids {
                runtime.evaluate_cached_window(id, None, *now as u64, false).unwrap();
            }
            if tick.next_poll_in_ms.is_none() {
                return;
            }
        }
    }

    #[test]
    fn floating_then_tiled_layout() {
        let mut runtime = start();
        let mut now = 1000.0;

        // A new window: initial configure (deferred while floating), then its
        // first commit centres it on the output.
        runtime.evaluate_window_preview(&window("w1", true), now as u64).unwrap();
        let evaluation = runtime.evaluate_window(&window("w1", true), now as u64).unwrap();
        let rect = evaluation.managed_window.rect.expect("managed rect");
        assert_eq!(rect.width, 800.0 + WINDOW_BORDER_PX * 2.0);
        assert_eq!(rect.x, (1920.0 - rect.width) / 2.0);
        assert!(
            evaluation
                .actions
                .iter()
                .any(|action| action.channel.is_none() && action.animation.is_some()),
            "the open animation rides along: {:?}",
            evaluation.actions
        );

        // Tile the workspace: the window becomes a tile in the viewport.
        let toggled = runtime.invoke_key_binding("toggle-tiling-mode", now as u64).unwrap();
        assert!(toggled.invoked);
        assert!(toggled.dirty_window_ids.contains(&"w1".to_owned()));
        let cached = runtime.evaluate_cached_window("w1", None, now as u64, false).unwrap();
        let tiled = cached.managed_window.rect.unwrap();
        assert_eq!(tiled.y, 12.0);
        assert_eq!(tiled.height, 1080.0 - 24.0);
        assert!(cached.managed_window.tiled);
        settle(&mut runtime, &mut now, &["w1"]);

        // A second window joins the row after the focused one.
        runtime.evaluate_window_preview(&window("w2", false), now as u64).unwrap();
        runtime.evaluate_window(&window("w2", false), now as u64).unwrap();
        settle(&mut runtime, &mut now, &["w1", "w2"]);
        let first = runtime.evaluate_cached_window("w1", None, now as u64, true).unwrap();
        let second = runtime.evaluate_cached_window("w2", None, now as u64, true).unwrap();
        let (first, second) = (first.managed_window.rect.unwrap(), second.managed_window.rect.unwrap());
        assert!(second.x > first.x, "{first:?} {second:?}");

        // Keyboard navigation and workspace switching run without trouble.
        for binding in ["tile-focus-left", "tile-focus-right", "tile-move-left", "workspace-next", "workspace-prev"] {
            assert!(runtime.invoke_key_binding(binding, now as u64).unwrap().invoked);
            settle(&mut runtime, &mut now, &["w1", "w2"]);
        }

        // Closing: the close animation's duration is declared.
        let close = runtime.start_close("w2", now as u64).unwrap();
        assert_eq!(close.close_animation_duration_ms, Some(500));
        settle(&mut runtime, &mut now, &["w1", "w2"]);
        runtime.window_closed("w2").unwrap();
        settle(&mut runtime, &mut now, &["w1"]);
    }

    #[test]
    fn decoration_reacts_to_focus() {
        let mut runtime = start();
        let focused = runtime.evaluate_window(&window("w1", true), 1).unwrap();
        let unfocused = runtime.evaluate_window(&window("w1", false), 2).unwrap();
        let border = |node: &shojiwm_rs::ssd::DecorationNode| node.style.border.unwrap().color;
        assert_eq!(border(&focused.node), hex("#d7ba7d"));
        assert_eq!(border(&unfocused.node), hex("#4f5666"));
    }

    #[test]
    fn title_button_tooltips_wait_for_a_resting_hover() {
        use shojiwm_rs::ssd::{DecorationNode, DecorationNodeKind};

        fn collect<'a>(node: &'a DecorationNode, out: &mut Vec<&'a DecorationNode>) {
            out.push(node);
            for child in &node.children {
                collect(child, out);
            }
        }
        fn popups(node: &DecorationNode) -> Vec<bool> {
            let mut nodes = Vec::new();
            collect(node, &mut nodes);
            nodes
                .iter()
                .filter_map(|node| match &node.kind {
                    DecorationNodeKind::Popup(popup) => Some(popup.open),
                    _ => None,
                })
                .collect()
        }
        let opened = |cached: &shojiwm_rs::ssd::DecorationCachedEvaluationResult| {
            cached
                .node_patches
                .iter()
                .filter_map(|patch| patch.replacement_node())
                .chain(cached.node.as_ref())
                .any(|node| popups(node).contains(&true))
        };

        let mut runtime = start();
        let evaluation = runtime.evaluate_window(&window("w1", true), 1000).unwrap();
        // Minimize and maximize carry a closed tooltip, close has none.
        assert_eq!(popups(&evaluation.node), [false, false]);
        let mut nodes = Vec::new();
        collect(&evaluation.node, &mut nodes);
        // The compositor reports the pointer on the minimize button as
        // interest in its tooltip.
        let minimize = nodes
            .iter()
            .find(|node| matches!(node.kind, DecorationNodeKind::Popup(_)))
            .and_then(|node| node.interaction.popup.as_ref()?.interest_change.clone())
            .expect("minimize tooltip interest handler");

        runtime.invoke_handler("w1", &minimize.true_handler, 1000).unwrap();
        for now in [1000, 1200, 1490] {
            runtime.scheduler_tick(now as f64).unwrap();
            let cached = runtime.evaluate_cached_window("w1", None, now, false).unwrap();
            assert!(!opened(&cached), "the tooltip opened early at {now} ms");
        }
        runtime.scheduler_tick(1500.0).unwrap();
        let cached = runtime.evaluate_cached_window("w1", None, 1500, false).unwrap();
        assert!(opened(&cached), "the tooltip opens after a resting hover");

        // Leaving closes it again.
        runtime.invoke_handler("w1", &minimize.false_handler, 1600).unwrap();
        let cached = runtime.evaluate_cached_window("w1", None, 1600, false).unwrap();
        assert!(!opened(&cached));
    }
}
