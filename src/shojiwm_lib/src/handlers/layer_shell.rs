use smithay::utils::SERIAL_COUNTER;
use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output, wl_surface::WlSurface},
    },
    backend::renderer::buffer_dimensions,
    utils::{Buffer as BufferCoord, Logical, Rectangle, Size},
    wayland::{
        compositor::{
            BufferAssignment, Damage, SurfaceAttributes, get_children, get_parent, with_states,
        },
        viewporter::ViewportCachedState,
        shell::wlr_layer::{
            KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
            WlrLayerShellHandler,
        },
    },
};
use tracing::{debug, info, warn};

use crate::state::{PendingLayerSurface, ShojiWM};

fn layer_focus_debug_enabled() -> bool {
    crate::env_flag!("SHOJI_LAYER_FOCUS_DEBUG")
}

fn layer_popup_root_debug_enabled() -> bool {
    std::env::var_os("SHOJI_LAYER_POPUP_ROOT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

impl WlrLayerShellHandler for ShojiWM {
    fn shell_state(&mut self) -> &mut smithay::wayland::shell::wlr_layer::WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_popup(
        &mut self,
        _parent: WlrLayerSurface,
        popup: smithay::wayland::shell::xdg::PopupSurface,
    ) {
        // `xdg_surface.get_popup` arrived with a null parent, so the xdg-shell
        // `new_popup` could not place it; the panel is only known now.
        self.unconstrain_popup(&popup);
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer_kind: Layer,
        namespace: String,
    ) {
        // A client may still hold the wl_output of a disconnected monitor; that
        // Output is out of the space and would never render or close the layer.
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|output| self.space.outputs().any(|candidate| candidate == output))
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
            .or_else(|| self.space.outputs().next().cloned());
        // With every monitor disconnected (e.g. a DP monitor entering power
        // saving), clients re-create the layers closed on disconnect right
        // away. There is nowhere to place them, so close them again; clients
        // create new ones when an output global appears.
        let Some(output) = output else {
            warn!(%namespace, "no output for new layer surface; closing it");
            surface.send_close();
            return;
        };
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
            let layer_id = crate::ssd::layer_runtime_id(&layer);
            self.layer_source_rects.remove(&layer_id);
            crate::backend::shader_effect::purge_backdrop_cache_for_layer(
                &mut self.layer_backdrop_cache,
                &layer_id,
            );
            let framebuffer_prefix = format!("{layer_id}@");
            self.layer_framebuffer_effect_states
                .retain(|key, _| !key.starts_with(&framebuffer_prefix));
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
            // The output is going away, so these layers never render here again.
            let layer_id = crate::ssd::layer_runtime_id(layer);
            crate::backend::shader_effect::purge_backdrop_cache_for_layer(
                &mut self.layer_backdrop_cache,
                &layer_id,
            );
            let framebuffer_prefix = format!("{layer_id}@");
            self.layer_framebuffer_effect_states
                .retain(|key, _| !key.starts_with(&framebuffer_prefix));
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

pub fn handle_commit(state: &mut ShojiWM, surface: &WlSurface, damage: LayerCommitDamage) {
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
        if crate::env_flag!("SHOJI_SOURCE_DAMAGE_DEBUG")
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
        let runtime_id = crate::ssd::layer_runtime_id(layer);
        let previous_rect = match layer_rect {
            Some(rect) => state.layer_source_rects.insert(runtime_id, rect),
            None => state.layer_source_rects.remove(&runtime_id),
        };
        let damage_rects = layer_source_damage_rects(&damage, previous_rect, layer_rect);
        let source_damage = match layer.layer() {
            Layer::Background | Layer::Bottom => {
                state.lower_layer_scene_generation =
                    state.lower_layer_scene_generation.wrapping_add(1);
                &mut state.lower_layer_source_damage
            }
            Layer::Top | Layer::Overlay => {
                state.upper_layer_scene_generation =
                    state.upper_layer_scene_generation.wrapping_add(1);
                &mut state.upper_layer_source_damage
            }
        };
        source_damage.extend(damage_rects.into_iter().map(|rect| crate::state::OwnedDamageRect {
            owner: owner.clone(),
            rect,
        }));

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

/// What a commit changed on a layer surface, for backdrop invalidation. Read by
/// [`committed_layer_damage`] before `on_commit_buffer_handler` drains the damage
/// from the surface state.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerCommitDamage {
    /// Nothing drawn changed: no new buffer and no damage (a commit carrying only
    /// a region, a frame callback, ...).
    Unchanged,
    /// The damage of the root surface, which has no subsurfaces.
    Rects {
        surface: Vec<Rectangle<i32, Logical>>,
        buffer: Vec<Rectangle<i32, BufferCoord>>,
        /// Size of the attached buffer; `None` when this commit attached none.
        buffer_size: Option<Size<i32, BufferCoord>>,
    },
    /// Anything that cannot be narrowed down: the whole surface.
    Whole,
}

/// The damage of this commit, if `surface` is the root of a layer surface. Called
/// for every commit that is not a toplevel's, before `on_commit_buffer_handler`.
pub fn committed_layer_damage(surface: &WlSurface) -> LayerCommitDamage {
    // A subsurface commit, or a root whose sync subsurfaces this commit applies:
    // their damage is not in the root's, and mapping it needs their offsets.
    if get_parent(surface).is_some() || !get_children(surface).is_empty() {
        return LayerCommitDamage::Whole;
    }
    with_states(surface, |states| {
        // Cropping is not mapped below; scaling is, as buffer size to surface size.
        if states
            .cached_state
            .get::<ViewportCachedState>()
            .current()
            .src
            .is_some()
        {
            return LayerCommitDamage::Whole;
        }
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        let attributes = attributes.current();
        let buffer_size = match &attributes.buffer {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                let Some(size) = buffer_dimensions(buffer) else {
                    return LayerCommitDamage::Whole;
                };
                Some(size)
            }
            Some(BufferAssignment::Removed) => return LayerCommitDamage::Whole,
            None => None,
        };
        if attributes.buffer_transform != wl_output::Transform::Normal {
            return LayerCommitDamage::Whole;
        }
        if attributes.damage.is_empty() {
            // A new buffer is new content even when the client damaged nothing.
            return if buffer_size.is_some() {
                LayerCommitDamage::Whole
            } else {
                LayerCommitDamage::Unchanged
            };
        }
        let mut surface_damage = Vec::new();
        let mut buffer_damage = Vec::new();
        for damage in &attributes.damage {
            match damage {
                Damage::Surface(rect) => surface_damage.push(*rect),
                Damage::Buffer(rect) => buffer_damage.push(*rect),
            }
        }
        if !buffer_damage.is_empty() && buffer_size.is_none() {
            return LayerCommitDamage::Whole;
        }
        LayerCommitDamage::Rects {
            surface: surface_damage,
            buffer: buffer_damage,
            buffer_size,
        }
    })
}

/// The global rects a layer commit invalidates for the backdrop effects that see
/// the layer. `previous_rect` / `layer_rect` are the layer's global rect at its
/// last commit and now (`None` while unmapped); a move or resize damages both.
fn layer_source_damage_rects(
    damage: &LayerCommitDamage,
    previous_rect: Option<crate::ssd::LogicalRect>,
    layer_rect: Option<crate::ssd::LogicalRect>,
) -> Vec<crate::ssd::LogicalRect> {
    if previous_rect != layer_rect {
        return previous_rect.into_iter().chain(layer_rect).collect();
    }
    let Some(layer_rect) = layer_rect else {
        return Vec::new();
    };
    let LayerCommitDamage::Rects {
        surface,
        buffer,
        buffer_size,
    } = damage
    else {
        return match damage {
            LayerCommitDamage::Unchanged => Vec::new(),
            _ => vec![layer_rect],
        };
    };
    let surface_rects = surface
        .iter()
        .map(|rect| (rect.loc.x as f64, rect.loc.y as f64, rect.size.w as f64, rect.size.h as f64));
    // The buffer covers the whole surface (a root layer surface has no geometry
    // offset), whatever buffer scale or viewport destination maps one to the other.
    let buffer_rects = buffer_size.iter().flat_map(|size| {
        let scale_x = layer_rect.width as f64 / size.w.max(1) as f64;
        let scale_y = layer_rect.height as f64 / size.h.max(1) as f64;
        buffer.iter().map(move |rect| {
            (
                rect.loc.x as f64 * scale_x,
                rect.loc.y as f64 * scale_y,
                rect.size.w as f64 * scale_x,
                rect.size.h as f64 * scale_y,
            )
        })
    });
    surface_rects
        .chain(buffer_rects)
        .filter_map(|(x, y, width, height)| {
            // Outward to whole logical pixels, then clipped to the surface.
            let left = (x.floor() as i32).max(0);
            let top = (y.floor() as i32).max(0);
            let right = (((x + width).ceil()) as i32).min(layer_rect.width);
            let bottom = (((y + height).ceil()) as i32).min(layer_rect.height);
            (right > left && bottom > top).then(|| {
                crate::ssd::LogicalRect::new(
                    layer_rect.x + left,
                    layer_rect.y + top,
                    right - left,
                    bottom - top,
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod source_damage_tests {
    use super::*;

    fn layer() -> crate::ssd::LogicalRect {
        crate::ssd::LogicalRect::new(100, 0, 500, 800)
    }

    #[test]
    fn buffer_damage_maps_through_the_buffer_to_surface_scale() {
        // A fractionally scaled client: 1.8x buffer shown through a viewport.
        let damage = LayerCommitDamage::Rects {
            surface: Vec::new(),
            buffer: vec![Rectangle::new((180, 9).into(), (90, 18).into())],
            buffer_size: Some((900, 1440).into()),
        };
        assert_eq!(
            layer_source_damage_rects(&damage, Some(layer()), Some(layer())),
            vec![crate::ssd::LogicalRect::new(200, 5, 50, 10)]
        );
    }

    #[test]
    fn surface_damage_is_clipped_to_the_surface() {
        let damage = LayerCommitDamage::Rects {
            surface: vec![
                Rectangle::new((490, 790).into(), (100, 100).into()),
                Rectangle::new((600, 0).into(), (10, 10).into()),
            ],
            buffer: Vec::new(),
            buffer_size: None,
        };
        assert_eq!(
            layer_source_damage_rects(&damage, Some(layer()), Some(layer())),
            vec![crate::ssd::LogicalRect::new(590, 790, 10, 10)]
        );
    }

    #[test]
    fn unchanged_whole_and_geometry_changes() {
        assert!(
            layer_source_damage_rects(&LayerCommitDamage::Unchanged, Some(layer()), Some(layer()))
                .is_empty()
        );
        assert_eq!(
            layer_source_damage_rects(&LayerCommitDamage::Whole, Some(layer()), Some(layer())),
            vec![layer()]
        );
        let moved = crate::ssd::LogicalRect::new(120, 0, 500, 800);
        assert_eq!(
            layer_source_damage_rects(&LayerCommitDamage::Unchanged, Some(layer()), Some(moved)),
            vec![layer(), moved]
        );
        assert_eq!(
            layer_source_damage_rects(&LayerCommitDamage::Unchanged, Some(layer()), None),
            vec![layer()]
        );
        assert_eq!(
            layer_source_damage_rects(&LayerCommitDamage::Unchanged, None, Some(layer())),
            vec![layer()]
        );
    }
}
