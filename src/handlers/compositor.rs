use crate::{grabs::resize_grab, handlers::{layer_shell, xdg_shell}, state::{ClientState, ShojiWM}};
use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_shm,
    reexports::wayland_server::{
        Resource,
        protocol::{wl_buffer, wl_surface::WlSurface},
        Client,
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_parent, is_sync_subsurface, CompositorClientState, CompositorHandler, CompositorState,
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
        on_commit_buffer_handler::<Self>(surface);
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
                window.on_commit();
                let snapshot = self.snapshot_window(window);
                let commit_time = std::time::Duration::from(self.clock.now());
                if let Some(previous_commit_time) =
                    self.window_commit_times.insert(window.clone(), commit_time)
                {
                    trace!(
                        window_id = snapshot.id,
                        title = snapshot.title,
                        app_id = snapshot.app_id,
                        commit_delta_ms = (commit_time.saturating_sub(previous_commit_time).as_secs_f64() * 1000.0),
                        "mapped window commit cadence"
                    );
                }
                self.snapshot_dirty_window_ids.insert(snapshot.id.clone());
                if let Some(decoration) = self.window_decorations.get(window) {
                    self.pending_decoration_damage.push(decoration.layout.root.rect);
                }
                debug!(surface = ?root.id(), "toplevel commit matched mapped window");
            }
        };

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
