use smithay::utils::SERIAL_COUNTER;
use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output, wl_surface::WlSurface},
    },
    wayland::{
        compositor::{get_parent, with_states},
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
            WlrLayerShellHandler,
        },
    },
};
use tracing::{debug, info};

use crate::state::{PendingLayerSurface, ShojiWM};

fn layer_focus_debug_enabled() -> bool {
    std::env::var_os("SHOJI_LAYER_FOCUS_DEBUG").is_some()
}

fn layer_popup_root_debug_enabled() -> bool {
    std::env::var_os("SHOJI_LAYER_POPUP_ROOT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

impl WlrLayerShellHandler for ShojiWM {
    fn shell_state(&mut self) -> &mut smithay::wayland::shell::wlr_layer::WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer_kind: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| {
                let pos = self.seat.get_pointer()?.current_location();
                let pos_i = pos.to_i32_floor();
                self.space
                    .outputs()
                    .find(|output| {
                        self.space
                            .output_geometry(output)
                            .is_some_and(|geometry| geometry.contains(pos_i))
                    })
                    .cloned()
            })
            .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
        let layer = LayerSurface::new(surface, namespace);
        self.pending_layer_surfaces
            .push(PendingLayerSurface { output, layer });
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let destroyed = {
            self.space.outputs().find_map(|output| {
                let map = layer_map_for_output(output);
                let layer = map
                    .layers()
                    .find(|candidate| candidate.layer_surface() == &surface)
                    .cloned();
                layer.map(|layer| (output.clone(), layer))
            })
        };

        if let Some((output, layer)) = destroyed {
            self.mapped_on_demand_layer_surfaces
                .remove(&layer.wl_surface().id().protocol_id());
            if self.layer_shell_on_demand_focus.as_ref() == Some(&layer) {
                self.layer_shell_on_demand_focus = None;
            }
            let mut map = layer_map_for_output(&output);
            map.unmap_layer(&layer);
            drop(map);
            self.update_keyboard_focus(SERIAL_COUNTER.next_serial());
            self.schedule_redraw();
        } else if let Some(index) = self
            .pending_layer_surfaces
            .iter()
            .position(|pending| pending.layer.layer_surface() == &surface)
        {
            self.pending_layer_surfaces.swap_remove(index);
        }
    }
}

impl ShojiWM {
    pub(crate) fn close_layer_surfaces_for_output(&mut self, output: &Output) {
        let mapped_layers = {
            let map = layer_map_for_output(output);
            map.layers().cloned().collect::<Vec<_>>()
        };

        let pending_layers = std::mem::take(&mut self.pending_layer_surfaces);
        let mut retained_pending_layers = Vec::with_capacity(pending_layers.len());
        for pending in pending_layers {
            if &pending.output == output {
                pending.layer.layer_surface().send_close();
            } else {
                retained_pending_layers.push(pending);
            }
        }
        self.pending_layer_surfaces = retained_pending_layers;

        let mut focus_changed = false;
        for layer in &mapped_layers {
            self.mapped_on_demand_layer_surfaces
                .remove(&layer.wl_surface().id().protocol_id());
            if self.layer_shell_on_demand_focus.as_ref() == Some(layer) {
                self.layer_shell_on_demand_focus = None;
                focus_changed = true;
            }
            layer.layer_surface().send_close();
        }

        {
            let mut map = layer_map_for_output(output);
            for layer in &mapped_layers {
                map.unmap_layer(layer);
            }
        }

        if focus_changed {
            self.update_keyboard_focus(SERIAL_COUNTER.next_serial());
        }
    }
}

pub fn handle_commit(state: &mut ShojiWM, surface: &WlSurface) {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }

    let output = if let Some(index) = state
        .pending_layer_surfaces
        .iter()
        .position(|pending| pending.layer.wl_surface() == &root)
    {
        let pending = state.pending_layer_surfaces.swap_remove(index);
        let output = pending.output;
        {
            let mut map = layer_map_for_output(&output);
            map.map_layer(&pending.layer).unwrap();
        }
        output
    } else {
        let Some(output) = state
            .space
            .outputs()
            .find(|output| {
                let map = layer_map_for_output(output);
                map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
        else {
            return;
        };
        output
    };

    let initial_configure_sent = with_states(&root, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .initial_configure_sent
    });

    let mut map = layer_map_for_output(&output);
    map.arrange();

    if let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL) {
        if !initial_configure_sent {
            debug!(surface = ?surface.id(), "sending initial layer-shell configure");
            layer.layer_surface().send_configure();
        }

        let layer_geo = map.layer_geometry(layer);
        let output_loc = state
            .space
            .output_geometry(&output)
            .map(|geo| geo.loc)
            .unwrap_or_default();
        // `layer_geometry` returns coordinates relative to the output's layer
        // map (output-local). `window_source_damage`, the effect-rect we
        // intersect against in `source_damage_intersects_rect`, and every
        // other consumer of these rects operate in **global** logical space.
        // Translate by the output origin here so multi-monitor setups don't
        // silently drop backdrop invalidation on non-primary outputs (where
        // output_loc.x/y > 0 means the local-coord rect never intersects the
        // global effect rect).
        let layer_rect = layer_geo.map(|geo| {
            crate::ssd::LogicalRect::new(
                output_loc.x + geo.loc.x,
                output_loc.y + geo.loc.y,
                geo.size.w,
                geo.size.h,
            )
        });
        let owner = format!("{}", layer_surface_id(&root));
        if layer_popup_root_debug_enabled() {
            info!(
                root_surface_id = layer.wl_surface().id().protocol_id(),
                surface_id = surface.id().protocol_id(),
                initial_configure_sent,
                layer = ?layer.layer(),
                keyboard_interactivity = ?layer.cached_state().keyboard_interactivity,
                layer_geo = ?layer_geo,
                output_loc = ?output_loc,
                "layer popup root debug: layer commit"
            );
        }
        if std::env::var_os("SHOJI_SOURCE_DAMAGE_DEBUG").is_some()
            && let Some(geo) = layer_geo {
                debug!(
                    owner = %owner,
                    layer_geo_loc = ?geo.loc,
                    layer_geo_size = ?geo.size,
                    output_loc = ?output_loc,
                    global_loc_x = output_loc.x + geo.loc.x,
                    global_loc_y = output_loc.y + geo.loc.y,
                    "layer source damage stored (global coords)"
                );
            }
        match layer.layer() {
            Layer::Background | Layer::Bottom => {
                state.lower_layer_scene_generation =
                    state.lower_layer_scene_generation.wrapping_add(1);
                if let Some(rect) = layer_rect {
                    state
                        .lower_layer_source_damage
                        .push(crate::state::OwnedDamageRect { owner, rect });
                }
            }
            Layer::Top | Layer::Overlay => {
                state.upper_layer_scene_generation =
                    state.upper_layer_scene_generation.wrapping_add(1);
                if let Some(rect) = layer_rect {
                    state
                        .upper_layer_source_damage
                        .push(crate::state::OwnedDamageRect { owner, rect });
                }
            }
        }

        let surface_id = layer.wl_surface().id().protocol_id();
        let keyboard_interactivity = layer.cached_state().keyboard_interactivity;
        let is_mapped = layer_geo.is_some();
        let was_mapped = state.mapped_on_demand_layer_surfaces.contains(&surface_id);

        if matches!(
            keyboard_interactivity,
            KeyboardInteractivity::OnDemand | KeyboardInteractivity::Exclusive
        ) {
            if is_mapped && !was_mapped {
                if layer_focus_debug_enabled() {
                    debug!(
                        surface_id,
                        layer = ?layer.layer(),
                        ?keyboard_interactivity,
                        is_mapped,
                        was_mapped,
                        "auto focusing newly mapped keyboard-interactive layer"
                    );
                }
                state.mapped_on_demand_layer_surfaces.insert(surface_id);
                if matches!(keyboard_interactivity, KeyboardInteractivity::OnDemand)
                    && matches!(layer.layer(), Layer::Overlay | Layer::Top)
                {
                    state.layer_shell_on_demand_focus = Some(layer.clone());
                }
            } else if !is_mapped {
                state.mapped_on_demand_layer_surfaces.remove(&surface_id);
                if state.layer_shell_on_demand_focus.as_ref() == Some(layer) {
                    state.layer_shell_on_demand_focus = None;
                }
            }
        } else {
            state.mapped_on_demand_layer_surfaces.remove(&surface_id);
            if state.layer_shell_on_demand_focus.as_ref() == Some(layer) {
                state.layer_shell_on_demand_focus = None;
            }
        }
    }

    drop(map);
    state.update_keyboard_focus(SERIAL_COUNTER.next_serial());
    state.refresh_pointer_focus(std::time::Duration::from(state.clock.now()).as_millis() as u32);
    state.schedule_redraw();
}

fn layer_surface_id(surface: &WlSurface) -> u32 {
    surface.id().protocol_id()
}
