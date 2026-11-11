//! Key binding options reach the compositor with the binding.

use shojiwm_rs::{HostMessage, RuntimeBoot, RuntimeHost, cli::CommonArgs, prelude::*};

fn setup() {
    COMPOSITOR.key.bind_with(
        "louder",
        "XF86AudioRaiseVolume",
        KeyBindOptions::new().locked().repeat(),
        || {},
    );
    COMPOSITOR.key.bind("plain", "Super+T", || {});
    // Not valid: a tap cannot repeat, so it is not registered.
    COMPOSITOR.key.bind_with(
        "tap",
        "Super",
        KeyBindOptions::new().on_release().repeat(),
        || {},
    );
}

#[test]
fn locked_and_repeat_are_sent_with_the_binding() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let entries = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::KeyBindings(update) => Some(update.entries),
            _ => None,
        })
        .last()
        .expect("key bindings should be sent");
    let flags = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry.locked, entry.repeat))
        .collect::<Vec<_>>();
    assert_eq!(flags, [("louder", true, true), ("plain", false, false)]);
}
