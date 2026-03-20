use std::sync::mpsc;
use smithay::reexports::calloop::channel::Sender as CalloopSender;

use super::{
    icon::{IconSpec, IconRasterizer},
    text::TextRasterizer,
    text::LabelSpec,
};

#[derive(Debug, Clone)]
pub enum AsyncAssetJob {
    Text { spec_hash: u64, spec: LabelSpec },
    Icon { spec_hash: u64, spec: IconSpec },
}

pub type AsyncAssetJobSender = mpsc::Sender<AsyncAssetJob>;

#[derive(Debug, Clone)]
pub enum AsyncAssetResult {
    TextReady {
        spec_hash: u64,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
    TextMissing {
        spec_hash: u64,
    },
    IconReady {
        spec_hash: u64,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    },
    IconMissing {
        spec_hash: u64,
    },
}

pub fn spawn_async_asset_worker(
    result_sender: CalloopSender<AsyncAssetResult>,
) -> AsyncAssetJobSender {
    let (job_sender, job_receiver) = mpsc::channel();

    std::thread::Builder::new()
        .name("shojiwm-async-assets".into())
        .spawn(move || {
            let mut text_rasterizer = TextRasterizer::new(None);
            let mut icon_rasterizer = IconRasterizer::new(None);

            while let Ok(job) = job_receiver.recv() {
                match job {
                    AsyncAssetJob::Text { spec_hash, spec } => {
                        let result = if let Some(rendered) =
                            text_rasterizer.render_label_pixels(&spec)
                        {
                            AsyncAssetResult::TextReady {
                                spec_hash,
                                width: rendered.width,
                                height: rendered.height,
                                pixels: rendered.pixels,
                            }
                        } else {
                            AsyncAssetResult::TextMissing { spec_hash }
                        };
                        let _ = result_sender.send(result);
                    }
                    AsyncAssetJob::Icon { spec_hash, spec } => {
                        let result = if let Some(rendered) =
                            icon_rasterizer.render_icon_pixels(&spec)
                        {
                            AsyncAssetResult::IconReady {
                                spec_hash,
                                width: rendered.width,
                                height: rendered.height,
                                pixels: rendered.pixels,
                            }
                        } else {
                            AsyncAssetResult::IconMissing { spec_hash }
                        };
                        let _ = result_sender.send(result);
                    }
                }
            }
        })
        .expect("failed to spawn async asset worker");

    job_sender
}
