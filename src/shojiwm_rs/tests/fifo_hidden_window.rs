//! A window the config hides is drawn on no output, yet its wp_fifo_v1
//! barriers are still cleared at each output refresh. A FIFO-mode client
//! commits with set_barrier, then with wait_barrier: if the barrier is never
//! signaled, the second commit stays blocked and the client freezes.

mod common;

use shojiwm_rs::prelude::*;
use smithay::{
    desktop::Window as SpaceWindow,
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface as ServerSurface},
    wayland::compositor::{SurfaceAttributes, with_states},
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_compositor, wl_region},
};
use wayland_protocols::wp::fifo::v1::client::{wp_fifo_manager_v1, wp_fifo_v1};

const SIZE: i32 = 200;

/// Every window is kept mapped but hidden, as a minimized one.
fn setup() {
    COMPOSITOR.window.composition(|_| {
        ManagedWindow::new()
            .rect(Rect::new(0.0, 0.0, SIZE as f64, SIZE as f64))
            .visible(false)
            .child(ClientWindow::new())
    });
}

fn server_window(session: &common::Session, protocol_id: u32) -> SpaceWindow {
    session
        .state
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|toplevel| toplevel.wl_surface().id().protocol_id() == protocol_id)
        })
        .cloned()
        .expect("the toplevel is not in the space")
}

/// The opaque region stands for the content of the second commit: unlike the
/// buffer, which the commit handler takes, it stays in the current state.
fn has_opaque_region(surface: &ServerSurface) -> bool {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<SurfaceAttributes>()
            .current()
            .opaque_region
            .is_some()
    })
}

#[test]
fn fifo_barrier_of_a_hidden_window_is_cleared_at_output_refresh() {
    let mut session = common::Session::new(setup);
    let surface = session.map_toplevel_with_buffer("hidden", SIZE, SIZE);
    // A frame re-reads the windows first; the config's `visible(false)` lands then.
    session.state.refresh_window_decorations().unwrap();
    session.roundtrip();

    let window = server_window(&session, surface.id().protocol_id());
    let server_surface = window.toplevel().unwrap().wl_surface().clone();
    assert!(
        !session.state.window_allows_render(&window),
        "the config must hide the window, else it is drawn and the old path clears it"
    );

    let qh = session.queue.handle();
    let fifo_manager: wp_fifo_manager_v1::WpFifoManagerV1 = session.bind("wp_fifo_manager_v1", 1);
    let fifo = fifo_manager.get_fifo(&surface, &qh, ());
    let compositor: wl_compositor::WlCompositor = session.bind("wl_compositor", 1);

    // First content update: sets the barrier, waits for nothing, so it applies.
    fifo.set_barrier();
    surface.commit();
    session.roundtrip();
    assert!(!has_opaque_region(&server_surface));

    // Second content update: waits for the barrier of the first one.
    let region = compositor.create_region(&qh, ());
    region.add(0, 0, SIZE, SIZE);
    surface.set_opaque_region(Some(&region));
    fifo.wait_barrier();
    surface.commit();
    session.roundtrip();
    // Without this, the test passes vacuously if some other path signals the barrier.
    assert!(
        !has_opaque_region(&server_surface),
        "the second commit must be blocked until the barrier is signaled"
    );

    // What a backend does after each repaint of an output.
    let output = session.state.space.outputs().next().cloned().unwrap();
    session.state.signal_post_repaint_barriers(&output);
    session.roundtrip();

    assert!(
        has_opaque_region(&server_surface),
        "the hidden window's FIFO barrier was not cleared: its commit stays blocked"
    );
}

macro_rules! ignore_events {
    ($($proxy:ty),*) => {$(
        impl Dispatch<$proxy, ()> for common::Client {
            fn event(_: &mut Self, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore_events!(
    wp_fifo_manager_v1::WpFifoManagerV1,
    wp_fifo_v1::WpFifoV1,
    wl_region::WlRegion
);
