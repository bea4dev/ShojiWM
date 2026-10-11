//! `xdg_toplevel_tag_manager_v1` server (staging xdg-toplevel-tag-v1.xml).
//!
//! A client tags its windows ("org.kde.spectacle" overlay, a main window)
//! so a compositor can remember window rules across restarts. ShojiWM has
//! no use for the tags and only accepts them.
//!
//! The global must exist anyway: KDE's KWindowSystem (6.30) calls
//! `set_toplevel_tag` without checking that the compositor offers the
//! protocol. Without the global, Spectacle's region mode (`spectacle -r`)
//! crashes in `wl_proxy_get_version(NULL)` as its overlay window is made.

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};
use wayland_protocols::xdg::toplevel_tag::v1::server::xdg_toplevel_tag_manager_v1::{
    self, XdgToplevelTagManagerV1,
};

use crate::state::ShojiWM;

const VERSION: u32 = 1;

/// Creates the global. Nothing to keep: the requests are ignored.
pub fn init_xdg_toplevel_tag(display: &DisplayHandle) {
    display.create_global::<ShojiWM, XdgToplevelTagManagerV1, _>(VERSION, ());
}

impl GlobalDispatch<XdgToplevelTagManagerV1, ()> for ShojiWM {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<XdgToplevelTagManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgToplevelTagManagerV1, ()> for ShojiWM {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &XdgToplevelTagManagerV1,
        request: xdg_toplevel_tag_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use xdg_toplevel_tag_manager_v1::Request;
        match request {
            // Accepted and ignored (see the module header).
            Request::SetToplevelTag { .. } => {}
            Request::SetToplevelDescription { .. } => {}
            Request::Destroy => {}
            _ => {}
        }
    }
}
