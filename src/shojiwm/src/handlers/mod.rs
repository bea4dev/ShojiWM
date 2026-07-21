mod compositor;
mod layer_shell;
mod xdg_shell;
mod xwayland;

//
// Wl Seat
//

use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::{Focus, PointerHandle};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::desktop::{PopupKind, WindowSurfaceType, find_popup_root_surface, layer_map_for_output};
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
    Mode as KdeDecorationMode, OrgKdeKwinServerDecoration,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Rectangle};
use smithay::utils::{Serial, SERIAL_COUNTER};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::{
    PointerConstraintsHandler, with_pointer_constraint,
};
use smithay::wayland::background_effect::{Capability, ExtBackgroundEffectHandler};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, ImportNotifier};
use smithay::wayland::fractional_scale::{with_fractional_scale, FractionalScaleHandler};
use smithay::wayland::idle_inhibit::IdleInhibitHandler;
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::wayland::input_method::{InputMethodHandler, PopupSurface};
use smithay::wayland::shell::kde::decoration::KdeDecorationHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::session_lock::{
    LockSurface, LockSurfaceConfigure, SessionLockHandler, SessionLockManagerState,
    SessionLocker,
};
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::wayland::xdg_activation::{
    XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
};
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::wayland::image_capture_source::{
    ImageCaptureSource, ImageCaptureSourceHandler, OutputCaptureSourceHandler,
    OutputCaptureSourceState, ToplevelCaptureSourceHandler, ToplevelCaptureSourceState,
};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, Frame, FrameRef, ImageCopyCaptureHandler,
    ImageCopyCaptureState, Session, SessionRef,
};
use smithay::output::WeakOutput;
use smithay::{backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma}};

use crate::state::ShojiWM;

impl SeatHandler for ShojiWM {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<ShojiWM> {
        &mut self.seat_state
    }

    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        // A new cursor surface (or hotspot update on the same surface) was set; clear
        // any previous override marker so the commit handler re-applies the hotspot
        // reinterpretation exactly once for the next commit (Xwayland HiDPI workaround).
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &image {
            smithay::wayland::compositor::with_states(surface, |states| {
                if let Some(applied) = states
                    .data_map
                    .get::<std::sync::Mutex<crate::state::CursorOverrideApplied>>()
                {
                    applied.lock().unwrap().applied = false;
                }
            });
        }
        self.cursor_status = image;
        self.schedule_redraw();
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        self.sync_window_keyboard_focus_from_surface(focused);
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
        if std::env::var_os("SHOJI_LAYER_FOCUS_DEBUG").is_some() {
            tracing::debug!(
                focused_surface = focused.map(|surface| surface.id().protocol_id()),
                "keyboard focus changed"
            );
        }
    }
}

smithay::delegate_dispatch2!(ShojiWM);

impl PointerConstraintsHandler for ShojiWM {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() != Some(surface) {
            return;
        }
        let Some((focused_surface, surface_origin)) = self.pointer_contents.surface.as_ref() else {
            return;
        };
        if focused_surface != surface {
            return;
        }
        let location = pointer.current_location();
        with_pointer_constraint(surface, pointer, |constraint| {
            let Some(constraint) = constraint else {
                return;
            };
            let local = (location - *surface_origin).to_i32_round();
            if constraint
                .region()
                .is_none_or(|region| region.contains(local))
            {
                constraint.activate();
            }
        });
    }

    fn remove_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {}

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        location: smithay::utils::Point<f64, smithay::utils::Logical>,
    ) {
        if !with_pointer_constraint(surface, pointer, |constraint| {
            constraint.is_some_and(|constraint| constraint.is_active())
        }) {
            return;
        }

        let Some((focused_surface, surface_origin)) = self.pointer_contents.surface.as_ref() else {
            return;
        };
        if focused_surface == surface {
            pointer.set_location(*surface_origin + location);
        }
    }
}

impl ForeignToplevelListHandler for ShojiWM {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.foreign_toplevel_list_state
    }
}

impl IdleNotifierHandler for ShojiWM {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

impl IdleInhibitHandler for ShojiWM {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle_inhibited_surfaces.push(surface);
        self.refresh_idle_inhibit_state();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        if let Some(index) = self
            .idle_inhibited_surfaces
            .iter()
            .position(|inhibited| inhibited == &surface)
        {
            self.idle_inhibited_surfaces.remove(index);
        }
        self.refresh_idle_inhibit_state();
    }
}

impl SessionLockHandler for ShojiWM {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        self.session_lock_active = true;
        self.layer_shell_on_demand_focus = None;
        self.window_keyboard_focus_owner = None;
        self.window_keyboard_focus = None;
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(
                self,
                Option::<WlSurface>::None,
                SERIAL_COUNTER.next_serial(),
            );
        }
        confirmation.lock();
        self.schedule_redraw();
    }

    fn unlock(&mut self) {
        self.session_lock_active = false;
        self.session_lock_surfaces.clear();
        self.pointer_contents = crate::state::PointerContents::default();
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(
                self,
                Option::<WlSurface>::None,
                SERIAL_COUNTER.next_serial(),
            );
        }
        self.update_keyboard_focus(SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    fn new_surface(&mut self, surface: LockSurface, output: WlOutput) {
        let Some(output) = self.output_for_lock_resource(&surface, &output) else {
            tracing::warn!("session lock surface could not be matched to an output");
            return;
        };
        self.session_lock_surfaces
            .insert(output.name(), surface.clone());
        self.configure_session_lock_surface_for_output(&output);
        self.focus_session_lock_surface(SERIAL_COUNTER.next_serial());
        self.schedule_redraw();
    }

    fn ack_configure(&mut self, _surface: WlSurface, _configure: LockSurfaceConfigure) {
        self.schedule_redraw();
    }
}

// ext-image-capture-source-v1 + ext-image-copy-capture-v1 (Phase 5b-i skeleton)
//
// This wires up the protocol globals and the four handler traits. Actual
// frame rendering is deferred to Phase 5b-ii (outputs reusing the existing
// screencopy_render machinery) and 5b-iii (per-toplevel render). For now
// `frame()` always fails with Unknown so clients see well-defined behaviour.

impl ImageCaptureSourceHandler for ShojiWM {
    fn source_destroyed(&mut self, _source: ImageCaptureSource) {}
}

impl OutputCaptureSourceHandler for ShojiWM {
    fn output_capture_source_state(&mut self) -> &mut OutputCaptureSourceState {
        &mut self.output_capture_source_state
    }

    fn output_source_created(
        &mut self,
        source: ImageCaptureSource,
        output: &smithay::output::Output,
    ) {
        // Stash the WeakOutput so capture_constraints / frame can resolve back.
        source.user_data().insert_if_missing(|| output.downgrade());
    }
}

impl ToplevelCaptureSourceHandler for ShojiWM {
    fn toplevel_capture_source_state(&mut self) -> &mut ToplevelCaptureSourceState {
        &mut self.toplevel_capture_source_state
    }

    fn toplevel_source_created(
        &mut self,
        source: ImageCaptureSource,
        toplevel: ForeignToplevelHandle,
    ) {
        source
            .user_data()
            .insert_if_missing(|| toplevel.downgrade());
    }
}

impl ImageCopyCaptureHandler for ShojiWM {
    fn image_copy_capture_state(&mut self) -> &mut ImageCopyCaptureState {
        &mut self.image_copy_capture_state
    }

    fn capture_constraints(&mut self, source: &ImageCaptureSource) -> Option<BufferConstraints> {
        // Resolve the source back to either an Output or a ForeignToplevelHandle
        // and report its size. SHM Xrgb8888 only for the first cut — dmabuf
        // negotiation comes later.
        let size = resolve_source_size(self, source)?;
        Some(BufferConstraints {
            size,
            shm: vec![smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888],
            dma: None,
        })
    }

    fn new_session(&mut self, session: Session) {
        let source_id = session.source().id();
        self.image_copy_capture_sessions
            .entry(source_id)
            .or_default()
            .push(session);
    }

    fn frame(&mut self, session: &SessionRef, frame: Frame) {
        // Route the frame to the next render pass for whichever output /
        // toplevel owns its source. The render code drains the queue.
        use crate::backend::image_copy_capture_render::{CaptureTarget, PendingCapture};

        let draw_cursor = session.draw_cursor();
        let source = session.source();
        if let Some(weak) = source.user_data().get::<WeakOutput>() {
            let Some(output) = weak.upgrade() else {
                frame.fail(CaptureFailureReason::Unknown);
                return;
            };
            if !self.runtime_output_render_enabled(&output.name()) {
                frame.fail(CaptureFailureReason::Unknown);
                return;
            }
            self.image_copy_capture_pending.push(PendingCapture {
                frame,
                target: CaptureTarget::Output(weak.clone()),
                draw_cursor,
                session: session.clone(),
            });
            return;
        }
        if let Some(weak) = source
            .user_data()
            .get::<smithay::wayland::foreign_toplevel_list::ForeignToplevelWeakHandle>(
        ) {
            self.image_copy_capture_pending.push(PendingCapture {
                frame,
                target: CaptureTarget::Toplevel(weak.clone()),
                draw_cursor,
                session: session.clone(),
            });
            return;
        }
        // Unknown source type — fail it so the client doesn't hang.
        frame.fail(CaptureFailureReason::Unknown);
    }

    fn frame_aborted(&mut self, _frame: FrameRef) {}

    fn session_destroyed(&mut self, session: SessionRef) {
        let source_id = session.source().id();
        if let Some(vec) = self.image_copy_capture_sessions.get_mut(&source_id) {
            vec.retain(|s| s.as_ref() != session);
            if vec.is_empty() {
                self.image_copy_capture_sessions.remove(&source_id);
            }
        }
    }
}

/// Resolve an `ImageCaptureSource` to its natural buffer size. Returns `None`
/// if the source's underlying object is gone (output disconnected, window
/// closed).
fn resolve_source_size(
    state: &ShojiWM,
    source: &ImageCaptureSource,
) -> Option<smithay::utils::Size<i32, smithay::utils::Buffer>> {
    use smithay::utils::Transform;
    if let Some(weak) = source.user_data().get::<WeakOutput>()
        && let Some(output) = weak.upgrade()
    {
        if !state.runtime_output_render_enabled(&output.name()) {
            return None;
        }
        let mode = output.current_mode()?;
        // Apply the output's transform so the captured buffer matches what
        // physically renders.
        let physical = output.current_transform().transform_size(mode.size);
        return Some(physical.to_logical(1).to_buffer(1, Transform::Normal));
    }
    if let Some(weak) = source
        .user_data()
        .get::<smithay::wayland::foreign_toplevel_list::ForeignToplevelWeakHandle>()
        && let Some(handle) = weak.upgrade()
    {
        // Find the window that owns this handle and use its geometry.
        let window = state.space.elements().find(|w| {
            w.user_data()
                .get::<ForeignToplevelHandle>()
                .is_some_and(|h| h.matches(&handle))
        })?;
        let geom = window.geometry();
        if geom.size.w > 0 && geom.size.h > 0 {
            // Buffer dims must match the scale the render path uses (cursor
            // elements come in at output-physical scale; rendering at
            // anything else creates a cursor / window scale mismatch). Pick
            // the scale of the output the window is primarily on.
            let scale = state
                .space
                .outputs_for_element(window)
                .into_iter()
                .next()
                .map(|o| o.current_scale().fractional_scale())
                .unwrap_or(1.0);
            let w = (geom.size.w as f64 * scale).round().max(1.0) as i32;
            let h = (geom.size.h as f64 * scale).round().max(1.0) as i32;
            return Some(
                smithay::utils::Size::<i32, smithay::utils::Logical>::from((w, h))
                    .to_buffer(1, Transform::Normal),
            );
        }
    }
    None
}

impl XdgActivationHandler for ShojiWM {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn token_created(&mut self, _token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        let Some((serial, seat)) = data.serial else {
            return false;
        };

        let Some(seat) = Seat::from_resource(&seat) else {
            return false;
        };
        if seat != self.seat {
            return false;
        }

        let keyboard_valid = seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        if keyboard_valid {
            return true;
        }

        let pointer_valid = seat
            .get_pointer()
            .and_then(|pointer| pointer.last_enter())
            .is_some_and(|last_enter| serial.is_no_older_than(&last_enter));
        if pointer_valid {
            return true;
        }

        // Discord and Telegram commonly create activation tokens with a stale
        // serial when restoring from their tray menu. Treat tokens from our
        // own seat as user-driven so minimized windows can be shown again.
        tracing::debug!(
            ?serial,
            "accepting xdg activation token with stale serial from known seat"
        );
        true
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if token_data.timestamp.elapsed().as_secs() >= 10 {
            return;
        }

        let window = self
            .space
            .elements()
            .find(|candidate| self.surface_belongs_to_window(candidate, &surface))
            .cloned();

        if let Some(window) = window {
            self.request_window_activate(
                &window,
                crate::ssd::WindowActivateRequestSourceSnapshot::XdgActivation,
            );
            if !self
                .window_decorations
                .get(&window)
                .is_some_and(|decoration| decoration.managed_window.managed)
            {
                self.space.raise_element(&window, true);
            }
            self.set_window_keyboard_focus_target_surface(&window, Some(&surface));
            self.focus_layer_surface_if_on_demand(None);
            self.update_keyboard_focus(Serial::from(0));
            self.schedule_redraw();
        }
    }
}

impl FractionalScaleHandler for ShojiWM {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

        let popup_root = self
            .popups
            .find_popup(&surface)
            .or_else(|| self.popups.find_popup(&root))
            .and_then(|popup| find_popup_root_surface(&popup).ok());

        let focused_output = self
            .seat
            .get_keyboard()
            .and_then(|keyboard| keyboard.current_focus())
            .or_else(|| self.window_keyboard_focus.clone())
            .or_else(|| {
                self.layer_shell_on_demand_focus
                    .as_ref()
                    .map(|layer| layer.wl_surface().clone())
            })
            .and_then(|focused_surface| {
                let mut focused_root = focused_surface;
                while let Some(parent) = smithay::wayland::compositor::get_parent(&focused_root) {
                    focused_root = parent;
                }

                self.space
                    .elements()
                    .find(|window| {
                        window
                            .toplevel()
                            .is_some_and(|toplevel| toplevel.wl_surface() == &focused_root)
                            || window
                                .x11_surface()
                                .and_then(|x11| x11.wl_surface())
                                .as_ref()
                                == Some(&focused_root)
                    })
                    .cloned()
                    .and_then(|window| self.space.outputs_for_element(&window).first().cloned())
                    .or_else(|| {
                        self.space.outputs().find_map(|output| {
                            let map = layer_map_for_output(output);
                            let found = map
                                .layer_for_surface(&focused_root, WindowSurfaceType::TOPLEVEL)
                                .is_some();
                            drop(map);
                            found.then(|| output.clone())
                        })
                    })
            });

        smithay::wayland::compositor::with_states(&surface, |states| {
            let primary_scanout_output =
                smithay::desktop::utils::surface_primary_scanout_output(&surface, states)
                    .or_else(|| {
                        if root != surface {
                            smithay::wayland::compositor::with_states(&root, |states| {
                                smithay::desktop::utils::surface_primary_scanout_output(
                                    &root, states,
                                )
                                .or_else(|| {
                                    self.space
                                        .elements()
                                        .find(|window| {
                                            window.toplevel().is_some_and(|toplevel| {
                                                toplevel.wl_surface() == &root
                                            })
                                        })
                                        .cloned()
                                        .and_then(|window| {
                                            self.space.outputs_for_element(&window).first().cloned()
                                        })
                                        .or_else(|| {
                                            self.space.outputs().find_map(|output| {
                                                let map = layer_map_for_output(output);
                                                let found = map
                                                    .layer_for_surface(
                                                        &root,
                                                        WindowSurfaceType::TOPLEVEL,
                                                    )
                                                    .is_some();
                                                drop(map);
                                                found.then(|| output.clone())
                                            })
                                        })
                                })
                            })
                        } else {
                            self.space
                                .elements()
                                .find(|window| {
                                    window
                                        .toplevel()
                                        .is_some_and(|toplevel| toplevel.wl_surface() == &root)
                                })
                                .cloned()
                                .and_then(|window| {
                                    self.space.outputs_for_element(&window).first().cloned()
                                })
                                .or_else(|| {
                                    self.space.outputs().find_map(|output| {
                                        let map = layer_map_for_output(output);
                                        let found = map
                                            .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                                            .is_some();
                                        drop(map);
                                        found.then(|| output.clone())
                                    })
                                })
                        }
                    })
                    .or_else(|| {
                        popup_root.as_ref().and_then(|popup_root| {
                            self.space
                                .elements()
                                .find(|window| {
                                    window
                                        .toplevel()
                                        .is_some_and(|toplevel| toplevel.wl_surface() == popup_root)
                                })
                                .cloned()
                                .and_then(|window| {
                                    self.space.outputs_for_element(&window).first().cloned()
                                })
                                .or_else(|| {
                                    self.space.outputs().find_map(|output| {
                                        let map = layer_map_for_output(output);
                                        let found = map
                                            .layer_for_surface(
                                                popup_root,
                                                WindowSurfaceType::TOPLEVEL,
                                            )
                                            .is_some();
                                        drop(map);
                                        found.then(|| output.clone())
                                    })
                                })
                        })
                    })
                    .or_else(|| focused_output.clone())
                    .or_else(|| self.space.outputs().next().cloned());

            if let Some(output) = primary_scanout_output {
                with_fractional_scale(states, |fractional_scale| {
                    fractional_scale.set_preferred_scale(output.current_scale().fractional_scale());
                });
            }
        });
    }
}

impl TabletSeatHandler for ShojiWM {
    fn tablet_tool_image(
        &mut self,
        _tool: &smithay::backend::input::TabletToolDescriptor,
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        self.cursor_status = image;
        self.schedule_redraw();
    }
}

impl InputMethodHandler for ShojiWM {
    fn new_popup(&mut self, surface: PopupSurface) {
        let popup_kind = PopupKind::from(surface);
        if let Err(err) = self.popups.track_popup(popup_kind.clone()) {
            tracing::warn!(?err, "failed to track input method popup");
        } else {
            self.note_popup_tracked(&popup_kind, "input-method-new-popup");
        }
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {}

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        self.note_popup_dismiss_requested(
            surface.wl_surface(),
            surface
                .get_parent()
                .map(|parent| parent.surface.id().protocol_id()),
            "input-method-dismiss-popup",
        );
        if let Some(parent) = surface.get_parent().map(|parent| parent.surface.clone()) {
            let _ =
                smithay::desktop::PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space
            .elements()
            .find_map(|window| {
                (window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == parent))
                .then(|| window.geometry())
            })
            .unwrap_or_default()
    }
}

impl KdeDecorationHandler for ShojiWM {
    fn kde_decoration_state(
        &self,
    ) -> &smithay::wayland::shell::kde::decoration::KdeDecorationState {
        &self.kde_decoration_state
    }

    fn new_decoration(&mut self, surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration) {
        self.note_kde_decoration_created(surface, decoration);
    }

    fn request_mode(
        &mut self,
        surface: &WlSurface,
        decoration: &OrgKdeKwinServerDecoration,
        mode: smithay::reexports::wayland_server::WEnum<KdeDecorationMode>,
    ) {
        if let Ok(mode) = mode.into_result() {
            self.note_kde_decoration_request(
                surface,
                decoration,
                match mode {
                    KdeDecorationMode::Client => crate::ssd::WindowDecorationModeSnapshot::Client,
                    KdeDecorationMode::Server => crate::ssd::WindowDecorationModeSnapshot::Server,
                    _ => return,
                },
            );
        }
    }

    fn release(&mut self, _decoration: &OrgKdeKwinServerDecoration, surface: &WlSurface) {
        self.remove_window_decoration_negotiation(surface);
    }
}

impl DmabufHandler for ShojiWM {
    fn dmabuf_state(&mut self) -> &mut smithay::wayland::dmabuf::DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let imported = self
            .tty_backends
            .values_mut()
            .any(|backend| backend.renderer.import_dmabuf(&dmabuf, None).is_ok());

        if imported || self.tty_backends.is_empty() {
            let _ = notifier.successful::<ShojiWM>();
        } else {
            notifier.failed();
        }
    }
}

impl ExtBackgroundEffectHandler for ShojiWM {
    fn capabilities(&self) -> Capability {
        Capability::Blur
    }

    fn set_blur_region(
        &mut self,
        _wl_surface: WlSurface,
        _region: smithay::wayland::compositor::RegionAttributes,
    ) {
        self.schedule_redraw();
    }

    fn unset_blur_region(&mut self, _wl_surface: WlSurface) {
        self.schedule_redraw();
    }
}

//
// Wl Data Device
//

impl SelectionHandler for ShojiWM {
    type SelectionUserData = ();
}

impl DataDeviceHandler for ShojiWM {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for ShojiWM {}
impl WaylandDndGrabHandler for ShojiWM {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let ptr = seat.get_pointer().unwrap();
                let start_data = ptr.grab_start_data().unwrap();

                // create a dnd grab to start the operation
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                ptr.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => {
                // smallvil lacks touch handling
                source.cancel();
            }
        }
    }
}

impl PrimarySelectionHandler for ShojiWM {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl DataControlHandler for ShojiWM {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

//
// Wl Output & Xdg Output
//

impl OutputHandler for ShojiWM {
    fn output_bound(&mut self, _output: Output, _wl_output: WlOutput) {
        // ext-workspace may be bound before wl_output. Refresh group/output
        // associations whenever a client creates a wl_output resource so that
        // the matching group receives output_enter regardless of bind order.
        let dh = self.display_handle.clone();
        let outputs = self.space.outputs().cloned().collect::<Vec<_>>();
        self.ext_workspace_manager_state
            .refresh_outputs(&dh, &outputs);
    }
}

//
// wlr-screencopy
//

impl crate::protocols::screencopy::ScreencopyHandler for ShojiWM {
    fn frame(
        &mut self,
        manager: &smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        screencopy: crate::protocols::screencopy::Screencopy,
    ) {
        // Queue both with/without-damage requests for processing on the next
        // redraw of the captured output. Without-damage requests are always
        // rendered, while with-damage requests wait until damage exists.
        self.screencopy_state.push(manager, screencopy);
        self.schedule_redraw();
    }

    fn screencopy_output_enabled(&self, output: &Output) -> bool {
        self.runtime_output_render_enabled(&output.name())
    }

    fn screencopy_state(&mut self) -> &mut crate::protocols::screencopy::ScreencopyManagerState {
        &mut self.screencopy_state
    }
}

crate::delegate_screencopy!(ShojiWM);
crate::delegate_tearing_control!(ShojiWM);
crate::delegate_wlr_foreign_toplevel!(ShojiWM);
crate::workspace_manager::delegate_ext_workspace_manager!(ShojiWM);
