//! `xdg_toplevel_tag_manager_v1` (staging xdg-toplevel-tag-v1.xml), through
//! smithay's implementation.
//!
//! A client tags its windows ("org.kde.spectacle" overlay, a main window)
//! so a compositor can remember window rules across restarts. ShojiWM has
//! no use for the tags and only accepts them (smithay keeps them in the
//! surface's `XdgToplevelTagSurfaceData`).
//!
//! The global must exist anyway: KDE's KWindowSystem (6.30) calls
//! `set_toplevel_tag` without checking that the compositor offers the
//! protocol. Without the global, Spectacle's region mode (`spectacle -r`)
//! crashes in `wl_proxy_get_version(NULL)` as its overlay window is made.

use smithay::{
    reexports::wayland_server::DisplayHandle,
    wayland::xdg_toplevel_tag::{XdgToplevelTagHandler, XdgToplevelTagManager},
};

use crate::state::ShojiWM;

/// Creates the global. Nothing to keep: the tags are ignored.
pub fn init_xdg_toplevel_tag(display: &DisplayHandle) {
    XdgToplevelTagManager::new::<ShojiWM>(display);
}

impl XdgToplevelTagHandler for ShojiWM {}
