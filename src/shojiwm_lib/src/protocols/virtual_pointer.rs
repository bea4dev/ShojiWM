//! `zwlr_virtual_pointer_manager_v1` (wlr-virtual-pointer-unstable-v1).
//!
//! Lets a client move the pointer, press buttons and scroll as if it were a
//! device: `wlrctl pointer`, remote desktop servers, test drivers. The
//! requests become [`crate::virtual_input`] events and take the same path as
//! libinput's. A pointer made with `create_virtual_pointer_with_output`
//! maps `motion_absolute` onto that output instead of the whole layout.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use smithay::backend::input::{Axis, AxisSource, ButtonState, InputEvent};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::{
    zwlr_virtual_pointer_manager_v1::{self, ZwlrVirtualPointerManagerV1},
    zwlr_virtual_pointer_v1::{self, ZwlrVirtualPointerV1},
};
use smithay::reexports::wayland_server::protocol::wl_pointer;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::utils::{Logical, Rectangle};

use crate::virtual_input::{
    AxisFrame, VirtualAxis, VirtualButton, VirtualDevice, VirtualInput, VirtualMotion,
    VirtualMotionAbsolute, absolute_fraction,
};

const VERSION: u32 = 2;

pub trait VirtualPointerHandler {
    fn virtual_pointer_input(&mut self, event: InputEvent<VirtualInput>);
    /// The space `motion_absolute` without an output spans.
    fn virtual_pointer_layout(&self) -> Option<Rectangle<i32, Logical>>;
    fn virtual_pointer_output_geometry(&self, output: &Output) -> Option<Rectangle<i32, Logical>>;
}

#[derive(Debug)]
pub struct VirtualPointerData {
    device: VirtualDevice,
    output: Option<Output>,
    frame: Mutex<AxisFrame>,
}

#[derive(Debug, Default)]
pub struct VirtualPointerManagerState;

impl VirtualPointerManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrVirtualPointerManagerV1, ()>,
        D: Dispatch<ZwlrVirtualPointerManagerV1, ()>,
        D: Dispatch<ZwlrVirtualPointerV1, VirtualPointerData>,
        D: 'static,
    {
        display.create_global::<D, ZwlrVirtualPointerManagerV1, _>(VERSION, ());
        Self
    }
}

fn time_usec(time_msec: u32) -> u64 {
    u64::from(time_msec) * 1000
}

fn wire_axis(pointer: &ZwlrVirtualPointerV1, axis: WEnum<wl_pointer::Axis>) -> Option<Axis> {
    match axis {
        WEnum::Value(wl_pointer::Axis::HorizontalScroll) => Some(Axis::Horizontal),
        WEnum::Value(wl_pointer::Axis::VerticalScroll) => Some(Axis::Vertical),
        _ => {
            pointer.post_error(zwlr_virtual_pointer_v1::Error::InvalidAxis, "unknown axis");
            None
        }
    }
}

impl<D> GlobalDispatch<ZwlrVirtualPointerManagerV1, (), D> for VirtualPointerManagerState
where
    D: GlobalDispatch<ZwlrVirtualPointerManagerV1, ()>,
    D: Dispatch<ZwlrVirtualPointerManagerV1, ()>,
    D: Dispatch<ZwlrVirtualPointerV1, VirtualPointerData>,
    D: VirtualPointerHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrVirtualPointerManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<ZwlrVirtualPointerManagerV1, (), D> for VirtualPointerManagerState
where
    D: Dispatch<ZwlrVirtualPointerManagerV1, ()>,
    D: Dispatch<ZwlrVirtualPointerV1, VirtualPointerData>,
    D: VirtualPointerHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _manager: &ZwlrVirtualPointerManagerV1,
        request: zwlr_virtual_pointer_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // ShojiWM has one seat, so the requested one is not consulted.
        let (id, output) = match request {
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointer { seat: _, id } => {
                (id, None)
            }
            zwlr_virtual_pointer_manager_v1::Request::CreateVirtualPointerWithOutput {
                seat: _,
                output,
                id,
            } => (id, output.as_ref().and_then(Output::from_resource)),
            zwlr_virtual_pointer_manager_v1::Request::Destroy => return,
            _ => return,
        };
        static NEXT_POINTER: AtomicU64 = AtomicU64::new(1);
        let pointer_id = NEXT_POINTER.fetch_add(1, Ordering::Relaxed);
        data_init.init(
            id,
            VirtualPointerData {
                device: VirtualDevice {
                    id: format!("zwlr-virtual-pointer-{pointer_id}"),
                    name: "wlr virtual pointer".into(),
                    keyboard: false,
                },
                output,
                frame: Mutex::new(AxisFrame::default()),
            },
        );
    }
}

impl<D> Dispatch<ZwlrVirtualPointerV1, VirtualPointerData, D> for VirtualPointerManagerState
where
    D: Dispatch<ZwlrVirtualPointerV1, VirtualPointerData>,
    D: VirtualPointerHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        pointer: &ZwlrVirtualPointerV1,
        request: zwlr_virtual_pointer_v1::Request,
        data: &VirtualPointerData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let device = data.device.clone();
        match request {
            zwlr_virtual_pointer_v1::Request::Motion { time, dx, dy } => {
                state.virtual_pointer_input(InputEvent::PointerMotion {
                    event: VirtualMotion {
                        device,
                        time: time_usec(time),
                        dx,
                        dy,
                    },
                });
            }
            zwlr_virtual_pointer_v1::Request::MotionAbsolute {
                time,
                x,
                y,
                x_extent,
                y_extent,
            } => {
                let Some(layout) = state.virtual_pointer_layout() else {
                    return;
                };
                let output = data
                    .output
                    .as_ref()
                    .and_then(|output| state.virtual_pointer_output_geometry(output));
                let Some((x, y)) = absolute_fraction(x, y, x_extent, y_extent, output, layout)
                else {
                    return;
                };
                state.virtual_pointer_input(InputEvent::PointerMotionAbsolute {
                    event: VirtualMotionAbsolute {
                        device,
                        time: time_usec(time),
                        x,
                        y,
                    },
                });
            }
            zwlr_virtual_pointer_v1::Request::Button {
                time,
                button,
                state: button_state,
            } => {
                let button_state = match button_state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => ButtonState::Pressed,
                    WEnum::Value(wl_pointer::ButtonState::Released) => ButtonState::Released,
                    _ => return,
                };
                state.virtual_pointer_input(InputEvent::PointerButton {
                    event: VirtualButton {
                        device,
                        time: time_usec(time),
                        button,
                        state: button_state,
                    },
                });
            }
            zwlr_virtual_pointer_v1::Request::Axis { time, axis, value } => {
                if let Some(axis) = wire_axis(pointer, axis) {
                    let mut frame = data.frame.lock().unwrap();
                    frame.time_msec = time;
                    frame.add(axis, value);
                }
            }
            zwlr_virtual_pointer_v1::Request::AxisSource { axis_source } => {
                let source = match axis_source {
                    WEnum::Value(wl_pointer::AxisSource::Wheel) => AxisSource::Wheel,
                    WEnum::Value(wl_pointer::AxisSource::Finger) => AxisSource::Finger,
                    WEnum::Value(wl_pointer::AxisSource::Continuous) => AxisSource::Continuous,
                    WEnum::Value(wl_pointer::AxisSource::WheelTilt) => AxisSource::WheelTilt,
                    _ => {
                        pointer.post_error(
                            zwlr_virtual_pointer_v1::Error::InvalidAxisSource,
                            "unknown axis source",
                        );
                        return;
                    }
                };
                data.frame.lock().unwrap().source = Some(source);
            }
            zwlr_virtual_pointer_v1::Request::AxisStop { time, axis } => {
                if let Some(axis) = wire_axis(pointer, axis) {
                    let mut frame = data.frame.lock().unwrap();
                    frame.time_msec = time;
                    frame.stop(axis);
                }
            }
            zwlr_virtual_pointer_v1::Request::AxisDiscrete {
                time,
                axis,
                value,
                discrete,
            } => {
                if let Some(axis) = wire_axis(pointer, axis) {
                    let mut frame = data.frame.lock().unwrap();
                    frame.time_msec = time;
                    frame.add_discrete(axis, value, discrete);
                }
            }
            zwlr_virtual_pointer_v1::Request::Frame => {
                let frame = std::mem::take(&mut *data.frame.lock().unwrap());
                if !frame.is_empty() {
                    state.virtual_pointer_input(InputEvent::PointerAxis {
                        event: VirtualAxis {
                            device,
                            time: time_usec(frame.time_msec),
                            frame,
                        },
                    });
                }
            }
            zwlr_virtual_pointer_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

/// Delegate the `zwlr_virtual_pointer_manager_v1` global to
/// [`VirtualPointerManagerState`].
#[macro_export]
macro_rules! delegate_virtual_pointer {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1: ()
        ] => $crate::protocols::virtual_pointer::VirtualPointerManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1: ()
        ] => $crate::protocols::virtual_pointer::VirtualPointerManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::virtual_pointer::v1::server::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1: $crate::protocols::virtual_pointer::VirtualPointerData
        ] => $crate::protocols::virtual_pointer::VirtualPointerManagerState);
    };
}
