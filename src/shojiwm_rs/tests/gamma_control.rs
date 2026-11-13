//! wlr-gamma-control on an output whose gamma cannot be set: the test
//! compositor's output has no CRTC, so a control for it fails at once, and
//! the client may keep using it without error.

mod common;

use std::cell::RefCell;

use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_output};
use wayland_protocols_wlr::gamma_control::v1::client::{
    zwlr_gamma_control_manager_v1::ZwlrGammaControlManagerV1,
    zwlr_gamma_control_v1::{self, ZwlrGammaControlV1},
};

thread_local! {
    static EVENTS: RefCell<Vec<zwlr_gamma_control_v1::Event>> = const { RefCell::new(Vec::new()) };
}

#[test]
fn gamma_control_fails_on_an_output_without_gamma() {
    let mut session = common::Session::new(|| {});
    let qh = session.queue.handle();
    let output: wl_output::WlOutput = session.bind("wl_output", 1);
    let manager: ZwlrGammaControlManagerV1 = session.bind("zwlr_gamma_control_manager_v1", 1);
    let control = manager.get_gamma_control(&output, &qh, ());
    session.roundtrip();
    EVENTS.with(|events| {
        assert!(matches!(
            events.borrow().as_slice(),
            [zwlr_gamma_control_v1::Event::Failed]
        ));
    });

    // A failed control is inert: setting gamma through it is no protocol error.
    let file = common::tempfile("gamma");
    control.set_gamma(std::os::fd::AsFd::as_fd(&file));
    control.destroy();
    session.roundtrip();
    assert!(session.connection.protocol_error().is_none());
}

impl Dispatch<ZwlrGammaControlManagerV1, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &ZwlrGammaControlManagerV1,
        _: <ZwlrGammaControlManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrGammaControlV1, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &ZwlrGammaControlV1,
        event: zwlr_gamma_control_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        EVENTS.with(|events| events.borrow_mut().push(event));
    }
}
