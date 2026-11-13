//! `COMPOSITOR.output.set_clear_color` reaches the compositor while the
//! config loads, so the first frame already has it.

use shojiwm_rs::{HostMessage, RuntimeBoot, RuntimeHost, cli::CommonArgs, prelude::*};

fn setup() {
    COMPOSITOR.output.set_clear_color(hex("#ff000080"));
}

#[test]
fn the_clear_color_is_sent_premultiplied_during_setup() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let color = std::iter::from_fn(|| host.pop())
        .find_map(|message| match message {
            HostMessage::ClearColor(color) => Some(color),
            _ => None,
        })
        .expect("set_clear_color was not sent");
    let alpha = (128.0_f64 / 255.0) as f32;
    assert_eq!(color, [alpha, 0.0, 0.0, alpha]);
}

#[test]
fn a_reload_sends_the_last_clear_color_again() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(ConfigBuilder::new(|| {
            setup();
            COMPOSITOR.on_reload(|| {
                COMPOSITOR.output.set_clear_color(hex("#000000"));
                Ok(())
            });
        })),
        &args,
    )
    .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    let last_sent = |host: &RuntimeHost| {
        std::iter::from_fn(|| host.pop())
            .filter_map(|message| match message {
                HostMessage::ClearColor(color) => Some(color),
                _ => None,
            })
            .last()
    };
    assert!(last_sent(&host).is_some());

    // The compositor handles what `on_reload` sends, then resets the colour to
    // the default before `reload`: the colour has to come again from there.
    runtime.prepare_reload().unwrap();
    last_sent(&host);
    runtime.reload().unwrap();
    assert_eq!(last_sent(&host), Some([0.0, 0.0, 0.0, 1.0]));
}
