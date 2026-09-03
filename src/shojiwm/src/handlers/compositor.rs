use crate::{
    grabs::resize_grab,
    handlers::{layer_shell, xdg_shell},
    state::{ClientState, CursorOverrideApplied, ShojiWM},
};
use calloop::Interest;
use smithay::{
    backend::renderer::{
        buffer_dimensions,
        utils::{RendererSurfaceStateUserData, on_commit_buffer_handler},
    },
    reexports::wayland_server::{
        Client, Resource,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    utils::{Logical, Rectangle, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        commit_timing::CommitTimerStateUserData,
        compositor::{
            BufferAssignment, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, SurfaceData, TraversalAction, add_blocker, add_pre_commit_hook,
            get_parent, is_sync_subsurface, with_states, with_surface_tree_upward,
        },
        dmabuf::get_dmabuf,
        shell::xdg::SurfaceCachedState,
        shm::{ShmHandler, ShmState},
        viewporter::ViewportCachedState,
    },
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tracing::{debug, info, trace};

fn commit_rate_debug_enabled() -> bool {
    std::env::var_os("SHOJI_COMMIT_RATE_DEBUG").is_some()
}

fn mpv_frame_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MPV_FRAME_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn frame_liveness_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_LIVENESS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn browser_geometry_debug_enabled() -> bool {
    std::env::var_os("SHOJI_BROWSER_GEOMETRY_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn x11_browser_cpu_debug_enabled() -> bool {
    std::env::var_os("SHOJI_X11_BROWSER_CPU_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

/// Resolution of `wl_fixed`, the wire type every `wp_viewport.set_source` value is rounded to.
const WL_FIXED_QUANTUM: f64 = 1.0 / 256.0;

/// Snap a viewport source rectangle that overshoots the buffer by at most one `wl_fixed`
/// quantum back inside it. `None` means the rectangle is either already valid or too far out
/// to be a rounding artefact; in the latter case smithay posts `out_of_buffer` as the
/// protocol demands.
///
/// `wl_fixed_from_double` rounds x, y, w and h independently, each by up to half a quantum,
/// so a far edge the client computed as exactly the buffer edge arrives one quantum past it
/// whenever both terms are half-way ties. Firefox's video layers hit this (Mozilla bug
/// 2062135, still open in 157.0a1): `y=881.33984375 + h=198.6640625 = 1080 + 1/256`, and
/// Firefox deliberately crashes on any protocol error. A near edge below zero or a size
/// larger than the buffer cannot come from rounding a valid rectangle, so those stay errors.
/// The near edge is moved rather than the size shrunk: an integer size stays integer, and
/// smithay raises `bad_size` for a fractional source size without a destination size.
fn snap_viewport_source_overshoot(
    src: Rectangle<f64, Logical>,
    bounds: Size<i32, Logical>,
) -> Option<Rectangle<f64, Logical>> {
    fn snap_axis(loc: f64, size: f64, bound: f64) -> Option<f64> {
        // Every operand is a dyadic rational (a multiple of 1/256 or an integer), so this
        // arithmetic is exact in f64 and the comparisons need no epsilon.
        let overshoot = loc + size - bound;
        let snapped = loc - overshoot;
        (overshoot > 0.0 && overshoot <= WL_FIXED_QUANTUM && snapped >= 0.0).then_some(snapped)
    }

    let bounds = bounds.to_f64();
    let x = snap_axis(src.loc.x, src.size.w, bounds.w);
    let y = snap_axis(src.loc.y, src.size.h, bounds.h);
    if x.is_none() && y.is_none() {
        return None;
    }
    let mut snapped = src;
    snapped.loc.x = x.unwrap_or(src.loc.x);
    snapped.loc.y = y.unwrap_or(src.loc.y);
    Some(snapped)
}

/// Runs from `commit`, immediately before `on_commit_buffer_handler` validates every surface
/// in the tree: pull a committed `wp_viewport` source rectangle that overshoots its buffer by
/// a single `wl_fixed` quantum back inside it.
///
/// Deliberately not a pre-commit hook. The `out_of_buffer` check lives in
/// `SurfaceView::from_states`, reached from `on_commit_buffer_handler` on the *current* state
/// once the transaction has applied, so the only state guaranteed to match what it validates
/// is the current state read at that same point. A hook on the pending state would run
/// before queued transactions land (dmabuf fence blockers, synchronized subsurfaces), and a
/// synchronized child's state is committed again from its parent's commit without the
/// child's hooks running at all. Firefox's video layers are synchronized subsurfaces.
fn snap_committed_viewport_sources(surface: &WlSurface) {
    // The same traversal `on_commit_buffer_handler` is about to make.
    if is_sync_subsurface(surface) {
        return;
    }
    with_surface_tree_upward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |surface, states, _| snap_current_viewport_source(surface, states),
        |_, _, _| true,
    );
}

/// Snap one surface's committed viewport source. Takes the `SurfaceData` the traversal hands
/// out rather than calling `with_states`, which would re-lock the surface the traversal
/// already holds.
fn snap_current_viewport_source(surface: &WlSurface, states: &SurfaceData) {
    let src = {
        let mut viewport_cache = states.cached_state.get::<ViewportCachedState>();
        viewport_cache.current().src
    };
    let Some(src) = src else {
        return;
    };

    // The validator compares against the logical size of the buffer this commit leaves
    // attached, derived exactly as `RendererSurfaceState::update_buffer` derives it: a newly
    // attached buffer under the scale and transform committed with it, otherwise the size
    // the renderer state already holds, whose scale and transform were fixed when that
    // buffer was attached.
    let attached = {
        let mut attrs_cache = states.cached_state.get::<SurfaceAttributes>();
        let attrs = attrs_cache.current();
        match &attrs.buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                Some(buffer_dimensions(buffer).map(|dims| {
                    dims.to_logical(attrs.buffer_scale, Transform::from(attrs.buffer_transform))
                }))
            }
            Some(BufferAssignment::Removed) => Some(None),
            None => None,
        }
    };
    let bounds = match attached {
        Some(bounds) => bounds,
        None => states
            .data_map
            .get::<RendererSurfaceStateUserData>()
            .and_then(|state| state.lock().unwrap().buffer_size()),
    };
    let Some(bounds) = bounds else {
        return;
    };

    if let Some(snapped) = snap_viewport_source_overshoot(src, bounds) {
        info!(
            surface = ?surface.id(),
            ?src,
            ?snapped,
            ?bounds,
            "viewport source overshoots the buffer by a wl_fixed rounding quantum, snapped"
        );
        let mut viewport_cache = states.cached_state.get::<ViewportCachedState>();
        viewport_cache.current().src = Some(snapped);
        // The pending state keeps the client's last `set_source` until it sends another, and
        // every later commit would re-apply the same overshoot: carry the correction over so
        // the snap (and this log line) happens once per rectangle, not once per frame. Leave
        // it alone if the client has already queued a different rectangle.
        let pending = viewport_cache.pending();
        if pending.src == Some(src) {
            pending.src = Some(snapped);
        }
    }
}

fn is_chrome_like_app_id(app_id: Option<&str>) -> bool {
    app_id.is_some_and(|app_id| {
        let app_id = app_id.to_ascii_lowercase();
        app_id == "google-chrome" || app_id.contains("chromium") || app_id.contains("chrome")
    })
}

fn previous_transform_snapshot_source_damage_time(
    window_id: &str,
    now: Duration,
) -> Option<Duration> {
    static TIMES: OnceLock<Mutex<HashMap<String, Duration>>> = OnceLock::new();
    let map = TIMES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap();
    guard.insert(window_id.to_string(), now)
}

impl CompositorHandler for ShojiWM {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        if let Some(data) = client.get_data::<smithay::xwayland::XWaylandClientData>() {
            return &data.compositor_state;
        }
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        trace!(surface = ?surface.id(), "wl_surface commit received");
        // Niri-style per-commit redraw gating.
        //
        // Previously this handler ended with an unconditional `self.schedule_redraw()` and also
        // called `self.request_tty_maintenance("surface-commit")` at the top. That combination
        // drove the historical Firefox high-CPU loop: every subsurface / non-presented root
        // surface commit woke a full redraw + pre-render maintenance pass, which flushed
        // wayland traffic back to the client, which committed again, ...
        //
        // Instead we now follow niri's `queue_redraw(&output)` discipline: only schedule a
        // redraw when the commit actually affects something that is rendered (a mapped
        // toplevel / X11 window via `pending_source_damage`, a popup via `xdg_shell::handle_commit`,
        // or a layer surface via `layer_shell::handle_commit`). Maintenance (`space.refresh()` /
        // popup cleanup / `flush_clients`) still runs every event-loop iteration — that part is
        // handled in `backend::run_tty_udev` and does not depend on per-commit requests — so
        // popup-heavy clients like the noctalia shell right-click menu still appear immediately.
        self.scene_generation = self.scene_generation.wrapping_add(1);
        let mut pending_source_damage: Option<(
            smithay::desktop::Window,
            Vec<crate::ssd::LogicalRect>,
        )> = None;
        let mut cursor_surface_committed = false;
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let mapped_window = self
                .space
                .elements()
                .find(|w| {
                    w.toplevel().is_some_and(|t| t.wl_surface() == &root)
                        || w.x11_surface().and_then(|x11| x11.wl_surface()).as_ref() == Some(&root)
                })
                .cloned();
            if let Some(window) = mapped_window.as_ref() {
                pending_source_damage = Some((
                    window.clone(),
                    self.logical_source_damage_rects_for_surface(window, surface),
                ));
            } else if matches!(
                &self.cursor_status,
                smithay::input::pointer::CursorImageStatus::Surface(cursor_surface)
                    if cursor_surface == &root
            ) {
                // A cursor-role surface updated. This path is not reached through layer-shell
                // / xdg-shell / mapped-window tracking, so we must schedule the redraw here
                // (niri does the equivalent via its own cursor-surface branch).
                cursor_surface_committed = true;
                let cursor_size = self.cursor_theme.size() as i32;
                if surface == &root {
                    with_states(surface, |states| {
                        // Apply the role-specific buffer offset to the hotspot so the cursor
                        // stays anchored when the client attaches a buffer at a non-zero (x, y).
                        let buffer_delta = states
                            .cached_state
                            .get::<SurfaceAttributes>()
                            .current()
                            .buffer_delta
                            .take();
                        if let Some(buffer_delta) = buffer_delta
                            && let Some(attrs) = states
                                .data_map
                                .get::<Mutex<smithay::input::pointer::CursorImageAttributes>>()
                            {
                                attrs.lock().unwrap().hotspot -= buffer_delta;
                            }

                        // Workaround for Xwayland (via xwayland-satellite) sending oversized
                        // cursor buffers without setting buffer_scale: it attaches a 48×48
                        // Adwaita buffer and never calls set_buffer_scale(2), resulting in a
                        // logical 48-px cursor that renders as 72 physical px on a 1.5×
                        // output. xwayland-satellite does not viewport-correct cursor
                        // surfaces (only toplevels), so we patch buffer_scale here on
                        // commit. WaylandSurfaceRenderElement::geometry() then uses the
                        // corrected view.dst at render time.
                        let buffer_dims = match &states
                            .cached_state
                            .get::<SurfaceAttributes>()
                            .current()
                            .buffer
                        {
                            Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(
                                buffer,
                            )) => smithay::backend::renderer::buffer_dimensions(buffer),
                            _ => None,
                        };
                        if let Some(dims) = buffer_dims {
                            let max_dim = dims.w.max(dims.h);
                            if cursor_size > 0 && max_dim > cursor_size {
                                let mut attrs_cache =
                                    states.cached_state.get::<SurfaceAttributes>();
                                let attrs = attrs_cache.current();
                                if attrs.buffer_scale == 1 {
                                    let factor = ((max_dim as f64 / cursor_size as f64).round()
                                        as i32)
                                        .max(2);
                                    attrs.buffer_scale = factor;

                                    // Hotspot reinterpretation: divide by factor exactly once
                                    // per set_cursor cycle. CursorOverrideApplied tracks this
                                    // and is reset in `cursor_image()` whenever a new cursor
                                    // surface is set.
                                    states.data_map.insert_if_missing_threadsafe(|| {
                                        Mutex::new(CursorOverrideApplied::default())
                                    });
                                    let mut applied = states
                                        .data_map
                                        .get::<Mutex<CursorOverrideApplied>>()
                                        .unwrap()
                                        .lock()
                                        .unwrap();
                                    if !applied.applied {
                                        applied.applied = true;
                                        if let Some(cursor_attrs) =
                                            states
                                                .data_map
                                                .get::<Mutex<
                                                    smithay::input::pointer::CursorImageAttributes,
                                                >>()
                                        {
                                            let mut hot = cursor_attrs.lock().unwrap();
                                            hot.hotspot.x /= factor;
                                            hot.hotspot.y /= factor;
                                        }
                                    }
                                }
                            }
                        }
                    });
                }
            }
            if x11_browser_cpu_debug_enabled()
                && let Some(window) = mapped_window {
                    let snapshot = self.snapshot_window(&window);
                    if is_chrome_like_app_id(snapshot.app_id.as_deref()) {
                        let (buffer_attached, damage_count, frame_callback_count) = with_states(
                            surface,
                            |states| {
                                let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                                let attrs = attrs.current();
                                (
                                    matches!(
                                        attrs.buffer.as_ref(),
                                        Some(
                                            smithay::wayland::compositor::BufferAssignment::NewBuffer(
                                                _
                                            )
                                        )
                                    ),
                                    attrs.damage.len(),
                                    attrs.frame_callbacks.len(),
                                )
                            },
                        );
                        info!(
                            window_id = %snapshot.id,
                            title = %snapshot.title,
                            app_id = ?snapshot.app_id,
                            is_xwayland = snapshot.is_xwayland,
                            surface_id = ?surface.id(),
                            root_surface_id = ?root.id(),
                            committed_surface_is_root = surface == &root,
                            buffer_attached,
                            damage_count,
                            frame_callback_count,
                            "x11 browser cpu: surface commit",
                        );
                    }
                }
        }
        snap_committed_viewport_sources(surface);
        on_commit_buffer_handler::<Self>(surface);
        if let Some((window, source_damage)) = pending_source_damage {
            self.window_scene_generation = self.window_scene_generation.wrapping_add(1);
            window.on_commit();
            // Title / app_id may have changed via xdg_toplevel set_title /
            // set_app_id between commits. sync_foreign_toplevel short-circuits
            // when nothing changed so this is cheap.
            self.sync_foreign_toplevel(&window);
            let snapshot = self.snapshot_window(&window);
            if browser_geometry_debug_enabled()
                && matches!(
                    snapshot.app_id.as_deref(),
                    Some("google-chrome") | Some("firefox")
                )
            {
                let (surface_geometry, attrs) = with_states(surface, |states| {
                    let geometry = states
                        .cached_state
                        .get::<SurfaceCachedState>()
                        .current()
                        .geometry;
                    let mut attrs_cache = states.cached_state.get::<SurfaceAttributes>();
                    let attrs = attrs_cache.current();
                    (
                        geometry,
                        (
                            attrs.buffer_delta,
                            attrs.buffer_scale,
                            attrs.damage.len(),
                            attrs.opaque_region.is_some(),
                            attrs.input_region.is_some(),
                        ),
                    )
                });
                info!(
                    window_id = %snapshot.id,
                    title = %snapshot.title,
                    app_id = ?snapshot.app_id,
                    surface_id = ?surface.id(),
                    surface_geometry = ?surface_geometry,
                    buffer_delta = ?attrs.0,
                    buffer_scale = attrs.1,
                    damage_count = attrs.2,
                    has_opaque_region = attrs.3,
                    has_input_region = attrs.4,
                    source_damage_count = source_damage.len(),
                    "browser geometry: root surface commit",
                );
            }
            if frame_liveness_debug_enabled() {
                info!(
                    window_id = %snapshot.id,
                    title = %snapshot.title,
                    app_id = ?snapshot.app_id,
                    source_damage_count = source_damage.len(),
                    "frame liveness: window commit observed",
                );
            }
            let commit_time = std::time::Duration::from(self.clock.now());
            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                let previous_commit_time =
                    previous_transform_snapshot_source_damage_time(&snapshot.id, commit_time);
                let delta_ms = previous_commit_time
                    .and_then(|previous| commit_time.checked_sub(previous))
                    .map(|delta| delta.as_secs_f64() * 1000.0);
                tracing::info!(
                    window_id = %snapshot.id,
                    commit_time = ?commit_time,
                    previous_commit_time = ?previous_commit_time,
                    delta_ms = ?delta_ms,
                    source_damage = ?source_damage,
                    source_damage_count = source_damage.len(),
                    "transform snapshot compositor source damage"
                );
            }
            if commit_rate_debug_enabled() {
                let delta_ms = self
                    .window_commit_times
                    .get(&window)
                    .and_then(|prev| commit_time.checked_sub(*prev))
                    .map(|d| d.as_secs_f64() * 1000.0);
                info!(
                    window_id = %snapshot.id,
                    title = ?snapshot.title,
                    app_id = ?snapshot.app_id,
                    delta_ms = ?delta_ms,
                    "commit rate debug"
                );
            }
            if mpv_frame_debug_enabled() && snapshot.app_id.as_deref() == Some("mpv") {
                let delta_ms = self
                    .window_commit_times
                    .get(&window)
                    .and_then(|prev| commit_time.checked_sub(*prev))
                    .map(|d| d.as_secs_f64() * 1000.0);
                info!(
                    window_id = %snapshot.id,
                    surface = ?surface.id(),
                    commit_time_ms = commit_time.as_secs_f64() * 1000.0,
                    delta_ms,
                    source_damage_count = source_damage.len(),
                    needs_redraw_before = self.needs_redraw,
                    window_source_damage_pending_before = self.window_source_damage.len(),
                    pending_decoration_damage_before = self.pending_decoration_damage.len(),
                    "mpv frame debug: commit"
                );
            }
            self.window_commit_times.insert(window.clone(), commit_time);
            if self.window_allows_render(&window) {
                self.snapshot_dirty_window_ids.insert(snapshot.id.clone());
                self.window_source_damage
                    .extend(
                        source_damage
                            .into_iter()
                            .map(|rect| crate::state::OwnedDamageRect {
                                owner: snapshot.id.clone(),
                                rect,
                            }),
                    );
                if let Some(decoration) = self.window_decorations.get(&window) {
                    self.pending_decoration_damage
                        .push(decoration.layout.root.rect);
                }
                if let Some(top) = window.toplevel() {
                    debug!(surface = ?top.wl_surface().id(), "toplevel commit matched mapped window");
                }
                // This commit touched a rendered mapped toplevel / X11 window. Queue a redraw.
                // Idle ManagedWindows still accept commits so their latest buffer is ready on
                // restore, but they intentionally don't wake rendering or source-damage effects.
                self.schedule_redraw();
            } else if frame_liveness_debug_enabled() {
                info!(
                    window_id = %snapshot.id,
                    title = %snapshot.title,
                    app_id = ?snapshot.app_id,
                    "frame liveness: idle window commit ignored for redraw",
                );
            }
        }

        // `xdg_shell::handle_commit` schedules its own redraw for popup commits (both the
        // tracked `PopupKind::Xdg` path and the "untracked xdg_popup role" fallback), so we
        // don't need to force one here. Likewise `layer_shell::handle_commit` calls
        // `schedule_redraw` whenever it recognises the commit as targeting a mapped layer
        // surface. Commits that are neither mapped-window / popup / layer (e.g. bare root
        // surfaces without any render element, orphan subsurfaces) deliberately produce no
        // redraw request.
        xdg_shell::handle_commit(self, surface);
        layer_shell::handle_commit(self, surface);
        resize_grab::handle_commit(&mut self.space, surface);

        if !self.idle_inhibited_surfaces.is_empty() {
            self.refresh_idle_inhibit_state();
        }

        if cursor_surface_committed || self.is_session_lock_surface_tree_surface(surface) {
            self.schedule_redraw();
        }
    }

    fn destroyed(&mut self, _surface: &WlSurface) {
        self.refresh_idle_inhibit_state();
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // On Intel/AMD, the kernel's dma-resv fences implicitly protect us, but the
        // NVIDIA proprietary driver provides no such safety net. Without it, the
        // compositor ends up sampling VRAM that is still being written to at the
        // exact moment of "input -> damage -> client redraw", which shows up as
        // visual noise. To prevent this, we explicitly wait for the dmabuf's fences
        // to signal when a commit occurs (by blocking the commit until then).
        add_pre_commit_hook::<Self, _>(surface, move |state, _dh, surface| {
            let maybe_dmabuf = with_states(surface, |data| {
                data.cached_state
                    .get::<SurfaceAttributes>()
                    .pending()
                    .buffer
                    .as_ref()
                    .and_then(|assignment| match assignment {
                        BufferAssignment::NewBuffer(buffer) => get_dmabuf(buffer).cloned().ok(),
                        _ => None,
                    })
            });
            if let Some(dmabuf) = maybe_dmabuf
                && let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ)
                && let Some(client) = surface.client()
            {
                let res = state.loop_handle.insert_source(source, move |_, _, state| {
                    let dh = state.display_handle.clone();
                    state
                        .client_compositor_state(&client)
                        .blocker_cleared(state, &dh);
                    Ok(())
                });
                if res.is_ok() {
                    add_blocker(surface, blocker);
                }
            }

            // This commit carries a wp_commit_timer timestamp (smithay's commit-timing
            // hook runs after this one and turns it into a barrier blocker). If the
            // outputs are already idle, nothing else re-examines commit-timing deadlines
            // until the next `frame_finish`, which never comes because the blocked commit
            // holds the only pending damage — so queue a timer re-arm for after this
            // dispatch, once the barrier is registered.
            let has_commit_timer_timestamp = with_states(surface, |states| {
                states
                    .data_map
                    .get::<CommitTimerStateUserData>()
                    .is_some_and(|timer| timer.borrow().timestamp.is_some())
            });
            if has_commit_timer_timestamp {
                state.loop_handle.insert_idle(|state| {
                    let loop_handle = state.loop_handle.clone();
                    crate::backend::tty::arm_commit_timing_timers(state, &loop_handle);
                });
            }
        });
    }
}

impl BufferHandler for ShojiWM {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for ShojiWM {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rectangle<f64, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn bounds(w: i32, h: i32) -> Size<i32, Logical> {
        (w, h).into()
    }

    /// Mirrors the check in smithay's `ensure_viewport_valid`.
    fn validator_accepts(src: Rectangle<f64, Logical>, bounds: Size<i32, Logical>) -> bool {
        Rectangle::from_size(bounds.to_f64()).contains_rect(src)
    }

    #[test]
    fn snaps_the_1080p_strip_that_killed_firefox_on_2_9_2026() {
        // wp_viewport#181: x=0,y=881.33984375,w=1920,h=198.6640625 vs 1920x1080
        let src = rect(0.0, 881.33984375, 1920.0, 198.6640625);
        assert!(!validator_accepts(src, bounds(1920, 1080)));
        let snapped = snap_viewport_source_overshoot(src, bounds(1920, 1080)).unwrap();
        assert_eq!(snapped.loc.y, 881.3359375);
        assert_eq!(snapped.loc.y + snapped.size.h, 1080.0);
        assert_eq!(snapped.loc.x, 0.0);
        assert_eq!(snapped.size, src.size);
        assert!(validator_accepts(snapped, bounds(1920, 1080)));
    }

    #[test]
    fn snaps_the_720p_strip_that_killed_firefox_on_1_9_2026() {
        // wp_viewport#579: x=0,y=183.44140625,w=1280,h=536.5625 vs 1280x720
        let src = rect(0.0, 183.44140625, 1280.0, 536.5625);
        assert!(!validator_accepts(src, bounds(1280, 720)));
        let snapped = snap_viewport_source_overshoot(src, bounds(1280, 720)).unwrap();
        assert_eq!(snapped.loc.y, 183.4375);
        assert_eq!(snapped.loc.y + snapped.size.h, 720.0);
        assert!(validator_accepts(snapped, bounds(1280, 720)));
    }

    #[test]
    fn snaps_both_axes_independently() {
        // x is a 1/256 tie against an integer width, y is the recorded strip.
        let src = rect(0.00390625, 881.33984375, 1920.0, 198.6640625);
        let snapped = snap_viewport_source_overshoot(src, bounds(1920, 1080)).unwrap();
        assert_eq!(snapped.loc.x, 0.0);
        assert_eq!(snapped.loc.y, 881.3359375);
        assert!(validator_accepts(snapped, bounds(1920, 1080)));
    }

    #[test]
    fn leaves_valid_rectangles_alone() {
        assert!(
            snap_viewport_source_overshoot(rect(0.0, 880.0, 1920.0, 200.0), bounds(1920, 1080))
                .is_none()
        );
        assert!(
            snap_viewport_source_overshoot(rect(0.0, 0.0, 1920.0, 1080.0), bounds(1920, 1080))
                .is_none()
        );
        assert!(
            snap_viewport_source_overshoot(rect(10.5, 20.25, 100.0, 50.75), bounds(1920, 1080))
                .is_none()
        );
    }

    #[test]
    fn leaves_real_overshoots_to_the_validator() {
        // Two quanta over: not a rounding tie.
        let src = rect(0.0, 881.34375, 1920.0, 198.6640625);
        assert!(snap_viewport_source_overshoot(src, bounds(1920, 1080)).is_none());
        // A full pixel over.
        let src = rect(0.0, 881.0, 1920.0, 200.0);
        assert!(snap_viewport_source_overshoot(src, bounds(1920, 1080)).is_none());
    }

    #[test]
    fn does_not_push_the_near_edge_negative() {
        // A size larger than the buffer cannot come from rounding a valid rectangle.
        let src = rect(0.0, 0.0, 1920.0, 1080.00390625);
        assert!(snap_viewport_source_overshoot(src, bounds(1920, 1080)).is_none());
        // Nor can a negative origin.
        let src = rect(0.0, -0.00390625, 1920.0, 1080.0);
        assert!(snap_viewport_source_overshoot(src, bounds(1920, 1080)).is_none());
    }

    #[test]
    fn honours_scaled_bounds() {
        // A 3840x2160 buffer at scale 2 validates against 1920x1080 logical.
        let logical: Size<i32, Logical> = Size::<i32, smithay::utils::Buffer>::from((3840, 2160))
            .to_logical(2, Transform::Normal);
        assert_eq!(logical, bounds(1920, 1080));
        let src = rect(0.0, 881.33984375, 1920.0, 198.6640625);
        assert!(snap_viewport_source_overshoot(src, logical).is_some());
    }
}
