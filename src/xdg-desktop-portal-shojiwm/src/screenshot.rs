//! org.freedesktop.impl.portal.Screenshot backend implementation.
//!
//! See: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.Screenshot.html
//!
//! A screenshot is one output, captured with the same ext-image-copy-capture
//! path the picker thumbnails use, written as a PNG to the temp directory.
//! `interactive` lets the user choose the output in the ScreenCast picker;
//! otherwise it is the output of the focused window. An interactive request
//! captures every output before the picker opens, so the picker and its
//! closing animation are never in the picture.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::picker::{PickResult, PickerHandle};
use crate::sources::{self, Capture, OutputTarget, SourceKind, SourceUpdate, source_types};

/// Response codes of `org.freedesktop.impl.portal.Request`.
mod response {
    pub const SUCCESS: u32 = 0;
    pub const CANCELLED: u32 = 1;
    pub const OTHER: u32 = 2;
}

pub struct Screenshot {
    picker: PickerHandle,
    thumbnail_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
}

impl Screenshot {
    pub fn new(
        picker: PickerHandle,
        thumbnail_tx: tokio::sync::mpsc::Sender<SourceUpdate>,
    ) -> Self {
        Self {
            picker,
            thumbnail_tx,
        }
    }

    /// Let the user choose an output in the picker. `None` when cancelled.
    async fn pick_output(&self, request_key: String) -> Option<String> {
        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let stream_guard = sources::start_thumbnail_stream(
            request_key.clone(),
            source_types::MONITOR,
            init_tx,
            self.thumbnail_tx.clone(),
        );
        let sources = init_rx.await.unwrap_or_default();
        // The frontend's Request.Close does not reach this backend, so the
        // pick ends only in the picker.
        let (_keep_open, cancelled) = tokio::sync::watch::channel(false);
        let picked = self
            .picker
            .pick(request_key.clone(), sources, cancelled, false)
            .await;
        self.picker.cancel(&request_key);
        let _ = tokio::task::spawn_blocking(move || drop(stream_guard)).await;
        match picked {
            PickResult::Source(source) => match source.kind {
                SourceKind::Output(output) => Some(output.name),
                SourceKind::Toplevel(_) => None,
            },
            PickResult::Cancelled => None,
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Screenshot")]
impl Screenshot {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn screenshot(
        &self,
        handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let interactive = is_interactive(&options);
        tracing::info!(%handle, %app_id, %parent_window, interactive, "Screenshot");
        let target = if interactive {
            OutputTarget::All
        } else {
            OutputTarget::Focused
        };
        let captures = tokio::task::spawn_blocking(move || sources::capture_outputs(&target))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("capture thread join: {e}")))?;
        let mut captures = match captures {
            Ok(captures) => captures,
            Err(error) => {
                tracing::warn!(%handle, %error, "Screenshot failed");
                return Ok((response::OTHER, HashMap::new()));
            }
        };
        let capture = if interactive {
            let Some(name) = self.pick_output(handle.to_string()).await else {
                tracing::info!(%handle, "Screenshot: picker cancelled");
                return Ok((response::CANCELLED, HashMap::new()));
            };
            match captures.into_iter().find(|(captured, _)| *captured == name) {
                Some((_, capture)) => capture,
                None => {
                    tracing::warn!(%handle, output = %name, "Screenshot: output appeared after the capture");
                    return Ok((response::OTHER, HashMap::new()));
                }
            }
        } else {
            captures.swap_remove(0).1
        };
        let path = screenshot_path();
        let written = {
            let path = path.clone();
            tokio::task::spawn_blocking(move || write_png(&path, &capture))
                .await
                .map_err(|e| zbus::fdo::Error::Failed(format!("png thread join: {e}")))?
        };
        if let Err(error) = written {
            tracing::warn!(%handle, %error, "Screenshot failed");
            return Ok((response::OTHER, HashMap::new()));
        }
        let uri = format!("file://{}", path.display());
        tracing::info!(%handle, %uri, "Screenshot written");
        let uri = OwnedValue::try_from(Value::from(uri))
            .map_err(|e| zbus::fdo::Error::Failed(format!("uri value: {e}")))?;
        Ok((response::SUCCESS, HashMap::from([("uri".to_owned(), uri)])))
    }
}

fn is_interactive(options: &HashMap<String, OwnedValue>) -> bool {
    options
        .get("interactive")
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn screenshot_path() -> PathBuf {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("shojiwm-screenshot-{millis}.png"))
}

fn write_png(path: &Path, capture: &Capture) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder =
        png::Encoder::new(std::io::BufWriter::new(file), capture.width, capture.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut writer| writer.write_image_data(&capture.rgba))
        .map_err(|e| format!("png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_defaults_to_off() {
        assert!(!is_interactive(&HashMap::new()));
        let on = HashMap::from([(
            "interactive".to_owned(),
            OwnedValue::try_from(Value::from(true)).unwrap(),
        )]);
        assert!(is_interactive(&on));
    }

    #[test]
    fn the_png_holds_the_captured_pixels() {
        let capture = Capture {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, //
                0, 0, 255, 255, 10, 20, 30, 255,
            ],
        };
        let path =
            std::env::temp_dir().join(format!("shojiwm-png-test-{}.png", std::process::id()));
        write_png(&path, &capture).unwrap();

        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&pixels[..info.buffer_size()], capture.rgba.as_slice());
    }
}
