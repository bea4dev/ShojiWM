//! The stacking order that org_kde_plasma_window_management gives is the
//! order windows are drawn in: a raised window moves to the top, and the
//! clients are told.

mod common;

use std::cell::RefCell;

use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::plasma_window_management::client::org_kde_plasma_window_management::{
    self, OrgKdePlasmaWindowManagement,
};

thread_local! {
    /// The windows announced, in the order they came.
    static UUIDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// The last stacking order sent, bottom to top.
    static STACKING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

impl Dispatch<OrgKdePlasmaWindowManagement, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &OrgKdePlasmaWindowManagement,
        event: org_kde_plasma_window_management::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            org_kde_plasma_window_management::Event::WindowWithUuid { uuid, .. } => {
                UUIDS.with(|uuids| uuids.borrow_mut().push(uuid));
            }
            org_kde_plasma_window_management::Event::StackingOrderUuidChanged { uuids } => {
                STACKING.with(|stacking| {
                    *stacking.borrow_mut() = uuids.split(';').map(String::from).collect();
                });
            }
            _ => {}
        }
    }
}

fn setup() {}

fn stacking() -> Vec<String> {
    STACKING.with(|stacking| stacking.borrow().clone())
}

#[test]
fn a_raised_window_is_on_top_of_the_stacking_order() {
    let mut session = common::Session::new(setup);
    // (version 16: the last that sends the order as a string)
    let _manager: OrgKdePlasmaWindowManagement =
        session.bind("org_kde_plasma_window_management", 16);
    session.roundtrip();
    session.map_toplevel_with_buffer("a", 200, 200);
    session.map_toplevel_with_buffer("b", 200, 200);
    session.roundtrip();

    let uuids = UUIDS.with(|uuids| uuids.borrow().clone());
    assert_eq!(uuids.len(), 2, "both windows are announced");
    let (a, b) = (uuids[0].clone(), uuids[1].clone());
    assert_eq!(
        stacking(),
        [a.clone(), b.clone()],
        "the newest window is on top"
    );

    let bottom = session
        .state
        .space
        .elements()
        .next()
        .cloned()
        .expect("a window in the space");
    session.state.space.raise_element(&bottom, true);
    session.state.sync_taskbar_window_states();
    session.roundtrip();
    assert_eq!(stacking(), [b, a], "the raised window is on top");
}
