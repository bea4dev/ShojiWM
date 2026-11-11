//! Window fields the compositor fills in after a window first appears reach
//! the config through its snapshots.

use shojiwm_rs::{
    RuntimeBoot, RuntimeHandle, RuntimeHost,
    cli::CommonArgs,
    prelude::*,
    ssd::{DecorationNode, DecorationNodeKind, WaylandWindowSnapshot},
};

fn snapshot(foreign_toplevel_id: Option<&str>) -> WaylandWindowSnapshot {
    WaylandWindowSnapshot {
        pid: None,
        output_name: None,
        plasma: None,
        minimized_geometry: None,
        foreign_toplevel_id: foreign_toplevel_id.map(Into::into),
        id: "w1".into(),
        title: "title".into(),
        app_id: Some("test".into()),
        position: Default::default(),
        rect: Default::default(),
        is_focused: true,
        is_floating: true,
        is_maximized: false,
        is_fullscreen: false,
        is_xwayland: false,
        decoration: Default::default(),
        size_constraints: Default::default(),
        is_resizable: true,
        is_transient: false,
        parent_id: None,
        icon: None,
        interaction: Default::default(),
    }
}

fn setup() {
    COMPOSITOR.window.composition(|window| {
        let foreign = window
            .foreign_toplevel_id()
            .map(|id| id.clone().unwrap_or_else(|| "unmapped".into()));
        Flex::column()
            .child(Label::new(foreign))
            .child(ClientWindow::new())
    });
}

fn start() -> RuntimeHandle {
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args)
        .launch(RuntimeHost::detached());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime
}

fn label_text(node: &DecorationNode) -> Option<String> {
    if let DecorationNodeKind::Label(label) = &node.kind {
        return Some(label.text.clone());
    }
    node.children.iter().find_map(label_text)
}

#[test]
fn foreign_toplevel_id_follows_the_snapshot() {
    let mut runtime = start();
    let first = runtime.evaluate_window(&snapshot(None), 1).unwrap();
    assert_eq!(label_text(&first.node).as_deref(), Some("unmapped"));

    let mapped = runtime
        .evaluate_window(&snapshot(Some("Zq3f8Kx0")), 2)
        .unwrap();
    assert_eq!(label_text(&mapped.node).as_deref(), Some("Zq3f8Kx0"));
}
