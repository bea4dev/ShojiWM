use smithay::{
    desktop::{LayerSurface, WindowSurfaceType, layer_map_for_output},
    output::Output,
    reexports::{
        wayland_server::{Resource, protocol::{wl_output, wl_surface::WlSurface}},
    },
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
        },
    },
};
use tracing::debug;

use crate::state::ShojiWM;

impl WlrLayerShellHandler for ShojiWM {
    fn shell_state(&mut self) -> &mut smithay::wayland::shell::wlr_layer::WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .unwrap_or_else(|| self.space.outputs().next().unwrap().clone());
        let layer = LayerSurface::new(surface, namespace);
        let mut map = layer_map_for_output(&output);
        map.map_layer(&layer).unwrap();
        map.arrange();
        layer.layer_surface().send_configure();
        self.schedule_redraw();
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
            let mut map = layer_map_for_output(&output);
            map.unmap_layer(&layer);
            drop(map);
            self.schedule_redraw();
        }
    }
}

pub fn handle_commit(state: &mut ShojiWM, surface: &WlSurface) {
    let Some(output) = state.space.outputs().find(|output| {
        let map = layer_map_for_output(output);
        map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).is_some()
    }).cloned() else {
        return;
    };

    let initial_configure_sent = with_states(surface, |states| {
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

    if !initial_configure_sent {
        if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
            debug!(surface = ?surface.id(), "sending initial layer-shell configure");
            layer.layer_surface().send_configure();
        }
    }

    drop(map);
    state.schedule_redraw();
}
