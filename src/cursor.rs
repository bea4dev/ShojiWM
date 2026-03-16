use std::collections::HashMap;
use std::time::Duration;

use smithay::input::pointer::CursorIcon;
use tracing::warn;
use xcursor::{
    parser::Image,
    CursorTheme,
};

pub struct Cursor {
    theme: CursorTheme,
    cache: HashMap<CursorIcon, Vec<Image>>,
    size: u32,
}

impl Cursor {
    pub fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME")
            .ok()
            .unwrap_or_else(|| "default".into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(24);

        let theme = CursorTheme::load(&name);
        let mut cache = HashMap::new();
        cache.insert(
            CursorIcon::Default,
            load_icon(&theme, CursorIcon::Default).unwrap_or_else(|| {
                warn!("failed to load default xcursor theme image, using fallback cursor");
                vec![fallback_cursor_image()]
            }),
        );

        Self { theme, cache, size }
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

    images
        .iter()
        .filter(move |image| image.width == nearest_image.width && image.height == nearest_image.height)
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
    theme.load_icon(icon.name())
        .or_else(|| icon.alt_names().iter().find_map(|name| theme.load_icon(name)))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| xcursor::parser::parse_xcursor(&bytes))
}
