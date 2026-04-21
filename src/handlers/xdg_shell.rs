use smithay::{
    delegate_xdg_shell,
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window,
        WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords,
        layer_map_for_output,
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
            XdgToplevelSurfaceData, decoration::XdgDecorationHandler,
        },
    },
};

use crate::{
    grabs::{move_grab::MoveSurfaceGrab, resize_grab::ResizeSurfaceGrab},
    state::ShojiWM,
};
use tracing::{debug, info, warn};

fn xdg_popup_debug_enabled() -> bool {
    std::env::var_os("SHOJI_XDG_POPUP_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn apply_decoration_mode(
    state: &mut ShojiWM,
    toplevel: &ToplevelSurface,
    mode: xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
) {
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
        debug!(
            window_count = self.space.elements().count(),
            "mapped new toplevel into space"
        );
        self.schedule_redraw();
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        debug!(surface = ?surface.wl_surface().id(), "new xdg popup received");
        self.note_xdg_popup_created(surface.wl_surface().id().protocol_id());
        self.unconstrain_popup(&surface);
        let popup_kind = PopupKind::Xdg(surface.clone());
        match self.popups.track_popup(popup_kind.clone()) {
            Ok(()) => self.note_popup_tracked(&popup_kind, "xdg-new-popup"),
            Err(err) => warn!(
                surface_id = surface.wl_surface().id().protocol_id(),
                error = ?err,
                "failed to track xdg popup"
            ),
        }
        self.request_tty_maintenance("xdg-popup-created");
        self.refresh_pointer_focus(std::time::Duration::from(self.clock.now()).as_millis() as u32);
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
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
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
                .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
                .unwrap()
                .clone();
            let initial_window_location = self.space.element_location(&window).unwrap();
            let initial_window_size = window.geometry().size;

            surface.with_pending_state(|state| {
                state.states.set(xdg_toplevel::State::Resizing);
            });

            surface.send_pending_configure();

            if let Some(grab) = ResizeSurfaceGrab::start(
                start_data,
                window,
                edges.into(),
                Rectangle::new(initial_window_location, initial_window_size),
            ) {
                pointer.set_grab(self, grab, serial, Focus::Clear);
            }
        }
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let seat = Seat::from_resource(&seat).unwrap();
        let popup = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&popup) else {
            return;
        };

        let ret = self
            .popups
            .grab_popup::<Self>(root.clone(), popup, &seat, serial);

        if let Ok(mut grab) = ret {
            if let Some(keyboard) = seat.get_keyboard() {
                let can_receive_keyboard_focus = self
                    .space
                    .outputs()
                    .find_map(|output| {
                        let map = layer_map_for_output(output);
                        map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                            .map(|layer_surface| layer_surface.can_receive_keyboard_focus())
                    })
                    .unwrap_or(true);

                if can_receive_keyboard_focus {
                    if keyboard.is_grabbed()
                        && !(keyboard.has_grab(serial)
                            || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
                    {
                        grab.ungrab(PopupUngrabStrategy::All);
                        return;
                    }
                    keyboard.set_focus(self, grab.current_grab(), serial);
                    keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
                }
            }

            if let Some(pointer) = seat.get_pointer() {
                if pointer.is_grabbed()
                    && !(pointer.has_grab(serial)
                        || pointer
                            .has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
                {
                    grab.ungrab(PopupUngrabStrategy::All);
                    return;
                }
                pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
            }
        }
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
pub fn handle_commit(state: &mut ShojiWM, surface: &WlSurface) {
    // Handle toplevel commits.
    if let Some(window) = state
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
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
    state.popups.commit(surface);
    if let Some(popup) = state.find_popup_with_debug(surface, "xdg-handle-commit") {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                state.note_xdg_popup_committed(xdg.wl_surface().id().protocol_id());
                if !xdg.is_initial_configure_sent() {
                    // NOTE: This should never fail as the initial configure is always
                    // allowed.
                    xdg.send_configure().expect("initial configure failed");
                    let _ = state.display_handle.flush_clients();
                }
                state.request_tty_maintenance("xdg-popup-initial-configure");
                state.refresh_pointer_focus(
                    std::time::Duration::from(state.clock.now()).as_millis() as u32,
                );
                state.schedule_redraw();
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
        let debug_popup = xdg_popup_debug_enabled();
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        else {
            return;
        };

        let window_geo = self.space.element_geometry(window).unwrap();
        let window_center = smithay::utils::Point::from((
            window_geo.loc.x + window_geo.size.w / 2,
            window_geo.loc.y + window_geo.size.h / 2,
        ));
        let output = self
            .space
            .outputs()
            .filter_map(|output| {
                self.space.output_geometry(output).map(|geometry| {
                    let intersection_area = geometry
                        .intersection(window_geo)
                        .map(|intersection| {
                            i64::from(intersection.size.w.max(0))
                                * i64::from(intersection.size.h.max(0))
                        })
                        .unwrap_or(0);
                    let contains_center = geometry.contains(window_center);
                    (output, contains_center, intersection_area)
                })
            })
            .max_by_key(|(_, contains_center, intersection_area)| {
                (*contains_center, *intersection_area)
            })
            .map(|(output, _, _)| output)
            .or_else(|| self.space.outputs().next())
            .unwrap();
        let output_geo = self.space.output_geometry(output).unwrap();
        let popup_toplevel_coords = get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));

        // The target geometry for the positioner should be relative to its parent's geometry, so
        // we will compute that here.
        let mut target = output_geo;
        target.loc -= popup_toplevel_coords;
        target.loc -= window_geo.loc;

        popup.with_pending_state(|state| {
            let unconstrained = state.positioner.get_unconstrained_geometry(target);
            if debug_popup {
                debug!(
                    popup_surface_id = popup.wl_surface().id().protocol_id(),
                    root_surface_id = root.id().protocol_id(),
                    output = %output.name(),
                    window_geo = ?window_geo,
                    window_center = ?window_center,
                    output_geo = ?output_geo,
                    popup_toplevel_coords = ?popup_toplevel_coords,
                    positioner_geometry = ?state.positioner.get_geometry(),
                    target = ?target,
                    unconstrained = ?unconstrained,
                    "xdg popup unconstrain"
                );
            }
            state.geometry = unconstrained;
        });
    }
}
