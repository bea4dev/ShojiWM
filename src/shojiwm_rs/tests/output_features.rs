//! Output-wide config state that the compositor is sent when it changes:
//! custom compositions, frame pacing, the background effect, and the input
//! grab.

use std::collections::BTreeMap;

use shojiwm_rs::{
    HostMessage, RuntimeBoot, RuntimeHandle, RuntimeHost,
    cli::CommonArgs,
    output_composition::plan::CompositionNode,
    prelude::*,
    runtime_input_grab::{InputGrabEventSnapshot, InputGrabStateSnapshot},
    ssd::{
        OutputModeSnapshot, OutputPositionSnapshot, PointerModifierStateSnapshot,
        WaylandOutputSnapshot,
    },
};

thread_local! {
    static SWITCHER: Signal<bool> = signal(false);
    static BLUR_OFF: Signal<bool> = signal(false);
    static STRENGTH: Signal<f64> = signal(1.0);
    static KEYS: Signal<Vec<String>> = signal(Vec::new());
    static CANCELLED: Signal<Option<InputGrabCancelReason>> = signal(None);
}

fn setup() {
    let switcher = SWITCHER.with(|signal| *signal);
    let blur_off = BLUR_OFF.with(|signal| *signal);
    let strength = STRENGTH.with(|signal| *signal);
    COMPOSITOR.rendering.frame_pacing(FramePacing::LowLatency);
    COMPOSITOR.rendering.composition(move |output| {
        if !switcher.get() {
            return OutputStack::default_stacking();
        }
        let (width, height) = output_logical_size(output);
        let texture = RenderTexture::new().key("all").child(Windows::all());
        OutputStack::new()
            .child(Solid::new([0.0, 0.0, 0.0, 0.5]))
            .child(Scene3D::new(screen_camera(width, height, ScreenCamera::default())).plane(
                Plane::new(&texture, width, height).transform(transform3d().rotate_y(20.0)),
            ))
    });
    COMPOSITOR.effect.background_with(move || {
        (!blur_off.get()).then(|| {
            Effect::new(backdrop_source())
                .stage(dual_kawase_blur(4, 2))
                .stage(shader_stage("/fade.frag").uniform("strength", strength))
        })
    });
    COMPOSITOR.key.bind("grab", "Super+Tab", || {
        let keys = KEYS.with(|signal| *signal);
        let cancelled = CANCELLED.with(|signal| *signal);
        COMPOSITOR.input.grab(
            InputGrabOptions::new()
                .on_key(move |event| keys.update(|keys| keys.push(event.key.clone())))
                .on_cancel(move |reason| cancelled.set(Some(reason.clone()))),
        );
    });
}

fn output(name: &str) -> WaylandOutputSnapshot {
    WaylandOutputSnapshot {
        name: name.into(),
        description: None,
        make: None,
        model: None,
        serial: None,
        connector: None,
        enabled: true,
        resolution: Some(OutputModeSnapshot {
            width: 1920,
            height: 1080,
            refresh_rate: 60.0,
            clock_khz: None,
        }),
        position: OutputPositionSnapshot { x: 0, y: 0 },
        scale: 1.0,
        transform: Default::default(),
        available_modes: Vec::new(),
        subpixel: Default::default(),
        detected_subpixel: Default::default(),
        hdr_supported: false,
        hdmi: None,
    }
}

fn start() -> (RuntimeHandle, RuntimeHost) {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime.sync_display_state(BTreeMap::from([("DP-1".to_owned(), output("DP-1"))]));
    (runtime, host)
}

fn drain(host: &RuntimeHost) -> Vec<HostMessage> {
    std::iter::from_fn(|| host.pop()).collect()
}

fn tick(runtime: &mut RuntimeHandle) {
    runtime.scheduler_tick(0.0).unwrap();
}

#[test]
fn compositions_follow_their_signals() {
    // Outputs arriving publish the composition of each.
    let (mut runtime, host) = start();
    let messages = drain(&host);
    let plans = messages
        .iter()
        .find_map(|message| match message {
            HostMessage::OutputCompositions(plans) => Some(plans.clone()),
            _ => None,
        })
        .expect("the default stacking is published");
    assert_eq!(plans["DP-1"].nodes.len(), 4);
    assert!(messages.iter().any(|message| matches!(
        message,
        HostMessage::FramePacing(config) if config.pacing("DP-1") == FramePacing::LowLatency
    )));

    // Nothing changed: nothing is sent again.
    tick(&mut runtime);
    assert!(drain(&host).is_empty());

    // (Set from the test; config code would do it inside a turn.)
    SWITCHER.with(|signal| signal.set(true));
    tick(&mut runtime);
    let plans = drain(&host)
        .into_iter()
        .find_map(|message| match message {
            HostMessage::OutputCompositions(plans) => Some(plans),
            _ => None,
        })
        .expect("the switcher composition is published");
    let plan = &plans["DP-1"];
    assert_eq!(plan.textures.len(), 1);
    assert!(matches!(plan.nodes[1], CompositionNode::Scene3d(_)));
}

#[test]
fn background_effect_is_republished_when_its_signals_change() {
    let (mut runtime, host) = start();
    let first = runtime.background_effect_config().unwrap();
    assert!(first.is_some());
    drain(&host);

    STRENGTH.with(|signal| signal.set(0.5));
    tick(&mut runtime);
    let update = drain(&host).into_iter().find_map(|message| match message {
        HostMessage::BackgroundEffect(config) => Some(config),
        _ => None,
    });
    let config = update.expect("a uniform change republishes the effect").unwrap();
    assert_ne!(Some(config), first);

    BLUR_OFF.with(|signal| signal.set(true));
    tick(&mut runtime);
    let update = drain(&host).into_iter().find_map(|message| match message {
        HostMessage::BackgroundEffect(config) => Some(config),
        _ => None,
    });
    assert_eq!(update, Some(None), "the effect turns off");
}

#[test]
fn input_grab_receives_events_until_cancelled() {
    let (mut runtime, host) = start();
    drain(&host);
    runtime.invoke_key_binding("grab", 0).unwrap();
    let grab_id = drain(&host)
        .into_iter()
        .find_map(|message| match message {
            HostMessage::InputGrab(update) if update.active => Some(update.id),
            _ => None,
        })
        .expect("the grab starts");

    let key = |key: &str| InputGrabEventSnapshot::Key {
        key: key.into(),
        keycode: 0,
        state: InputGrabStateSnapshot::Pressed,
        modifiers: PointerModifierStateSnapshot {
            logo: true,
            alt: false,
            ctrl: false,
            shift: false,
        },
        timestamp: 0,
    };
    runtime.input_grab_event(grab_id, &key("Tab"), 0).unwrap();
    // A stale id is ignored.
    runtime.input_grab_event(grab_id + 100, &key("Escape"), 0).unwrap();
    assert_eq!(KEYS.with(|signal| signal.get_untracked()), ["Tab"]);

    runtime
        .input_grab_event(
            grab_id,
            &InputGrabEventSnapshot::Cancel {
                reason: "sessionLock".into(),
            },
            0,
        )
        .unwrap();
    assert_eq!(
        CANCELLED.with(|signal| signal.get_untracked()),
        Some(InputGrabCancelReason::SessionLock)
    );
    runtime.input_grab_event(grab_id, &key("Return"), 0).unwrap();
    assert_eq!(KEYS.with(|signal| signal.get_untracked()), ["Tab"]);
}
