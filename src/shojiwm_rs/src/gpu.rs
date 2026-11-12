//! `COMPOSITOR.gpu`: the seat's GPUs, and the one everything is composited on.
//! Only the TTY backend has GPUs to report; in a nested session the list stays
//! empty.

use std::rc::Rc;

use crate::{
    reactive::ReadSignal,
    runtime::{self, with_registry},
};

pub use shojiwm_lib::gpu::{GpuSnapshot, GpuVendor, RenderGpuChoice};

/// `COMPOSITOR.gpu`.
pub struct GpuController;

impl GpuController {
    /// Every GPU, ordered by [`GpuSnapshot::id`] (tracked). Empty until the
    /// first report after enable.
    pub fn list(&self) -> ReadSignal<Vec<GpuSnapshot>> {
        runtime::global().gpus.read_only()
    }

    /// The GPU everything is composited on (tracked).
    pub fn render(&self) -> Option<GpuSnapshot> {
        self.list().get().into_iter().find(|gpu| gpu.is_render)
    }

    /// The GPU with this [`GpuSnapshot::id`] (tracked).
    pub fn get(&self, id: &str) -> Option<GpuSnapshot> {
        self.list().get().into_iter().find(|gpu| gpu.id == id)
    }

    /// Choose the GPU everything is composited on (like Hyprland's
    /// `AQ_DRM_DEVICES`). Asked once at startup, before `on_enable` and before
    /// any GPU is opened, and only when there is more than one GPU; register it
    /// in the setup function. The environment variable `SHOJI_RENDER_GPU`
    /// wins over it. Displays on other GPUs keep working: their frames are
    /// composited on this GPU and copied over.
    ///
    /// ```ignore
    /// COMPOSITOR.gpu.select_render(|gpus| {
    ///     gpus.iter()
    ///         .find(|gpu| gpu.vendor == GpuVendor::Amd && !gpu.boot_vga)
    ///         .map_or(RenderGpuChoice::Auto, |gpu| RenderGpuChoice::Gpu(gpu.id.clone()))
    /// });
    /// ```
    pub fn select_render(&self, f: impl Fn(&[GpuSnapshot]) -> RenderGpuChoice + 'static) {
        with_registry(|registry| registry.render_gpu = Some(Rc::new(f)));
    }

    /// Environment that makes a program render on `gpu` (PRIME render
    /// offload), for a process spec's `env`.
    pub fn offload_env(&self, gpu: &GpuSnapshot) -> Vec<(String, String)> {
        shojiwm_lib::gpu::offload_env(gpu)
    }
}
