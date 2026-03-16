mod compositor;
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
use smithay::wayland::selection::data_device::{
    set_data_device_focus, DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::tablet_manager::TabletSeatHandler;
use smithay::{
    delegate_cursor_shape, delegate_data_device, delegate_output, delegate_seat,
    delegate_xdg_decoration,
};

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
        set_data_device_focus(dh, seat, client);
    }
}

delegate_seat!(ShojiWM);
delegate_cursor_shape!(ShojiWM);
delegate_xdg_decoration!(ShojiWM);

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

//
// Wl Output & Xdg Output
//

impl OutputHandler for ShojiWM {}
delegate_output!(ShojiWM);
