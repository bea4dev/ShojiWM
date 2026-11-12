//! `COMPOSITOR.cursor.set_transform` reaches the compositor, the identity is
//! sent as no transform, and values the renderer cannot draw are refused.

use shojiwm_rs::{HostMessage, RuntimeBoot, RuntimeHost, cli::CommonArgs, prelude::*};

fn setup() {
    COMPOSITOR
        .cursor
        .set_transform(Some(CursorTransform {
            rotate: -12.0,
            scale_x: 1.3,
            scale_y: 0.85,
            scale_axis: 30.0,
        }))
        .unwrap();
    COMPOSITOR
        .cursor
        .set_transform(Some(CursorTransform::default()))
        .unwrap();
    assert!(
        COMPOSITOR
            .cursor
            .set_transform(Some(CursorTransform {
                scale_x: 0.0,
                ..Default::default()
            }))
            .is_err()
    );
}

#[test]
fn the_transform_and_its_reset_reach_the_compositor() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let sent = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::CursorTransform(transform) => Some(transform),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sent,
        vec![
            Some(CursorTransform {
                rotate: -12.0,
                scale_x: 1.3,
                scale_y: 0.85,
                scale_axis: 30.0,
            }),
            None,
        ]
    );
}
