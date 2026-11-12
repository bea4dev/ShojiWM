//! A `code:<xkb keycode>` shortcut reaches the compositor; one below 8 does not.

use shojiwm_rs::{HostMessage, RuntimeBoot, RuntimeHost, cli::CommonArgs, prelude::*};

fn setup() {
    COMPOSITOR.key.bind("by-code", "Super+code:10", || {});
    COMPOSITOR.key.bind("bad-code", "Super+code:7", || {});
}

#[test]
fn keycode_shortcuts_are_sent_with_the_binding() {
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
    let shortcuts = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry.shortcut.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(shortcuts, [("by-code", "Super+code:10")]);
}
