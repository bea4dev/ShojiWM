use std::os::unix::io::OwnedFd;

use smithay::{
    delegate_xwayland_shell,
    desktop::Window,
    utils::{Logical, Rectangle},
    wayland::{
        selection::{
            SelectionTarget,
            data_device::{
                clear_data_device_selection, current_data_device_selection_userdata,
                request_data_device_client_selection, set_data_device_selection,
            },
            primary_selection::{
                clear_primary_selection, current_primary_selection_userdata,
                request_primary_client_selection, set_primary_selection,
            },
        },
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        X11Surface, X11Wm, XwmHandler,
        xwm::{Reorder, XwmId},
    },
};
use tracing::{error, trace, warn};

use crate::state::ShojiWM;

fn xwayland_popup_debug_enabled() -> bool {
    std::env::var_os("SHOJI_XWAYLAND_POPUP_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

impl XWaylandShellHandler for ShojiWM {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

delegate_xwayland_shell!(ShojiWM);

impl ShojiWM {
    fn find_x11_window(&self, surface: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| window.x11_surface() == Some(surface))
            .cloned()
    }

    fn x11_place_location(&self) -> (i32, i32) {
        let outputs: Vec<_> = self.space.outputs().cloned().collect();
        let output = match outputs.first() {
            Some(output) => output,
            None => return (0, 0),
        };
        let geo = match self.space.output_geometry(output) {
            Some(geo) => geo,
            None => return (0, 0),
        };
        (geo.loc.x + 32, geo.loc.y + 32)
    }
}

impl XwmHandler for ShojiWM {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        self.xwm.as_mut().expect("xwm not initialized")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if xwayland_popup_debug_enabled() {
            trace!(
                window = ?window,
                geometry = ?window.geometry(),
                "xwayland override-redirect window created"
            );
        }
    }

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Err(err) = window.set_mapped(true) {
            warn!(?err, "failed to mark X11 surface as mapped");
            return;
        }
        let location = self.x11_place_location();
        let smithay_window = Window::new_x11_window(window.clone());
        self.space.map_element(smithay_window, location, true);
        let bbox = window.geometry();
        let placed = Rectangle::<i32, Logical>::new((location.0, location.1).into(), bbox.size);
        if let Err(err) = window.configure(Some(placed)) {
            warn!(?err, "failed to configure newly mapped X11 window");
        }
        self.schedule_redraw();
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let location = window.geometry().loc;
        if xwayland_popup_debug_enabled() {
            trace!(
                window = ?window,
                geometry = ?window.geometry(),
                mapped_location = ?location,
                "xwayland override-redirect window mapped"
            );
        }
        let smithay_window = Window::new_x11_window(window);
        self.space
            .map_element(smithay_window, (location.x, location.y), true);
        self.schedule_redraw();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(elem) = self.find_x11_window(&window) {
            self.space.unmap_elem(&elem);
        }
        if !window.is_override_redirect() {
            if let Err(err) = window.set_mapped(false) {
                warn!(?err, "failed to mark X11 surface as unmapped");
            }
        }
        self.schedule_redraw();
    }

    fn destroyed_window(&mut self, _xwm: XwmId, _window: X11Surface) {
        self.schedule_redraw();
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geo = window.geometry();
        if let Some(w) = w {
            geo.size.w = w as i32;
        }
        if let Some(h) = h {
            geo.size.h = h as i32;
        }
        if let Err(err) = window.configure(geo) {
            warn!(?err, "failed to configure X11 window");
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        if xwayland_popup_debug_enabled() && window.is_override_redirect() {
            trace!(
                window = ?window,
                geometry = ?geometry,
                "xwayland override-redirect configure notify"
            );
        }
        if let Some(elem) = self.find_x11_window(&window) {
            self.space.map_element(elem, geometry.loc, false);
            self.schedule_redraw();
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unmaximize_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn fullscreen_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unfullscreen_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edges: smithay::xwayland::xwm::ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn allow_selection_access(&mut self, _xwm: XwmId, _selection: SelectionTarget) -> bool {
        true
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(err) = request_data_device_client_selection(&self.seat, mime_type, fd) {
                    error!(?err, "failed to request wayland clipboard for XWayland");
                }
            }
            SelectionTarget::Primary => {
                if let Err(err) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    error!(
                        ?err,
                        "failed to request wayland primary selection for XWayland"
                    );
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        trace!(?selection, ?mime_types, "X11 advertised a new selection");
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&self.display_handle, &self.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&self.display_handle, &self.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&self.display_handle, &self.seat)
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&self.display_handle, &self.seat)
                }
            }
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        self.xwm = None;
    }
}
