use crate::{
    grabs::resize_grab,
    handlers::{layer_shell, xdg_shell},
    state::{ClientState, ShojiWM},
};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        Client, Resource,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface,
        },
        shm::{ShmHandler, ShmState},
    },
};
use tracing::{debug, trace};

impl CompositorHandler for ShojiWM {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        trace!(surface = ?surface.id(), "wl_surface commit received");
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
            let commit_time = std::time::Duration::from(self.clock.now());
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

delegate_compositor!(ShojiWM);
delegate_shm!(ShojiWM);
