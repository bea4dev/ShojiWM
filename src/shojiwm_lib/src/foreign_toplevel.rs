//! Glue between ShojiWM's per-Window lifecycle and smithay's
//! `ext-foreign-toplevel-list-v1` server.
//!
//! Each mapped window holds at most one [`ForeignToplevelHandle`] in its
//! `Window::user_data()`. The handle is created on first map, updated when
//! title or app_id changes, and removed on destroy/unmap.
//!
//! The same three calls also drive the other taskbar protocols:
//! `zwlr_foreign_toplevel_manager_v1` (`wlr_foreign_toplevel.rs`) and KDE's
//! `org_kde_plasma_window_management` (`plasma_window_management.rs`).

use smithay::desktop::Window;
use smithay::wayland::compositor::with_states;
use smithay::wayland::foreign_toplevel_list::ForeignToplevelHandle;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::state::ShojiWM;

/// Read the live (title, app_id) for a Window — wayland or xwayland — using
/// the same accessors as our SSD snapshots.
fn read_title_app_id(window: &Window) -> (String, String) {
    if let Some(toplevel) = window.toplevel() {
        return with_states(toplevel.wl_surface(), |states| {
            let role = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .expect("xdg toplevel surface should have role data")
                .lock()
                .expect("xdg toplevel role mutex poisoned");
            (
                role.title.clone().unwrap_or_default(),
                role.app_id.clone().unwrap_or_default(),
            )
        });
    }
    if let Some(x11) = window.x11_surface() {
        return (x11.title(), x11.class());
    }
    (String::new(), String::new())
}

impl ShojiWM {
    /// Create a foreign-toplevel handle for this window if one doesn't exist
    /// yet, advertising the initial title/app_id. Idempotent: extra calls are
    /// no-ops.
    pub fn install_foreign_toplevel(&mut self, window: &Window) {
        if window.user_data().get::<ForeignToplevelHandle>().is_some() {
            return;
        }
        let (title, app_id) = read_title_app_id(window);
        let handle = self
            .foreign_toplevel_list_state
            .new_toplevel::<Self>(title.clone(), app_id.clone());
        // `done` lets any already-connected clients finalize the initial
        // burst of {title, app_id} events.
        handle.send_done();
        window.user_data().insert_if_missing(|| handle);
        self.install_wlr_foreign_toplevel(window, &title, &app_id);
        self.install_plasma_window(window);
    }

    /// Re-read title/app_id from the window and push any changes to the
    /// `ext-foreign-toplevel-list-v1` clients. Cheap when nothing changed:
    /// smithay's `send_title`/`send_app_id` short-circuit on equality.
    pub fn sync_foreign_toplevel(&mut self, window: &Window) {
        let Some(handle) = window.user_data().get::<ForeignToplevelHandle>().cloned() else {
            return;
        };
        let (title, app_id) = read_title_app_id(window);
        let title_changed = handle.title() != title;
        let app_id_changed = handle.app_id() != app_id;
        if !title_changed && !app_id_changed {
            self.sync_wlr_foreign_toplevel(window, &title, &app_id);
            self.sync_plasma_window(window);
            return;
        }
        if title_changed {
            handle.send_title(&title);
        }
        if app_id_changed {
            handle.send_app_id(&app_id);
        }
        handle.send_done();
        self.sync_wlr_foreign_toplevel(window, &title, &app_id);
        self.sync_plasma_window(window);
    }

    /// Push focus, minimize and geometry changes of every window to all
    /// taskbar protocols.
    pub fn sync_taskbar_window_states(&mut self) {
        self.sync_wlr_foreign_toplevel_states();
        self.sync_plasma_window_states();
    }

    /// Announce the toplevel as closed and drop our reference. Safe to call
    /// multiple times — the handle becomes inert after the first close.
    pub fn remove_foreign_toplevel(&mut self, window: &Window) {
        let Some(handle) = window.user_data().get::<ForeignToplevelHandle>().cloned() else {
            return;
        };
        self.foreign_toplevel_list_state.remove_toplevel(&handle);
        self.remove_wlr_foreign_toplevel(window);
        self.remove_plasma_window(window);
        // We can't remove from UserDataMap, but the handle is now inert so
        // subsequent sync_foreign_toplevel calls are harmless no-ops.
        let _ = window;
    }
}
