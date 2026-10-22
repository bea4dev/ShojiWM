//! Drives the reactive Rust runtime through the same `RuntimeHandle` the
//! compositor uses, and checks that signal changes come back as the minimal
//! updates the compositor expects.

use shojiwm_rs::{
    HostMessage, RuntimeBoot, RuntimeHandle, RuntimeHost,
    cli::CommonArgs,
    keyboard_layout::{KeyboardLayoutSnapshot, KeyboardLocksSnapshot},
    prelude::*,
    runtime_api::CompositionPatch,
    ssd::{
        DecorationNode, DecorationNodeKind, PopupNode, WaylandWindowAction, WaylandWindowSnapshot,
        WindowAction,
    },
};

static WIDTH: WindowStateKey<f64> = WindowStateKey::new("width", |_| 400.0);

fn snapshot(id: &str, title: &str, focused: bool) -> WaylandWindowSnapshot {
    WaylandWindowSnapshot {
        output_name: None,
        plasma: None,
        minimized_geometry: None,
        id: id.into(),
        title: title.into(),
        app_id: Some("test".into()),
        position: Default::default(),
        rect: Default::default(),
        is_focused: focused,
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

fn close_button(window: Window) -> Element {
    let hover = signal(false);
    let border = hover.map(|hover| {
        if *hover {
            hex("#00000000")
        } else {
            hex("#F0808030")
        }
    });
    Flex::row()
        .style(Style::new().relative())
        .child(
            Button::new()
                .on_hover_change(move |value| hover.set(value))
                .on_click(move || window.close())
                .style(Style::new().size(16.0, 16.0).border(1.0, border)),
        )
        .child_dyn(move || {
            hover
                .get()
                .then(|| Image::new("/assets/x.svg").style(Style::new().absolute()))
        })
}

fn setup() {
    COMPOSITOR.key.bind("grow", "Super+G", || {
        if let Some(window) = COMPOSITOR.window.list().first() {
            window.state(&WIDTH).update(|width| *width += 100.0);
        }
    });
    COMPOSITOR.key.bind("glow", "Super+H", || {
        GLOW.with(|glow| glow.set(1.0));
    });
    COMPOSITOR.window.composition(|window| {
        let width = window.state(&WIDTH);
        let border = window.is_focused().map(|focused| {
            if *focused {
                hex("#d7ba7d")
            } else {
                hex("#4f5666")
            }
        });
        let glow = GLOW.with(|glow| *glow);
        ManagedWindow::new()
            .rect(derive(move || Rect::new(0.0, 0.0, width.get(), 300.0)))
            .child(
                WindowBorder::new()
                    .style(Style::new().border(2.0, border).box_shadow(shadow(
                        0.0,
                        4.0,
                        12.0,
                        hex("#00000060"),
                    )))
                    .overlay(paint_shader("/ring.frag").uniform("glow", glow))
                    .child(
                        Flex::column()
                            .child(
                                ShaderEffect::new(
                                    Effect::new(backdrop_source())
                                        .stage(dual_kawase_blur(4, 2))
                                        .stage(shader_stage("/glass.frag").uniform("glow", glow)),
                                )
                                .direction(Direction::Row)
                                .child(Label::new(window.title()))
                                .child(close_button(window)),
                            )
                            .child(ClientWindow::new()),
                    ),
            )
    });
}

thread_local! {
    static GLOW: Signal<f64> = signal(0.0);
}

fn start() -> (RuntimeHandle, RuntimeHost) {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    (runtime, host)
}

fn find<'a>(
    node: &'a DecorationNode,
    predicate: &dyn Fn(&DecorationNode) -> bool,
) -> Option<&'a DecorationNode> {
    if predicate(node) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find(child, predicate))
}

fn label_text(node: &DecorationNode) -> Option<String> {
    find(node, &|node| {
        matches!(node.kind, DecorationNodeKind::Label(_))
    })
    .map(|node| match &node.kind {
        DecorationNodeKind::Label(label) => label.text.clone(),
        _ => unreachable!(),
    })
}

#[test]
fn reactive_config_round_trip() {
    let (mut runtime, host) = start();
    let bindings: Vec<String> = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::KeyBindings(update) => Some(
                update
                    .entries
                    .into_iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(bindings, ["grow", "glow"]);

    // First evaluation: a full tree with the title and the managed rect.
    let evaluation = runtime
        .evaluate_window(&snapshot("w1", "hello", true), 1)
        .unwrap();
    assert_eq!(label_text(&evaluation.node).as_deref(), Some("hello"));
    assert_eq!(evaluation.managed_window.rect.unwrap().width, 400.0);
    let border = find(&evaluation.node, &|node| {
        matches!(node.kind, DecorationNodeKind::WindowBorder)
    })
    .unwrap();
    assert_eq!(border.style.border.unwrap().color, hex("#d7ba7d"));
    assert_eq!(
        border.style.box_shadow,
        vec![shadow(0.0, 4.0, 12.0, hex("#00000060"))]
    );
    let overlay = border.style.overlay.as_ref().expect("overlay paint shader");
    assert!(overlay.shader.path.ends_with("ring.frag"));
    let button = find(&evaluation.node, &|node| {
        matches!(node.kind, DecorationNodeKind::Button(_))
    })
    .unwrap();
    let hover = button.interaction.hover_change.clone().unwrap();
    let DecorationNodeKind::Button(button_node) = &button.kind else {
        unreachable!()
    };
    let WindowAction::RuntimeHandler(click) = &button_node.action else {
        panic!("click should be a runtime handler")
    };
    let click = click.clone();
    assert!(
        find(&evaluation.node, &|node| matches!(
            node.kind,
            DecorationNodeKind::Image(_)
        ))
        .is_none()
    );

    // Hover: only the button row is re-sent, now with the icon.
    let invocation = runtime
        .invoke_handler("w1", &hover.true_handler, 2)
        .unwrap();
    assert!(invocation.invoked);
    assert_eq!(invocation.dirty_window_ids, ["w1"]);
    let cached = runtime
        .evaluate_cached_window("w1", None, 3, false)
        .unwrap();
    assert!(cached.node.is_none());
    assert!(!cached.managed_window_only);
    let replaced: Vec<&str> = cached
        .node_patches
        .iter()
        .map(CompositionPatch::node_id)
        .collect();
    // The button (border colour) sits inside the row (dynamic icon): one
    // replacement of the row covers both.
    assert_eq!(replaced.len(), 1, "{replaced:?}");
    let row = cached
        .node_patches
        .iter()
        .find_map(|patch| {
            patch
                .replacement_node()
                .filter(|node| node.children.len() == 2)
        })
        .expect("row patch");
    assert!(matches!(row.children[1].kind, DecorationNodeKind::Image(_)));

    // Nothing changed since: an empty update.
    let cached = runtime
        .evaluate_cached_window("w1", None, 4, false)
        .unwrap();
    assert!(cached.node_patches.is_empty() && cached.node.is_none());

    // Click: the close action comes back with the reply.
    let invocation = runtime.invoke_handler("w1", &click, 5).unwrap();
    assert!(
        matches!(invocation.actions.as_slice(), [action] if action.action == WaylandWindowAction::Close)
    );

    // Focus and title flow in through snapshots.
    let evaluation = runtime
        .evaluate_window(&snapshot("w1", "renamed", false), 6)
        .unwrap();
    assert_eq!(label_text(&evaluation.node).as_deref(), Some("renamed"));
    let border = find(&evaluation.node, &|node| {
        matches!(node.kind, DecorationNodeKind::WindowBorder)
    })
    .unwrap();
    assert_eq!(border.style.border.unwrap().color, hex("#4f5666"));

    // A key binding that only touches the managed rect.
    let invocation = runtime.invoke_key_binding("grow", 7).unwrap();
    assert!(invocation.invoked);
    assert_eq!(invocation.dirty_managed_window_ids, ["w1"]);
    let cached = runtime
        .evaluate_cached_window("w1", None, 8, false)
        .unwrap();
    assert!(cached.managed_window_only);
    assert_eq!(cached.managed_window.rect.unwrap().width, 500.0);

    // A reactive uniform: a single-uniform patch, no node rebuild.
    let invocation = runtime.invoke_key_binding("glow", 9).unwrap();
    assert!(invocation.dirty_window_node_ids.contains_key("w1"));
    let cached = runtime
        .evaluate_cached_window("w1", None, 10, false)
        .unwrap();
    assert_eq!(cached.node_patches.len(), 2, "{:?}", cached.node_patches);
    for stage in [1, shojiwm_rs::runtime_api::OVERLAY_STAGE_INDEX] {
        assert!(
            cached.node_patches.iter().any(|patch| matches!(
                patch,
                CompositionPatch::ShaderUniform {
                    name,
                    stage_index,
                    value: shojiwm_rs::ssd::ShaderUniformValue::Float(value),
                    ..
                } if name == "glow" && *value == 1.0 && *stage_index == stage
            )),
            "uniform patch of stage {stage}: {:?}",
            cached.node_patches
        );
    }

    // Closing.
    let invocation = runtime.start_close("w1", 11).unwrap();
    assert!(invocation.invoked);
    assert!(
        invocation
            .actions
            .iter()
            .any(|action| action.action == WaylandWindowAction::FinalizeClose)
    );
    runtime.window_closed("w1").unwrap();
    assert!(
        runtime
            .evaluate_cached_window("w1", None, 12, false)
            .is_err()
    );
}

#[test]
fn timers_and_animations_drive_the_scheduler() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            let progress = Scope::root().run(|| Animation::new(0.0));
            COMPOSITOR.key.bind("animate", "Super+A", move || {
                progress.start(AnimationOptions::to(1.0, 100.0));
            });
            COMPOSITOR.window.composition(move |_| {
                ManagedWindow::new()
                    .opacity(progress)
                    .child(ClientWindow::new())
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime
        .evaluate_window(&snapshot("w1", "a", true), 1000)
        .unwrap();
    let invocation = runtime.invoke_key_binding("animate", 1000).unwrap();
    assert_eq!(invocation.next_poll_in_ms, Some(0));
    let tick = runtime.scheduler_tick(1050.0).unwrap();
    assert_eq!(tick.dirty_managed_window_ids, ["w1"]);
    let cached = runtime
        .evaluate_cached_window("w1", None, 1050, false)
        .unwrap();
    assert!((cached.transform.opacity - 0.5).abs() < 1e-6);
    let tick = runtime.scheduler_tick(1100.0).unwrap();
    assert_eq!(tick.next_poll_in_ms, None);
}

#[test]
fn hovering_a_button_opens_its_popup() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            COMPOSITOR.window.composition(|_| {
                let hover = signal(false);
                ManagedWindow::new().child(
                    Flex::column()
                        .child(
                            Button::new()
                                .on_hover_change(move |on| hover.set(on))
                                .child(
                                    Popup::new()
                                        .open(hover)
                                        .placement(PopupPlacement::Top)
                                        .offset(6.0)
                                        .layer(PopupLayer::Window)
                                        .child(Label::new("Maximize")),
                                ),
                        )
                        .child(ClientWindow::new()),
                )
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let popup_of = |node: &DecorationNode| -> PopupNode {
        match find(node, &|node| {
            matches!(node.kind, DecorationNodeKind::Popup(_))
        })
        .map(|node| &node.kind)
        {
            Some(DecorationNodeKind::Popup(popup)) => *popup,
            _ => panic!("no popup in {node:?}"),
        }
    };
    let evaluation = runtime
        .evaluate_window(&snapshot("w1", "a", true), 1)
        .unwrap();
    let popup = popup_of(&evaluation.node);
    assert_eq!(
        popup,
        PopupNode {
            open: false,
            placement: PopupPlacement::Top,
            offset: 6.0,
            layer: PopupLayer::Window,
            ..PopupNode::default()
        }
    );
    let button = find(&evaluation.node, &|node| {
        matches!(node.kind, DecorationNodeKind::Button(_))
    })
    .unwrap();
    let hover = button.interaction.hover_change.clone().unwrap();

    runtime
        .invoke_handler("w1", &hover.true_handler, 2)
        .unwrap();
    let cached = runtime
        .evaluate_cached_window("w1", None, 3, false)
        .unwrap();
    assert_eq!(cached.node_patches.len(), 1, "{:?}", cached.node_patches);
    let patched = cached.node_patches[0]
        .replacement_node()
        .expect("node patch");
    assert!(popup_of(patched).open);
}

#[test]
fn popup_triggers_follow_compositor_events() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            COMPOSITOR.window.composition(|_| {
                ManagedWindow::new().child(
                    Flex::column()
                        .child(
                            Button::new().child(
                                Popup::new()
                                    .mode(PopupMode::Auto)
                                    .trigger(PopupTrigger::hover(300.0, 100.0))
                                    .child(Button::new().on_click(|| {})),
                            ),
                        )
                        .child(ClientWindow::new()),
                )
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    fn popup_state(node: &DecorationNode) -> Option<(PopupNode, shojiwm_rs::ssd::PopupHandlers)> {
        find(node, &|node| {
            matches!(node.kind, DecorationNodeKind::Popup(_))
        })
        .map(|node| match &node.kind {
            DecorationNodeKind::Popup(popup) => (
                *popup,
                node.interaction
                    .popup
                    .as_deref()
                    .cloned()
                    .unwrap_or_default(),
            ),
            _ => unreachable!(),
        })
    }
    let open_after = |runtime: &mut RuntimeHandle, now: u64| {
        runtime.scheduler_tick(now as f64).unwrap();
        let cached = runtime
            .evaluate_cached_window("w1", None, now, false)
            .unwrap();
        cached
            .node
            .iter()
            .chain(
                cached
                    .node_patches
                    .iter()
                    .filter_map(|patch| patch.replacement_node()),
            )
            .find_map(popup_state)
            .map(|(popup, _)| popup.open)
    };

    let evaluation = runtime
        .evaluate_window(&snapshot("w1", "a", true), 1000)
        .unwrap();
    let (popup, handlers) = popup_state(&evaluation.node).unwrap();
    assert_eq!((popup.mode, popup.open), (PopupMode::Auto, false));
    let interest = handlers
        .interest_change
        .expect("hover trigger listens to interest");
    let dismiss = handlers
        .dismiss
        .expect("auto popups listen to close requests");

    runtime
        .invoke_handler("w1", &interest.true_handler, 1000)
        .unwrap();
    assert_ne!(
        open_after(&mut runtime, 1200),
        Some(true),
        "opened before the delay"
    );
    assert_eq!(open_after(&mut runtime, 1300), Some(true));

    // Leaving and coming back within the close delay keeps it open.
    runtime
        .invoke_handler("w1", &interest.false_handler, 1400)
        .unwrap();
    runtime
        .invoke_handler("w1", &interest.true_handler, 1450)
        .unwrap();
    assert_ne!(open_after(&mut runtime, 1600), Some(false));

    // A close request closes it at once.
    runtime
        .invoke_handler("w1", dismiss.handler_for(PopupDismissReason::Escape), 1700)
        .unwrap();
    assert_eq!(open_after(&mut runtime, 1700), Some(false));
}

#[test]
fn ipc_socket_follows_the_compositor_display_not_the_environment() {
    thread_local! {
        static PATH: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
    }
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    // Nested, WAYLAND_DISPLAY still names the parent compositor while the
    // config loads; the IPC socket must use the compositor's own name.
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            PATH.with(|path| *path.borrow_mut() = Some(shojiwm_rs::ipc::default_socket_path()));
        })),
        &args,
    )
    .launch_with_display(host, Some(std::ffi::OsStr::new("wayland-nested-test")));
    runtime.preload().unwrap();
    let path = PATH.with(|path| path.borrow().clone()).unwrap();
    assert_eq!(
        path.file_name().unwrap(),
        "shojiwm-wayland-nested-test.sock"
    );
}

#[test]
fn workspace_requests_with_a_window_go_to_on_move_window() {
    use shojiwm_rs::runtime_workspace::RuntimeWorkspaceActivateRequestSnapshot;
    use std::{cell::RefCell, rc::Rc};

    let activated = Rc::new(RefCell::new(Vec::new()));
    let moved = Rc::new(RefCell::new(Vec::new()));
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new({
            let activated = activated.clone();
            let moved = moved.clone();
            move || {
                let activated = activated.clone();
                COMPOSITOR.workspace.on_activate(move |event| {
                    activated.borrow_mut().push(event.workspace_id.clone())
                });
                let moved = moved.clone();
                COMPOSITOR.workspace.on_move_window(move |event| {
                    moved
                        .borrow_mut()
                        .push((event.workspace_id.clone(), event.window_id.clone()))
                });
            }
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let request =
        |workspace_id: &str, window_id: Option<&str>| RuntimeWorkspaceActivateRequestSnapshot {
            workspace_id: workspace_id.into(),
            group_id: None,
            window_id: window_id.map(Into::into),
            create_name: None,
        };
    assert!(
        runtime
            .workspace_activate(&request("desk-2", Some("w1")), 1000)
            .unwrap()
            .invoked
    );
    assert!(
        runtime
            .workspace_activate(&request("desk-1", None), 1000)
            .unwrap()
            .invoked
    );
    assert_eq!(*moved.borrow(), [("desk-2".to_string(), "w1".to_string())]);
    assert_eq!(*activated.borrow(), ["desk-1".to_string()]);
}

#[test]
fn workspace_create_requests_go_to_on_create_and_are_advertised() {
    use shojiwm_rs::runtime_workspace::{
        RuntimeWorkspaceActivateRequestSnapshot, RuntimeWorkspaceConfigUpdate,
    };
    use std::{cell::RefCell, rc::Rc};

    let created = Rc::new(RefCell::new(Vec::new()));
    let activated = Rc::new(RefCell::new(0));
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new({
            let created = created.clone();
            let activated = activated.clone();
            move || {
                COMPOSITOR
                    .workspace
                    .configure(RuntimeWorkspaceConfigUpdate::default);
                let created = created.clone();
                COMPOSITOR.workspace.on_create(move |event| {
                    created
                        .borrow_mut()
                        .push((event.group_id.clone(), event.name.clone()))
                });
                let activated = activated.clone();
                COMPOSITOR
                    .workspace
                    .on_activate(move |_| *activated.borrow_mut() += 1);
            }
        })),
        &args,
    )
    .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    // Registering the listener offers `create_workspace` to bars.
    let can_create = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::Workspace(update) => Some(update.can_create),
            _ => None,
        })
        .last();
    assert_eq!(can_create, Some(true));

    let request = RuntimeWorkspaceActivateRequestSnapshot {
        workspace_id: String::new(),
        group_id: Some("eDP-1".into()),
        window_id: None,
        create_name: Some("5".into()),
    };
    assert!(runtime.workspace_activate(&request, 1000).unwrap().invoked);
    assert_eq!(
        *created.borrow(),
        [(Some("eDP-1".to_string()), "5".to_string())]
    );
    assert_eq!(*activated.borrow(), 0);
}

#[test]
fn drag_views_follow_the_drag_and_go_with_their_animation() {
    use shojiwm_lib::dnd::{DndEventSnapshot, DragSnapshot, DragViewCommand};
    use shojiwm_rs::dnd::{DragDevice, DragEndKind, DragPoint};

    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            COMPOSITOR.dnd.on_start(|drag| {
                let fade = Animation::new(1.0);
                let ended = drag.ended();
                let view = drag.show(
                    DragViewOptions::new()
                        .anchor(derive(move || {
                            if ended.get().is_some() {
                                DragViewAnchor::Global
                            } else {
                                DragViewAnchor::Pointer
                            }
                        }))
                        .opacity(fade)
                        .content({
                            let drag = drag.clone();
                            move || {
                                let text = match drag.action().get() {
                                    DragAction::Copy => "copy",
                                    _ => "none",
                                };
                                Flex::column()
                                    .child(drag.icon_slot(Label::new("no icon")))
                                    .child(Label::new(text))
                            }
                        }),
                );
                drag.on_end(move |_| {
                    let view = view.clone();
                    fade.start_then(AnimationOptions::to(0.0, 100.0), move |done| {
                        assert!(done);
                        view.dispose();
                    });
                });
            });
        })),
        &args,
    )
    .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    let drain = || std::iter::from_fn(|| host.pop()).collect::<Vec<_>>();
    let filter = drain()
        .into_iter()
        .find_map(|message| match message {
            HostMessage::EventFilter(filter) => Some(filter),
            _ => None,
        })
        .expect("event filter");
    assert!(filter.dnd, "a listener asks for drag events");

    let views = |messages: Vec<HostMessage>| -> Vec<DragViewCommand> {
        messages
            .into_iter()
            .filter_map(|message| match message {
                HostMessage::DragView(command) => Some(command),
                _ => None,
            })
            .collect()
    };
    let texts = |command: &DragViewCommand| match command {
        DragViewCommand::Content {
            content: Some(tree),
            ..
        } => {
            let mut texts = Vec::new();
            fn walk(node: &DecorationNode, texts: &mut Vec<String>) {
                if let DecorationNodeKind::Label(label) = &node.kind {
                    texts.push(label.text.clone());
                }
                if matches!(node.kind, DecorationNodeKind::WindowSlot) {
                    texts.push("<icon>".into());
                }
                node.children.iter().for_each(|child| walk(child, texts));
            }
            walk(tree, &mut texts);
            Some(texts)
        }
        _ => None,
    };

    let start = DndEventSnapshot::Start {
        drag: DragSnapshot {
            id: 7,
            mime_types: vec!["text/plain".into()],
            actions: Default::default(),
            action: DragAction::None,
            source_window_id: None,
            target_window_id: None,
            device: DragDevice::Pointer,
            origin: DragPoint { x: 10.0, y: 20.0 },
            icon: None,
        },
    };
    assert!(runtime.dnd_event(&start, 1000).unwrap().invoked);
    let commands = views(drain());
    assert!(matches!(
        commands[0],
        DragViewCommand::Show { drag_id: 7, .. }
    ));
    assert!(commands.iter().any(|command| matches!(
        command,
        DragViewCommand::Props { props, .. } if props.opacity == 1.0 && props.anchor == DragViewAnchor::Pointer
    )));
    let content: Vec<_> = commands.iter().filter_map(texts).collect();
    assert_eq!(content, [vec!["no icon".to_string(), "none".to_string()]]);

    // The target settles on copy and the application sets an icon: the
    // content is rebuilt with the icon slot.
    let update = DndEventSnapshot::Update {
        drag_id: 7,
        action: DragAction::Copy,
        target_window_id: None,
        icon: Some(shojiwm_rs::dnd::DragIconInfo {
            width: 32,
            height: 32,
            offset_x: -16,
            offset_y: -16,
        }),
    };
    runtime.dnd_event(&update, 1010).unwrap();
    let content: Vec<_> = views(drain()).iter().filter_map(texts).collect();
    assert_eq!(content, [vec!["<icon>".to_string(), "copy".to_string()]]);

    // The drop: the view is pinned, fades out, and is disposed of once the
    // fade is over.
    let end = DndEventSnapshot::End {
        drag_id: 7,
        kind: DragEndKind::Dropped,
        accepted: false,
        target_window_id: None,
        position: DragPoint { x: 50.0, y: 60.0 },
    };
    runtime.dnd_event(&end, 1020).unwrap();
    let commands = views(drain());
    assert!(commands.iter().any(|command| matches!(
        command,
        DragViewCommand::Props { props, .. } if props.anchor == DragViewAnchor::Global
    )));
    runtime.scheduler_tick(1070.0).unwrap();
    let commands = views(drain());
    assert!(commands.iter().any(|command| matches!(
        command,
        DragViewCommand::Props { props, .. } if (props.opacity - 0.5).abs() < 1e-6
    )));
    runtime.scheduler_tick(1120.0).unwrap();
    assert!(
        views(drain())
            .iter()
            .any(|command| matches!(command, DragViewCommand::Dispose { .. }))
    );
}

thread_local! {
    static LAYOUT_EVENTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static LOCK_EVENTS: std::cell::RefCell<Vec<KeyboardLocksSnapshot>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn count_keyboard_events() {
    COMPOSITOR
        .event
        .on_keyboard_layout_change(|_| LAYOUT_EVENTS.with(|count| count.set(count.get() + 1)));
    COMPOSITOR.event.on_keyboard_locks_change(|locks| {
        LOCK_EVENTS.with(|events| events.borrow_mut().push(*locks))
    });
}

#[test]
fn lock_keys_are_reported_apart_from_the_layout() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(Box::new(ConfigBuilder::new(count_keyboard_events)), &args)
        .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let layout = || KeyboardLayoutSnapshot {
        index: 0,
        name: "English (US)".into(),
    };
    let caps = |caps_lock| KeyboardLocksSnapshot {
        caps_lock,
        num_lock: true,
    };
    assert!(runtime.sync_keyboard_layout(layout()));
    assert!(runtime.sync_keyboard_locks(caps(false)));
    // Toggling Caps Lock is a lock change only, and only once per change.
    assert!(runtime.sync_keyboard_locks(caps(true)));
    assert!(!runtime.sync_keyboard_locks(caps(true)));
    assert!(!runtime.sync_keyboard_layout(layout()));
    assert!(runtime.sync_keyboard_locks(caps(false)));

    assert_eq!(LAYOUT_EVENTS.with(|count| count.get()), 1);
    assert_eq!(
        LOCK_EVENTS.with(|events| events.borrow().clone()),
        [caps(false), caps(true), caps(false)]
    );
    assert_eq!(
        untrack(|| COMPOSITOR.input.keyboard_locks().get()),
        Some(caps(false))
    );
}
