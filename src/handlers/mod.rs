mod compositor;
mod layer_shell;
mod xdg_shell;

//
// Wl Seat
//

use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::desktop::{PopupKind, WindowSurfaceType, layer_map_for_output};
use smithay::reexports::wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
    Mode as KdeDecorationMode, OrgKdeKwinServerDecoration,
};
use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, backend::ClientId, delegate_dispatch,
    delegate_global_dispatch,
};
use smithay::utils::{Logical, Rectangle};
use smithay::utils::Serial;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::background_effect::{Capability, ExtBackgroundEffectHandler};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, ImportNotifier};
use smithay::wayland::fractional_scale::{with_fractional_scale, FractionalScaleHandler};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodSeat, PopupSurface};
use smithay::wayland::shell::kde::decoration::KdeDecorationHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::wayland::text_input::TextInputSeat;
use smithay::wayland::virtual_keyboard::{
    VirtualKeyboardManagerGlobalData, VirtualKeyboardManagerState, VirtualKeyboardUserData,
};
use smithay::{
    delegate_commit_timing, delegate_cursor_shape, delegate_data_control, delegate_data_device,
    delegate_dmabuf, delegate_fifo, delegate_fixes, delegate_fractional_scale,
    delegate_input_method_manager, delegate_kde_decoration, delegate_layer_shell, delegate_output,
    delegate_presentation, delegate_primary_selection, delegate_seat, delegate_single_pixel_buffer,
    delegate_text_input_manager, delegate_viewporter, delegate_xdg_decoration,
};
use smithay::delegate_background_effect;
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
        self.cursor_status = image;
        self.schedule_redraw();
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
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

delegate_seat!(ShojiWM);
delegate_cursor_shape!(ShojiWM);
delegate_xdg_decoration!(ShojiWM);
delegate_layer_shell!(ShojiWM);
delegate_presentation!(ShojiWM);
delegate_fifo!(ShojiWM);
delegate_commit_timing!(ShojiWM);
delegate_viewporter!(ShojiWM);
delegate_fractional_scale!(ShojiWM);
delegate_single_pixel_buffer!(ShojiWM);
delegate_fixes!(ShojiWM);
delegate_text_input_manager!(ShojiWM);
delegate_input_method_manager!(ShojiWM);
delegate_kde_decoration!(ShojiWM);
delegate_background_effect!(ShojiWM);
delegate_global_dispatch!(ShojiWM: [ZwpVirtualKeyboardManagerV1: VirtualKeyboardManagerGlobalData] => VirtualKeyboardManagerState);
delegate_dispatch!(ShojiWM: [ZwpVirtualKeyboardManagerV1: ()] => VirtualKeyboardManagerState);

impl Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardUserData<ShojiWM>, ShojiWM> for ShojiWM {
    fn request(
        state: &mut Self,
        client: &Client,
        virtual_keyboard: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        data: &VirtualKeyboardUserData<Self>,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let should_flush_forwarded_virtual_keyboard = matches!(
            &request,
            zwp_virtual_keyboard_v1::Request::Key { .. }
                | zwp_virtual_keyboard_v1::Request::Modifiers { .. }
        );

        <VirtualKeyboardManagerState as Dispatch<
            ZwpVirtualKeyboardV1,
            VirtualKeyboardUserData<ShojiWM>,
            ShojiWM,
        >>::request(state, client, virtual_keyboard, request, data, dh, data_init);

        if should_flush_forwarded_virtual_keyboard && state.seat.input_method().keyboard_grabbed() {
            let mut active_text_input = false;
            state.seat.text_input().with_active_text_input(|_, _| {
                active_text_input = true;
            });
            if active_text_input {
                let _ = state.display_handle.flush_clients();
            }
        }
    }

    fn destroyed(
        state: &mut Self,
        client: ClientId,
        virtual_keyboard: &ZwpVirtualKeyboardV1,
        data: &VirtualKeyboardUserData<Self>,
    ) {
        <VirtualKeyboardManagerState as Dispatch<
            ZwpVirtualKeyboardV1,
            VirtualKeyboardUserData<ShojiWM>,
            ShojiWM,
        >>::destroyed(state, client, virtual_keyboard, data);
    }
}

impl FractionalScaleHandler for ShojiWM {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

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

    fn new_decoration(&mut self, _surface: &WlSurface, decoration: &OrgKdeKwinServerDecoration) {
        decoration.mode(KdeDecorationMode::Server);
    }

    fn request_mode(
        &mut self,
        _surface: &WlSurface,
        decoration: &OrgKdeKwinServerDecoration,
        _mode: smithay::reexports::wayland_server::WEnum<KdeDecorationMode>,
    ) {
        decoration.mode(KdeDecorationMode::Server);
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

delegate_dmabuf!(ShojiWM);

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

delegate_data_device!(ShojiWM);

impl PrimarySelectionHandler for ShojiWM {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

delegate_primary_selection!(ShojiWM);

impl DataControlHandler for ShojiWM {
    fn data_control_state(&mut self) -> &mut DataControlState {
        &mut self.data_control_state
    }
}

delegate_data_control!(ShojiWM);

//
// Wl Output & Xdg Output
//

impl OutputHandler for ShojiWM {}
delegate_output!(ShojiWM);
