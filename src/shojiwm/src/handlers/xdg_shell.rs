use smithay::{
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
    utils::{Rectangle, Serial, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, ToplevelSurface, XDG_POPUP_ROLE,
            XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
            decoration::XdgDecorationHandler,
        },
    },
};

use crate::{
    grabs::{move_grab::MoveSurfaceGrab, resize_grab::ResizeSurfaceGrab},
    ssd::{WindowMoveSourceSnapshot, WindowResizeSourceSnapshot},
    state::ShojiWM,
};
use tracing::{debug, info, warn};

fn xdg_popup_debug_enabled() -> bool {
    std::env::var_os("SHOJI_XDG_POPUP_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn mark_toplevel_metadata_dirty(
    state: &mut ShojiWM,
    surface: &ToplevelSurface,
    reason: &'static str,
) {
    let wl_surface = surface.wl_surface();
    let window = state
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface() == wl_surface)
        })
        .cloned();

    let Some(window) = window else {
        warn!(
            surface = ?wl_surface.id(),
            reason,
            "xdg toplevel metadata change did not match a mapped window"
        );
        return;
    };

    state.sync_foreign_toplevel(&window);

    let snapshot = state.snapshot_window(&window);
    let cached_snapshot = state
        .window_decorations
        .get(&window)
        .map(|cached| &cached.snapshot);
    info!(
        window_id = %snapshot.id,
        title = %snapshot.title,
        cached_title = ?cached_snapshot.map(|snapshot| snapshot.title.as_str()),
        app_id = ?snapshot.app_id,
        cached_app_id = ?cached_snapshot.and_then(|snapshot| snapshot.app_id.as_deref()),
        reason,
        "xdg toplevel metadata changed"
    );

    state.runtime_dirty_window_ids.insert(snapshot.id.clone());
    state.runtime_managed_only_window_ids.remove(&snapshot.id);
    state.runtime_node_only_window_ids.remove(&snapshot.id);
    state.runtime_poll_dirty = true;
    state.request_tty_maintenance(reason);
    state.schedule_redraw();
    state.mark_window_decoration_metadata_changed(surface.wl_surface());
}

impl XdgShellHandler for ShojiWM {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let surface_id = surface.wl_surface().id();
        let wl_surface = surface.wl_surface().clone();
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

        let uses_xwayland_refresh_override = surface.wl_surface().client().is_some_and(|client| {
            client
                .get_data::<crate::state::ClientState>()
                .is_some_and(|data| data.xwayland_refresh_override)
        });
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

        self.space
            .map_element(window.clone(), initial_location, false);
        self.ensure_wayland_window_decoration_policy(&wl_surface);
        // Announce the new toplevel on ext-foreign-toplevel-list-v1 so shells
        // and the portal picker can see it. Has to happen after map so the
        // initial title/app_id reads from xdg state succeed.
        self.install_foreign_toplevel(&window);
        if uses_xwayland_refresh_override {
            self.update_xwayland_refresh_override_for_window(&window, "xdg-toplevel-map");
        }
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

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        let Some(window) = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == wl_surface)
            })
            .cloned()
        else {
            self.remove_window_decoration_negotiation(wl_surface);
            self.request_tty_maintenance("xdg-toplevel-destroyed");
            self.schedule_redraw();
            return;
        };

        let decoration = self.window_decorations.get(&window).cloned();
        let window_id = decoration
            .as_ref()
            .map(|decoration| decoration.snapshot.id.clone());
        if let (Some(window_id), Some(decoration)) = (window_id.as_deref(), decoration.as_ref()) {
            if let Err(error) = crate::backend::tty::capture_live_snapshot_for_close(self, &window)
            {
                warn!(
                    window_id,
                    title = decoration.snapshot.title,
                    app_id = decoration.snapshot.app_id,
                    ?error,
                    "failed to capture destroyed xdg toplevel before closing animation"
                );
            }
            let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
            if let Err(error) =
                self.promote_window_to_closing_snapshot(window_id, decoration, now_ms)
            {
                warn!(
                    window_id,
                    title = decoration.snapshot.title,
                    app_id = decoration.snapshot.app_id,
                    ?error,
                    "failed to promote destroyed xdg toplevel to closing snapshot"
                );
            }
            self.prune_unpromoted_window_snapshot(window_id);
        }

        self.remove_foreign_toplevel(&window);
        self.prune_window_state(&window);
        self.space.unmap_elem(&window);
        self.remove_window_decoration_negotiation(wl_surface);
        self.request_tty_maintenance("xdg-toplevel-destroyed");
        self.schedule_redraw();
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

            let initial_window_rect =
                Rectangle::new(initial_window_location, window.geometry().size);
            let initial_event_rect = self.managed_resize_initial_rect(&window, initial_window_rect);
            let mut grab = MoveSurfaceGrab::start(
                start_data,
                window,
                initial_window_location,
                initial_event_rect,
                WindowMoveSourceSnapshot::ClientCsd,
            );
            grab.notify_start(self);

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

            let initial_window_rect = Rectangle::new(initial_window_location, initial_window_size);
            let initial_event_rect = self.managed_resize_initial_rect(&window, initial_window_rect);

            if let Some(mut grab) = ResizeSurfaceGrab::start(
                start_data,
                window,
                edges.into(),
                initial_window_rect,
                initial_event_rect,
                WindowResizeSourceSnapshot::ClientCsd,
            ) {
                grab.notify_start(self);
                pointer.set_grab(self, grab, serial, Focus::Clear);
            }
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        info!(
            surface = ?wl_surface.id(),
            "xdg toplevel maximize request received"
        );
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
            .cloned()
        else {
            warn!(
                surface = ?wl_surface.id(),
                "xdg toplevel maximize request did not match a mapped window"
            );
            return;
        };
        let snapshot = self.snapshot_window(&window);
        info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            "xdg toplevel maximize request matched window"
        );

        self.request_window_maximize(
            &window,
            true,
            crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
        );
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        info!(
            surface = ?wl_surface.id(),
            "xdg toplevel unmaximize request received"
        );
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
            .cloned()
        else {
            warn!(
                surface = ?wl_surface.id(),
                "xdg toplevel unmaximize request did not match a mapped window"
            );
            return;
        };
        let snapshot = self.snapshot_window(&window);
        info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            "xdg toplevel unmaximize request matched window"
        );

        self.request_window_maximize(
            &window,
            false,
            crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
        );
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let wl_surface = surface.wl_surface();
        info!(
            surface = ?wl_surface.id(),
            "xdg toplevel fullscreen request received"
        );
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
            .cloned()
        else {
            warn!(
                surface = ?wl_surface.id(),
                "xdg toplevel fullscreen request did not match a mapped window"
            );
            return;
        };
        let output_name = output
            .as_ref()
            .and_then(smithay::output::Output::from_resource)
            .map(|output| output.name());
        let snapshot = self.snapshot_window(&window);
        info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            requested_output = ?output_name,
            "xdg toplevel fullscreen request matched window"
        );

        self.request_window_fullscreen(
            &window,
            true,
            output_name,
            crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
        );
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        info!(
            surface = ?wl_surface.id(),
            "xdg toplevel unfullscreen request received"
        );
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
            .cloned()
        else {
            warn!(
                surface = ?wl_surface.id(),
                "xdg toplevel unfullscreen request did not match a mapped window"
            );
            return;
        };
        let snapshot = self.snapshot_window(&window);
        info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            "xdg toplevel unfullscreen request matched window"
        );

        self.request_window_fullscreen(
            &window,
            false,
            None,
            crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
        );
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        let wl_surface = surface.wl_surface();
        info!(
            surface = ?wl_surface.id(),
            "xdg toplevel minimize request received"
        );
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == wl_surface))
            .cloned()
        else {
            warn!(
                surface = ?wl_surface.id(),
                "xdg toplevel minimize request did not match a mapped window"
            );
            return;
        };
        let snapshot = self.snapshot_window(&window);
        info!(
            window_id = %snapshot.id,
            title = %snapshot.title,
            app_id = ?snapshot.app_id,
            "xdg toplevel minimize request matched window"
        );

        self.request_window_minimize(
            &window,
            true,
            crate::ssd::WindowStateRequestSourceSnapshot::ClientCsd,
        );
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        mark_toplevel_metadata_dirty(self, &surface, "xdg-toplevel-app-id-changed");
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        mark_toplevel_metadata_dirty(self, &surface, "xdg-toplevel-title-changed");
    }

    fn ack_configure(&mut self, surface: WlSurface, configure: Configure) {
        if let Configure::Toplevel(configure) = configure {
            self.acknowledge_xdg_decoration_mode(&surface, configure.state.decoration_mode);
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

impl XdgDecorationHandler for ShojiWM {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.note_xdg_decoration_created(toplevel.wl_surface());
    }

    fn request_mode(
        &mut self,
        toplevel: ToplevelSurface,
        mode: xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    ) {
        let preference = match mode {
            xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ClientSide => {
                crate::ssd::WindowDecorationModeSnapshot::Client
            }
            xdg_decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode::ServerSide => {
                crate::ssd::WindowDecorationModeSnapshot::Server
            }
            _ => return,
        };
        self.note_xdg_decoration_request(
            toplevel.wl_surface(),
            Some(preference),
            crate::ssd::WindowDecorationPolicyReasonSnapshot::ClientRequest,
        );
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.note_xdg_decoration_request(
            toplevel.wl_surface(),
            None,
            crate::ssd::WindowDecorationPolicyReasonSnapshot::ClientUnset,
        );
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
    let is_xdg_popup_surface =
        smithay::wayland::compositor::get_role(surface) == Some(XDG_POPUP_ROLE);

    state.commit_xdg_decoration_mode(surface);

    // Handle toplevel commits.
    let toplevel_window = state
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        .cloned();
    if let Some(window) = toplevel_window {
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
            // xdg_toplevel metadata and size constraints are not available from
            // `new_toplevel`: clients send them between role creation and this
            // initial buffer-less commit. Evaluate the TS layout here so the
            // very first configure already carries the final managed size.
            let snapshot = state.snapshot_window(&window);
            let initial_managed_client_rect =
                match state.initial_managed_window_client_rect(&snapshot) {
                    Ok(rect) => rect,
                    Err(error) => {
                        warn!(
                            window_id = snapshot.id,
                            title = snapshot.title,
                            app_id = snapshot.app_id,
                            error = ?error,
                            "failed to compute initial managed window rect"
                        );
                        None
                    }
                };

            let toplevel = window.toplevel().unwrap();
            if let Some(client_rect) = initial_managed_client_rect {
                toplevel.with_pending_state(|pending| {
                    pending.size = Some(Size::from((client_rect.width, client_rect.height)));
                });
                let geometry = window.geometry();
                state.space.relocate_element(
                    &window,
                    (
                        client_rect.x - geometry.loc.x,
                        client_rect.y - geometry.loc.y,
                    ),
                );
            }
            toplevel.send_configure();
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
    } else if is_xdg_popup_surface {
        // Some toolkit popup paths can commit before our popup tracking is observable through the
        // regular lookup path. We still need a pre-render refresh so the first click after the
        // popup appears targets the newly visible surface tree rather than the stale parent layer.
        state.request_tty_maintenance("xdg-popup-commit-untracked");
        state
            .refresh_pointer_focus(std::time::Duration::from(state.clock.now()).as_millis() as u32);
        state.schedule_redraw();
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
