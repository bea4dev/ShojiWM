mod compositor;
mod layer_shell;
mod xdg_shell;

//
// Wl Seat
//

use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::Serial;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, ImportNotifier};
use smithay::wayland::fractional_scale::{with_fractional_scale, FractionalScaleHandler};
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::selection::wlr_data_control::{DataControlHandler, DataControlState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{
    delegate_commit_timing, delegate_cursor_shape, delegate_data_control, delegate_data_device,
    delegate_dmabuf, delegate_fifo, delegate_fixes, delegate_fractional_scale, delegate_layer_shell,
    delegate_output, delegate_presentation, delegate_primary_selection, delegate_seat,
    delegate_single_pixel_buffer, delegate_viewporter,
    delegate_xdg_decoration,
};
use smithay::{backend::{allocator::dmabuf::Dmabuf, renderer::ImportDma}};

use crate::state::ShojiWM;

impl SeatHandler for ShojiWM {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<ShojiWM> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: smithay::input::pointer::CursorImageStatus) {
        self.cursor_status = image;
        self.schedule_redraw();
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
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

impl FractionalScaleHandler for ShojiWM {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }

        smithay::wayland::compositor::with_states(&surface, |states| {
            let primary_scanout_output = smithay::desktop::utils::surface_primary_scanout_output(&surface, states)
                .or_else(|| {
                            if root != surface {
                        smithay::wayland::compositor::with_states(&root, |states| {
                            smithay::desktop::utils::surface_primary_scanout_output(&root, states).or_else(|| {
                                self.space
                                    .elements()
                                    .find(|window| window.toplevel().is_some_and(|toplevel| toplevel.wl_surface() == &root))
                                    .cloned()
                                    .and_then(|window| {
                                    self.space.outputs_for_element(&window).first().cloned()
                                })
                            })
                        })
                    } else {
                        self.space
                            .elements()
                            .find(|window| window.toplevel().is_some_and(|toplevel| toplevel.wl_surface() == &root))
                            .cloned()
                            .and_then(|window| self.space.outputs_for_element(&window).first().cloned())
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
