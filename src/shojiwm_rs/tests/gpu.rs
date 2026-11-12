//! `COMPOSITOR.gpu`: the config picks the render GPU before enable, and sees
//! the GPUs the compositor reports.

use shojiwm_rs::{RuntimeBoot, RuntimeHost, cli::CommonArgs, prelude::*};

fn gpu(id: &str, card: &str, driver: &str, boot_vga: bool, is_render: bool) -> GpuSnapshot {
    GpuSnapshot {
        id: id.into(),
        dynamic_card_id: card.into(),
        render_node: None,
        vendor: GpuVendor::Other,
        vendor_id: 0,
        device_id: 0,
        driver: Some(driver.into()),
        name: None,
        boot_vga,
        removable: false,
        outputs: Vec::new(),
        is_render,
        in_use: true,
    }
}

fn gpus() -> Vec<GpuSnapshot> {
    vec![
        gpu("pci-0000:00:02.0", "card1", "i915", true, true),
        gpu("pci-0000:01:00.0", "card0", "amdgpu", false, false),
    ]
}

fn launch(setup: fn()) -> shojiwm_rs::RuntimeHandle {
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(Box::new(ConfigBuilder::new(setup)), &args)
        .launch(RuntimeHost::detached());
    runtime.preload().unwrap();
    runtime
}

#[test]
fn the_config_picks_the_render_gpu_before_enable() {
    let mut runtime = launch(|| {
        COMPOSITOR.gpu.select_render(|gpus| {
            gpus.iter()
                .find(|gpu| !gpu.boot_vga)
                .map_or(RenderGpuChoice::Auto, |gpu| {
                    RenderGpuChoice::Gpu(gpu.id.clone())
                })
        });
    });
    // Asked between preload and enable, as the TTY backend does.
    assert_eq!(
        runtime.select_render_gpu(&gpus()).unwrap(),
        RenderGpuChoice::Gpu("pci-0000:01:00.0".into())
    );
    runtime.enable().unwrap();
}

#[test]
fn without_a_choice_the_compositor_picks() {
    let mut runtime = launch(|| {});
    assert_eq!(
        runtime.select_render_gpu(&gpus()).unwrap(),
        RenderGpuChoice::Auto
    );
}

#[test]
fn a_panicking_choice_is_an_error_not_a_crash() {
    let mut runtime = launch(|| {
        COMPOSITOR.gpu.select_render(|_| panic!("no GPU for you"));
    });
    let error = runtime.select_render_gpu(&gpus()).unwrap_err();
    assert!(error.to_string().contains("no GPU for you"), "{error}");
}

#[test]
fn reported_gpus_reach_the_config() {
    let mut runtime = launch(|| {});
    runtime.enable().unwrap();
    assert!(COMPOSITOR.gpu.list().get().is_empty());

    let reported = gpus();
    assert!(runtime.sync_gpu_state(&reported));
    // The same GPUs again are not sent again.
    assert!(!runtime.sync_gpu_state(&reported));

    assert_eq!(COMPOSITOR.gpu.list().get(), reported);
    assert_eq!(
        COMPOSITOR.gpu.render().map(|gpu| gpu.id),
        Some("pci-0000:00:02.0".to_owned())
    );
    assert_eq!(
        COMPOSITOR
            .gpu
            .get("pci-0000:01:00.0")
            .map(|gpu| gpu.dynamic_card_id),
        Some("card0".to_owned())
    );
    assert_eq!(
        COMPOSITOR.gpu.offload_env(&reported[1]),
        vec![("DRI_PRIME".to_owned(), "pci-0000_01_00_0".to_owned())]
    );
}
