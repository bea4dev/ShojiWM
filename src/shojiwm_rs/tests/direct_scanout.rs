//! `direct_scanout` on an output's config reaches the compositor with the
//! display configuration.

use std::collections::BTreeMap;

use shojiwm_rs::{
    HostMessage, RuntimeBoot, RuntimeHost,
    cli::CommonArgs,
    prelude::*,
    ssd::{OutputModeSnapshot, OutputPositionSnapshot, WaylandOutputSnapshot},
};

fn setup() {
    COMPOSITOR.output.configure(|_| {
        let mut composited = OutputConfig::extend(1.0);
        composited.direct_scanout = Some(false);
        BTreeMap::from([
            ("DP-1".to_owned(), Some(composited)),
            ("DP-2".to_owned(), Some(OutputConfig::extend(1.0))),
        ])
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

#[test]
fn direct_scanout_is_sent_with_the_output_config() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime.sync_display_state(BTreeMap::from([
        ("DP-1".to_owned(), output("DP-1")),
        ("DP-2".to_owned(), output("DP-2")),
    ]));

    let display = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::Display(update) => Some(update),
            _ => None,
        })
        .last()
        .expect("the display configuration is published");
    let direct_scanout = |name: &str| display.outputs[name].as_ref().unwrap().direct_scanout;
    assert_eq!(direct_scanout("DP-1"), Some(false));
    assert_eq!(direct_scanout("DP-2"), None);
}
