//! A tag a client sets with xdg-toplevel-tag-v1 reaches the config as the
//! window's `tag`, also when it is set after the window was mapped.

mod common;

use std::cell::RefCell;

use shojiwm_rs::prelude::*;
use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_compositor};
use wayland_protocols::xdg::{
    shell::client::{xdg_toplevel, xdg_wm_base},
    toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1,
};

thread_local! {
    static TAGS: RefCell<Vec<Option<String>>> = const { RefCell::new(Vec::new()) };
}

fn setup() {
    COMPOSITOR.window.composition(|window| {
        let tag = window.tag().map(|tag| {
            TAGS.with(|tags| tags.borrow_mut().push(tag.clone()));
            tag.clone().unwrap_or_default()
        });
        Flex::column()
            .child(Label::new(tag))
            .child(ClientWindow::new())
    });
}

fn last_tag() -> Option<Option<String>> {
    TAGS.with(|tags| tags.borrow().last().cloned())
}

#[test]
fn toplevel_tag_reaches_the_config() {
    let mut session = common::Session::new(setup);
    let qh = session.queue.handle();
    let compositor: wl_compositor::WlCompositor = session.bind("wl_compositor", 1);
    let shell: xdg_wm_base::XdgWmBase = session.bind("xdg_wm_base", 1);
    let tags: XdgToplevelTagManagerV1 = session.bind("xdg_toplevel_tag_manager_v1", 1);

    // Clients usually tag a toplevel before its first commit.
    let first = compositor.create_surface(&qh, ());
    let toplevel: xdg_toplevel::XdgToplevel =
        shell.get_xdg_surface(&first, &qh, ()).get_toplevel(&qh, ());
    tags.set_toplevel_tag(&toplevel, "main window".into());
    first.commit();
    session.roundtrip();
    session.attach_buffer(&first, 100, 100);
    session.state.refresh_window_decorations().unwrap();
    assert_eq!(last_tag(), Some(Some("main window".into())));

    // A later tag reaches the config too.
    tags.set_toplevel_tag(&toplevel, "settings".into());
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();
    assert_eq!(last_tag(), Some(Some("settings".into())));
    let window = session.state.space.elements().next().cloned().unwrap();
    assert_eq!(
        session.state.snapshot_window(&window).tag.as_deref(),
        Some("settings")
    );
}

impl Dispatch<XdgToplevelTagManagerV1, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &XdgToplevelTagManagerV1,
        _: <XdgToplevelTagManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
