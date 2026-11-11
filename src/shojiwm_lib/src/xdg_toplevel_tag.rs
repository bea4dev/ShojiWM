//! `xdg_toplevel_tag_manager_v1` (staging xdg-toplevel-tag-v1.xml), through
//! smithay's implementation.
//!
//! A client tags its windows ("org.kde.spectacle" overlay, a main window)
//! so window rules can tell them apart across restarts. smithay keeps the
//! tag in the surface's `XdgToplevelTagSurfaceData`; the window snapshot
//! hands it to the config as the window's `tag`.
//!
//! KDE's KWindowSystem (6.30) calls `set_toplevel_tag` without checking that
//! the compositor offers the protocol. Without the global, Spectacle's region
//! mode (`spectacle -r`) crashes in `wl_proxy_get_version(NULL)` as its
//! overlay window is made.

use smithay::{
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::XdgToplevel,
        wayland_server::DisplayHandle,
    },
    wayland::xdg_toplevel_tag::{XdgToplevelTagHandler, XdgToplevelTagManager},
};

use crate::{handlers::xdg_shell::mark_toplevel_metadata_dirty, state::ShojiWM};

pub fn init_xdg_toplevel_tag(display: &DisplayHandle) {
    XdgToplevelTagManager::new::<ShojiWM>(display);
}

impl XdgToplevelTagHandler for ShojiWM {
    fn set_tag(&mut self, toplevel: XdgToplevel, _tag: String) {
        let Some(surface) = self.xdg_shell_state.get_toplevel(&toplevel) else {
            return;
        };
        mark_toplevel_metadata_dirty(self, &surface, "xdg-toplevel-tag-changed");
    }
}
