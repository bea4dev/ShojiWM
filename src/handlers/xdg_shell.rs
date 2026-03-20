use smithay::{
    delegate_xdg_shell,
    desktop::{
        PopupKind, PopupManager, Space, Window, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat,
        pointer::{Focus, GrabStartData as PointerGrabStartData},
    },
    reexports::{
        wayland_protocols::xdg::decoration as xdg_decoration,
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Resource,
            protocol::{wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Rectangle, Serial},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
            decoration::XdgDecorationHandler,
        },
    },
};

use crate::{
    grabs::{move_grab::MoveSurfaceGrab, resize_grab::ResizeSurfaceGrab},
    state::ShojiWM,
};
use tracing::{debug, info, warn};

fn apply_decoration_mode(state: &mut ShojiWM, toplevel: &ToplevelSurface, mode: xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode) {
    toplevel.with_pending_state(|pending| {
        pending.decoration_mode = Some(mode);
    });

    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    } else {
        toplevel.send_configure();
    }
    state.schedule_redraw();
}

impl XdgShellHandler for ShojiWM {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let surface_id = surface.wl_surface().id();
        info!(
            surface = ?surface_id,
            "new xdg toplevel received"
        );

        // Experimental Chromium CSD suppression hack, intentionally left disabled:
        //
        // Some compositors appear to force Chromium-family clients to stop drawing their own
        // rounded top corners by sending an initial configure with the maximized state set, even
        // for ordinary floating windows. This likely works because Chromium switches decoration
        // paths when it believes the window starts out maximized.
        //
        // We are deliberately not enabling this here because it is a risky compatibility hack:
        // it can change client behavior in hard-to-predict ways, may break initial sizing or
        // state tracking, and would be surprising as a compositor default for non-maximized
        // windows.
        //
        // If we want to revisit this later, the rough shape would be:
        //
        // surface.with_pending_state(|state| {
        //     state.states.set(xdg_toplevel::State::Maximized);
        // });
        // surface.send_pending_configure();

        let window = Window::new_wayland_window(surface);
        let snapshot = self.snapshot_window(&window);
        let initial_location = match self.suggested_window_location(&snapshot) {
            Ok(location) => location,
            Err(error) => {
                warn!(
                    window_id = snapshot.id,
                    title = snapshot.title,
                    app_id = snapshot.app_id,
                    error = ?error,
                    "failed to compute suggested SSD-aware window location, falling back to origin"
                );
                (0, 0)
            }
        };

        self.space.map_element(window, initial_location, false);
        debug!(window_count = self.space.elements().count(), "mapped new toplevel into space");
        self.schedule_redraw();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        debug!(surface = ?surface.wl_surface().id(), "new xdg popup received");
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
        self.schedule_redraw();
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            let geometry = positioner.get_geometry();
            state.geometry = geometry;
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, surface: ToplevelSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();

        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let window = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.space.element_location(&window).unwrap();

            let grab = MoveSurfaceGrab {
                start_data,
                window,
                initial_window_location,
            };

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        seat: wl_seat::WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        let seat = Seat::from_resource(&seat).unwrap();

        let wl_surface = surface.wl_surface();

        if let Some(start_data) = check_grab(&seat, wl_surface, serial) {
            let pointer = seat.get_pointer().unwrap();

            let window = self
                .space
                .elements()
                .find(|w| w.toplevel().unwrap().wl_surface() == wl_surface)
                .unwrap()
                .clone();
            let initial_window_location = self.space.element_location(&window).unwrap();
            let initial_window_size = window.geometry().size;

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });

            surface.send_pending_configure();

            let grab = ResizeSurfaceGrab::start(
                start_data,
                window,
                edges.into(),
                Rectangle::new(initial_window_location, initial_window_size),
            );

            pointer.set_grab(self, grab, serial, Focus::Clear);
        }
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // TODO popup grabs
    }
}

// Xdg Shell
delegate_xdg_shell!(ShojiWM);

impl XdgDecorationHandler for ShojiWM {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        apply_decoration_mode(self, &toplevel, self.default_decoration_mode);
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        _mode: xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        apply_decoration_mode(self, &toplevel, self.default_decoration_mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        apply_decoration_mode(self, &toplevel, self.default_decoration_mode);
    }
}

fn check_grab(
    seat: &Seat<ShojiWM>,
    surface: &WlSurface,
    serial: Serial,
) -> Option<PointerGrabStartData<ShojiWM>> {
    let pointer = seat.get_pointer()?;

    // Check that this surface has a click grab.
    if !pointer.has_grab(serial) {
        return None;
    }

    let start_data = pointer.grab_start_data()?;

    let (focus, _) = start_data.focus.as_ref()?;
    // If the focus was for a different surface, ignore the request.
    if !focus.id().same_client_as(&surface.id()) {
        return None;
    }

    Some(start_data)
}

/// Should be called on `WlSurface::commit`
pub fn handle_commit(popups: &mut PopupManager, space: &Space<Window>, surface: &WlSurface) {
    // Handle toplevel commits.
    if let Some(window) = space
        .elements()
        .find(|w| w.toplevel().unwrap().wl_surface() == surface)
        .cloned()
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });

        if !initial_configure_sent {
            window.toplevel().unwrap().send_configure();
        }
    }

    // Handle popup commits.
    popups.commit(surface);
    if let Some(popup) = popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    // NOTE: This should never fail as the initial configure is always
                    // allowed.
                    xdg.send_configure().expect("initial configure failed");
                }
            }
            PopupKind::InputMethod(ref _input_method) => {}
        }
    }
}

impl ShojiWM {
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().unwrap().wl_surface() == &root)
        else {
            return;
        };

        let output = self.space.outputs().next().unwrap();
        let output_geo = self.space.output_geometry(output).unwrap();
        let window_geo = self.space.element_geometry(window).unwrap();

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = output_geo;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
