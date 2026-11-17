use std::collections::HashMap;
use std::time::Duration;

use smithay::input::pointer::CursorIcon;
use tracing::warn;
use xcursor::{CursorTheme, parser::Image};

pub struct Cursor {
    name: String,
    theme: CursorTheme,
    cache: HashMap<CursorIcon, Vec<Image>>,
    size: u32,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCursorConfigUpdate {
    pub theme: String,
    pub size: u32,
    #[serde(default)]
    pub reload: bool,
}

/// `COMPOSITOR.cursor.setPosition`: where to move the cursor.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum RuntimeCursorPositionTarget {
    /// A point on `output`, placed like a pointer region's anchor.
    Output {
        output: String,
        #[serde(default = "center_anchor")]
        anchor: crate::runtime_pointer_region::PointerRegionAnchor,
        #[serde(default)]
        offset: crate::runtime_pointer_region::PointerRegionOffset,
    },
    /// Global logical coordinates in the output layout.
    Global { x: f64, y: f64 },
}

/// `COMPOSITOR.cursor.overrideKind`: the cursor shown everywhere, whatever
/// the client under the pointer asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorOverride {
    Named(CursorIcon),
    Hidden,
}

impl CursorOverride {
    /// A CSS cursor name, or `none` to hide the cursor.
    pub fn parse(name: &str) -> Result<Self, String> {
        if name == "none" {
            return Ok(Self::Hidden);
        }
        name.parse::<CursorIcon>()
            .map(Self::Named)
            .map_err(|_| format!("unknown cursor kind: {name:?}"))
    }

    pub fn status(self) -> smithay::input::pointer::CursorImageStatus {
        match self {
            Self::Named(icon) => smithay::input::pointer::CursorImageStatus::Named(icon),
            Self::Hidden => smithay::input::pointer::CursorImageStatus::Hidden,
        }
    }
}

/// `COMPOSITOR.cursor.setTransform`: the cursor image is scaled along two
/// axes, then rotated, both around the hotspot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorTransform {
    /// Degrees, clockwise on screen.
    pub rotate: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    /// Degrees, clockwise, the scale's axes are turned from the image's own.
    /// The image itself is not turned by it: a stretch along a direction of
    /// travel leaves the cursor upright.
    pub scale_axis: f64,
}

impl Default for CursorTransform {
    fn default() -> Self {
        Self {
            rotate: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            scale_axis: 0.0,
        }
    }
}

impl CursorTransform {
    /// The largest scale on either axis; bounds the texture the cursor is
    /// redrawn into.
    pub const MAX_SCALE: f64 = 8.0;

    /// `None` for the identity, which draws the cursor untouched (and lets it
    /// back onto the cursor plane).
    pub fn validated(self) -> Result<Option<Self>, String> {
        for (name, angle) in [("rotate", self.rotate), ("scaleAxis", self.scale_axis)] {
            if !angle.is_finite() {
                return Err(format!(
                    "cursor transform: {name} must be finite, got {angle}"
                ));
            }
        }
        for (name, scale) in [("scaleX", self.scale_x), ("scaleY", self.scale_y)] {
            if !scale.is_finite() || scale == 0.0 || scale.abs() > Self::MAX_SCALE {
                return Err(format!(
                    "cursor transform: {name} must be non-zero and within ±{}, got {scale}",
                    Self::MAX_SCALE
                ));
            }
        }
        let identity =
            self.rotate.rem_euclid(360.0) == 0.0 && self.scale_x == 1.0 && self.scale_y == 1.0;
        Ok((!identity).then_some(self))
    }

    /// The linear part in screen space (y down): `rotate · axis · scale ·
    /// axis⁻¹`, as `[[a, b], [c, d]]` mapping `(x, y)` to
    /// `(a x + b y, c x + d y)`.
    pub fn matrix(&self) -> [[f64; 2]; 2] {
        let (sin, cos) = self.scale_axis.to_radians().sin_cos();
        let across = sin * cos * (self.scale_x - self.scale_y);
        let scale = [
            [cos * cos * self.scale_x + sin * sin * self.scale_y, across],
            [across, sin * sin * self.scale_x + cos * cos * self.scale_y],
        ];
        let (sin, cos) = self.rotate.to_radians().sin_cos();
        [
            [
                cos * scale[0][0] - sin * scale[1][0],
                cos * scale[0][1] - sin * scale[1][1],
            ],
            [
                sin * scale[0][0] + cos * scale[1][0],
                sin * scale[0][1] + cos * scale[1][1],
            ],
        ]
    }
}

fn center_anchor() -> crate::runtime_pointer_region::PointerRegionAnchor {
    crate::runtime_pointer_region::PointerRegionAnchor::Center
}

/// `COMPOSITOR.cursor.getPosition`: the cursor as of the last pointer motion.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPositionSnapshot {
    pub x: f64,
    pub y: f64,
    pub output_name: Option<String>,
}

impl Cursor {
    pub fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME")
            .ok()
            .filter(|name| !name.is_empty() && !name.contains('\0'))
            .unwrap_or_else(|| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|size| (1..=512).contains(size))
            .unwrap_or(24);

        Self::from_config(name, size)
    }

    fn from_config(name: String, size: u32) -> Self {
        let theme = CursorTheme::load(&name);
        let cache = default_icon_cache(&theme, &name);
        Self {
            name,
            theme,
            cache,
            size,
        }
    }

    pub fn apply_runtime_config(&mut self, update: RuntimeCursorConfigUpdate) -> bool {
        if update.theme.is_empty() || update.size == 0 || update.size > 512 {
            warn!(?update, "ignoring invalid runtime cursor configuration");
            return false;
        }
        if !update.reload && update.theme == self.name && update.size == self.size {
            return false;
        }

        self.name = update.theme;
        self.size = update.size;
        self.theme = CursorTheme::load(&self.name);
        self.cache = default_icon_cache(&self.theme, &self.name);
        true
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    pub fn get_image(&mut self, icon: CursorIcon, scale: u32, time: Duration) -> Image {
        let size = self.size * scale;
        let icons = self.cache.entry(icon).or_insert_with(|| {
            load_icon(&self.theme, icon)
                .or_else(|| load_icon(&self.theme, CursorIcon::Default))
                .unwrap_or_else(|| {
                    warn!(requested = %icon.name(), "failed to load themed cursor image, using fallback");
                    vec![fallback_cursor_image()]
                })
        });
        frame(time.as_millis() as u32, size, icons)
    }
}

fn default_icon_cache(theme: &CursorTheme, name: &str) -> HashMap<CursorIcon, Vec<Image>> {
    let mut cache = HashMap::new();
    cache.insert(
        CursorIcon::Default,
        load_icon(theme, CursorIcon::Default).unwrap_or_else(|| {
            warn!(
                theme = name,
                "failed to load default xcursor theme image, using fallback cursor"
            );
            vec![fallback_cursor_image()]
        }),
    );
    cache
}

fn fallback_cursor_image() -> Image {
    let width = 16;
    let height = 24;
    let mut pixels_rgba = vec![0u8; width * height * 4];

    for y in 0..height {
        for x in 0..width {
            let draw = x == 0
                || y == 0
                || x == y.min(width - 1)
                || (x == 1 && y < height - 4)
                || (y > 10 && x > 0 && x < 4);
            if draw {
                let idx = (y * width + x) * 4;
                pixels_rgba[idx] = 255;
                pixels_rgba[idx + 1] = 255;
                pixels_rgba[idx + 2] = 255;
                pixels_rgba[idx + 3] = 255;
            }
        }
    }

    Image {
        size: 24,
        width: width as u32,
        height: height as u32,
        xhot: 1,
        yhot: 1,
        delay: 1,
        pixels_rgba,
        pixels_argb: vec![],
    }
}

fn nearest_images(size: u32, images: &[Image]) -> impl Iterator<Item = &Image> {
    let nearest_image = images
        .iter()
        .min_by_key(|image| (size as i32 - image.size as i32).abs())
        .expect("cursor set must not be empty");

    images.iter().filter(move |image| {
        image.width == nearest_image.width && image.height == nearest_image.height
    })
}

fn frame(mut millis: u32, size: u32, images: &[Image]) -> Image {
    let total = nearest_images(size, images).fold(0, |acc, image| acc + image.delay);
    if total == 0 {
        return nearest_images(size, images)
            .next()
            .expect("cursor set must not be empty")
            .clone();
    }

    millis %= total;

    for image in nearest_images(size, images) {
        if millis < image.delay {
            return image.clone();
        }
        millis -= image.delay;
    }

    unreachable!("cursor animation frame resolution should always return an image");
}

fn load_icon(theme: &CursorTheme, icon: CursorIcon) -> Option<Vec<Image>> {
    theme
        .load_icon(icon.name())
        .or_else(|| {
            icon.alt_names()
                .iter()
                .find_map(|name| theme.load_icon(name))
        })
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| xcursor::parser::parse_xcursor(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_reloads_only_when_needed() {
        let mut cursor = Cursor::from_config("__shojiwm_test_missing_theme__".into(), 24);

        assert!(!cursor.apply_runtime_config(RuntimeCursorConfigUpdate {
            theme: "__shojiwm_test_missing_theme__".into(),
            size: 24,
            reload: false,
        }));
        assert!(cursor.apply_runtime_config(RuntimeCursorConfigUpdate {
            theme: "__shojiwm_test_missing_theme__".into(),
            size: 24,
            reload: true,
        }));
        assert!(cursor.apply_runtime_config(RuntimeCursorConfigUpdate {
            theme: "__shojiwm_test_missing_theme__".into(),
            size: 48,
            reload: false,
        }));
        assert_eq!(cursor.size(), 48);
    }

    #[test]
    fn invalid_runtime_config_is_rejected() {
        let mut cursor = Cursor::from_config("__shojiwm_test_missing_theme__".into(), 24);

        assert!(!cursor.apply_runtime_config(RuntimeCursorConfigUpdate {
            theme: String::new(),
            size: 24,
            reload: true,
        }));
        assert!(!cursor.apply_runtime_config(RuntimeCursorConfigUpdate {
            theme: "default".into(),
            size: 513,
            reload: true,
        }));
        assert_eq!(cursor.size(), 24);
    }
}
