use crate::{
    grabs::resize_grab,
    handlers::{layer_shell, xdg_shell},
    state::{ClientState, ShojiWM},
};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_shm,
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource,
        backend::ClientId,
        delegate_dispatch, delegate_global_dispatch,
        protocol::{
            wl_buffer,
            wl_callback::WlCallback,
            wl_compositor::WlCompositor,
            wl_region::{self, WlRegion},
            wl_subcompositor::WlSubcompositor,
            wl_subsurface::WlSubsurface,
            wl_surface::WlSurface,
        },
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, RegionUserData,
            SubsurfaceUserData, SurfaceAttributes, SurfaceUserData, get_parent, is_sync_subsurface,
            with_states,
        },
        shm::{ShmHandler, ShmState},
        shell::xdg::SurfaceCachedState,
    },
};
use tracing::{debug, info, trace};

fn commit_rate_debug_enabled() -> bool {
    std::env::var_os("SHOJI_COMMIT_RATE_DEBUG").is_some()
}

fn frame_liveness_debug_enabled() -> bool {
    std::env::var_os("SHOJI_FRAME_LIVENESS_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn browser_geometry_debug_enabled() -> bool {
    std::env::var_os("SHOJI_BROWSER_GEOMETRY_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
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
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        trace!(surface = ?surface.id(), "wl_surface commit received");
        // A committed surface can be observed from two different paths:
        //
        // 1. directly while dispatching Wayland client requests from the display fd
        // 2. indirectly when Smithay replays a previously blocked commit after
        //    `blocker_cleared()` (for example from commit-timing / FIFO barriers)
        //
        // The TTY backend needs `space.refresh()/popups.cleanup()` before the next render after
        // either case. If we only request maintenance from the display-fd dispatch path, commits
        // coming from blocker replay can schedule a redraw without scheduling the pre-render
        // maintenance pass, which manifests as "the client updated, but the new contents do not
        // become visible until some unrelated input event causes another refresh".
        self.request_tty_maintenance("surface-commit");
        self.scene_generation = self.scene_generation.wrapping_add(1);
        let mut pending_source_damage: Option<(
            smithay::desktop::Window,
            Vec<crate::ssd::LogicalRect>,
        )> = None;
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == &root)
            {
                pending_source_damage = Some((
                    window.clone(),
                    self.logical_source_damage_rects_for_surface(window, surface),
                ));
            }
        }
        on_commit_buffer_handler::<Self>(surface);
        if let Some((window, source_damage)) = pending_source_damage {
            self.window_scene_generation = self.window_scene_generation.wrapping_add(1);
            window.on_commit();
            let snapshot = self.snapshot_window(&window);
            if browser_geometry_debug_enabled()
                && matches!(
                    snapshot.app_id.as_deref(),
                    Some("google-chrome") | Some("firefox")
                )
            {
                let (surface_geometry, attrs) = with_states(surface, |states| {
                    let geometry = states.cached_state.get::<SurfaceCachedState>().current().geometry;
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
            self.window_commit_times.insert(window.clone(), commit_time);
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
            debug!(surface = ?window.toplevel().unwrap().wl_surface().id(), "toplevel commit matched mapped window");
        }

        xdg_shell::handle_commit(&mut self.popups, &self.space, surface);
        layer_shell::handle_commit(self, surface);
        resize_grab::handle_commit(&mut self.space, surface);

        self.schedule_redraw();
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

// delegate_compositor!(ShojiWM) is intentionally expanded by hand here instead of using the
// macro directly. The reason is that we need to intercept wl_region requests before they reach
// Smithay's handler: Smithay's Size::new contains a debug_assert that panics when width or height
// is negative, but Firefox (and potentially other clients) sends wl_region rectangles with
// negative dimensions (e.g. height = -1) in certain situations such as moving a window to a
// different monitor. By handling WlRegion ourselves and filtering out invalid rectangles before
// forwarding to CompositorState, we avoid the panic without touching the Smithay source.
//
// If delegate_compositor! gains new delegations in a future Smithay update, the individual
// delegate_dispatch!/delegate_global_dispatch! lines below must be updated to match.
delegate_global_dispatch!(ShojiWM: [WlCompositor: ()] => CompositorState);
delegate_global_dispatch!(ShojiWM: [WlSubcompositor: ()] => CompositorState);
delegate_dispatch!(ShojiWM: [WlCompositor: ()] => CompositorState);
delegate_dispatch!(ShojiWM: [WlSurface: SurfaceUserData] => CompositorState);
delegate_dispatch!(ShojiWM: [WlCallback: ()] => CompositorState);
delegate_dispatch!(ShojiWM: [WlSubcompositor: ()] => CompositorState);
delegate_dispatch!(ShojiWM: [WlSubsurface: SubsurfaceUserData] => CompositorState);
impl Dispatch<WlRegion, RegionUserData> for ShojiWM {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &WlRegion,
        request: wl_region::Request,
        data: &RegionUserData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let skip = match &request {
            wl_region::Request::Add { width, height, .. }
            | wl_region::Request::Subtract { width, height, .. } => {
                if *width < 0 || *height < 0 {
                    tracing::debug!(
                        width,
                        height,
                        "ignoring wl_region rect with negative dimensions"
                    );
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !skip {
            <CompositorState as Dispatch<WlRegion, RegionUserData, Self>>::request(
                state, client, resource, request, data, dhandle, data_init,
            );
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        resource: &WlRegion,
        data: &RegionUserData,
    ) {
        <CompositorState as Dispatch<WlRegion, RegionUserData, Self>>::destroyed(
            state, client, resource, data,
        );
    }
}

delegate_shm!(ShojiWM);
