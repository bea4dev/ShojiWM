//! The content type a client declares with wp-content-type-v1 reaches the
//! config as the window's `content_type`, and changes with later commits.

mod common;

use std::cell::RefCell;

use shojiwm_rs::prelude::*;
use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_compositor};
use wayland_protocols::{
    wp::content_type::v1::client::{
        wp_content_type_manager_v1::WpContentTypeManagerV1,
        wp_content_type_v1::{Type, WpContentTypeV1},
    },
    xdg::shell::client::{xdg_toplevel, xdg_wm_base},
};

thread_local! {
    static SEEN: RefCell<Vec<Option<WindowContentType>>> = const { RefCell::new(Vec::new()) };
}

fn setup() {
    COMPOSITOR.window.composition(|window| {
        let label = window.content_type().map(|content_type| {
            SEEN.with(|seen| seen.borrow_mut().push(*content_type));
            format!("{content_type:?}")
        });
        Flex::column()
            .child(Label::new(label))
            .child(ClientWindow::new())
    });
}

fn last_seen() -> Option<Option<WindowContentType>> {
    SEEN.with(|seen| seen.borrow().last().copied())
}

#[test]
fn content_type_reaches_the_config() {
    let mut session = common::Session::new(setup);
    let qh = session.queue.handle();
    let compositor: wl_compositor::WlCompositor = session.bind("wl_compositor", 1);
    let shell: xdg_wm_base::XdgWmBase = session.bind("xdg_wm_base", 1);
    let manager: WpContentTypeManagerV1 = session.bind("wp_content_type_manager_v1", 1);

    let surface = compositor.create_surface(&qh, ());
    let _toplevel: xdg_toplevel::XdgToplevel = shell
        .get_xdg_surface(&surface, &qh, ())
        .get_toplevel(&qh, ());
    let content_type = manager.get_surface_content_type(&surface, &qh, ());
    content_type.set_content_type(Type::Video);
    surface.commit();
    session.roundtrip();
    session.attach_buffer(&surface, 100, 100);
    session.state.refresh_window_decorations().unwrap();
    assert_eq!(last_seen(), Some(Some(WindowContentType::Video)));

    // The type is double-buffered: it changes with the next commit.
    content_type.set_content_type(Type::Game);
    surface.commit();
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();
    assert_eq!(last_seen(), Some(Some(WindowContentType::Game)));
    let window = session.state.space.elements().next().cloned().unwrap();
    assert_eq!(
        session.state.snapshot_window(&window).content_type,
        Some(WindowContentType::Game)
    );

    content_type.set_content_type(Type::None);
    surface.commit();
    session.roundtrip();
    session.state.refresh_window_decorations().unwrap();
    assert_eq!(last_seen(), Some(None));
}

impl Dispatch<WpContentTypeManagerV1, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &WpContentTypeManagerV1,
        _: <WpContentTypeManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WpContentTypeV1, ()> for common::Client {
    fn event(
        _: &mut Self,
        _: &WpContentTypeV1,
        _: <WpContentTypeV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
