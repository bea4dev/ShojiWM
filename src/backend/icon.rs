use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    io::Cursor,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use png::ColorType;
use resvg::{tiny_skia, usvg};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            gles::{GlesError, GlesRenderer},
        },
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale as OutputScale},
};

use crate::ssd::{LogicalRect, WindowDecorationState, WindowIconSnapshot};
use crate::backend::async_assets::{AsyncAssetJob, AsyncAssetJobSender};

#[derive(Debug, Clone)]
pub struct CachedDecorationIcon {
    pub rect: LogicalRect,
    pub buffer: MemoryRenderBuffer,
}

#[derive(Debug, Clone)]
pub struct RenderedIconPixels {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct IconSpec {
    pub rect: LogicalRect,
    pub icon: Option<WindowIconSnapshot>,
    pub app_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct IconRasterizer {
    cache: HashMap<IconCacheKey, Option<MemoryRenderBuffer>>,
    path_cache: HashMap<IconPathCacheKey, Option<PathBuf>>,
    icon_index: Option<HashMap<String, Vec<PathBuf>>>,
    async_job_sender: Option<AsyncAssetJobSender>,
    async_buffers: HashMap<u64, TimedIconBuffer>,
    async_in_flight: HashSet<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IconCacheKey {
    source: String,
    width: i32,
    height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IconPathCacheKey {
    names: String,
    target: i32,
}

#[derive(Debug, Clone)]
struct TimedIconBuffer {
    buffer: MemoryRenderBuffer,
    last_used_at: Instant,
}

impl IconRasterizer {
    pub fn new(async_job_sender: Option<AsyncAssetJobSender>) -> Self {
        Self {
            cache: HashMap::new(),
            path_cache: HashMap::new(),
            icon_index: None,
            async_job_sender,
            async_buffers: HashMap::new(),
            async_in_flight: HashSet::new(),
        }
    }

    pub fn render_icon(&mut self, spec: &IconSpec) -> Option<CachedDecorationIcon> {
        self.prune_async_buffers();
        let spec_hash = hash_icon_spec(spec);
        if let Some(cached) = self.async_buffers.get_mut(&spec_hash) {
            cached.last_used_at = Instant::now();
            return Some(CachedDecorationIcon {
                rect: spec.rect,
                buffer: cached.buffer.clone(),
            });
        }

        if let Some(async_job_sender) = &self.async_job_sender {
            if self.async_in_flight.insert(spec_hash) {
                let _ = async_job_sender.send(AsyncAssetJob::Icon {
                    spec_hash,
                    spec: spec.clone(),
                });
            }
            return None;
        }

        let rendered = self.render_icon_pixels(spec)?;
        let buffer = MemoryRenderBuffer::from_slice(
            &rendered.pixels,
            Fourcc::Argb8888,
            (rendered.width, rendered.height),
            1,
            smithay::utils::Transform::Normal,
            None,
        );
        Some(CachedDecorationIcon {
            rect: spec.rect,
            buffer,
        })
    }

    pub fn render_icon_pixels(&mut self, spec: &IconSpec) -> Option<RenderedIconPixels> {
        if spec.rect.width <= 0 || spec.rect.height <= 0 {
            return None;
        }

        let source = icon_source_key(spec)?;
        let key = IconCacheKey {
            source,
            width: spec.rect.width,
            height: spec.rect.height,
        };

        let rgba = if let Some(bytes) = spec.icon.as_ref().and_then(|icon| icon.bytes.as_ref()) {
            decode_png_and_scale(bytes, spec.rect.width, spec.rect.height)?
        } else {
            let names = icon_candidate_names(spec);
            let Some(path) = self.find_icon_path_cached(&names, spec.rect.width, spec.rect.height) else {
                self.cache.insert(key, None);
                return None;
            };
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase());
            let Some(bytes) = fs::read(&path).ok() else {
                self.cache.insert(key, None);
                return None;
            };
            let Some(rgba) = decode_icon_and_scale(
                &bytes,
                extension.as_deref(),
                spec.rect.width,
                spec.rect.height,
            ) else {
                self.cache.insert(key, None);
                return None;
            };
            rgba
        };

        let pixels = rgba_to_argb8888(&rgba);
        let buffer = MemoryRenderBuffer::from_slice(
            &pixels,
            Fourcc::Argb8888,
            (spec.rect.width, spec.rect.height),
            1,
            smithay::utils::Transform::Normal,
            None,
        );

        self.cache.insert(key, Some(buffer));
        Some(RenderedIconPixels {
            width: spec.rect.width,
            height: spec.rect.height,
            pixels,
        })
    }

    pub fn handle_async_ready(
        &mut self,
        spec_hash: u64,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    ) {
        self.async_in_flight.remove(&spec_hash);
        self.async_buffers.insert(
            spec_hash,
            TimedIconBuffer {
                buffer: MemoryRenderBuffer::from_slice(
                    &pixels,
                    Fourcc::Argb8888,
                    (width, height),
                    1,
                    smithay::utils::Transform::Normal,
                    None,
                ),
                last_used_at: Instant::now(),
            },
        );
    }

    pub fn handle_async_miss(&mut self, spec_hash: u64) {
        self.async_in_flight.remove(&spec_hash);
    }

    fn find_icon_path_cached(
        &mut self,
        names: &[String],
        width: i32,
        height: i32,
    ) -> Option<PathBuf> {
        let key = IconPathCacheKey {
            names: names.join("\u{0}"),
            target: width.max(height),
        };
        if let Some(cached) = self.path_cache.get(&key) {
            return cached.clone();
        }

        let resolved = find_icon_path_in_index(self.ensure_icon_index(), names, width, height);
        self.path_cache.insert(key, resolved.clone());
        resolved
    }

    fn ensure_icon_index(&mut self) -> &HashMap<String, Vec<PathBuf>> {
        if self.icon_index.is_none() {
            let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
            for root in icon_roots() {
                if !root.exists() {
                    continue;
                }
                build_icon_index(&root, &mut index);
            }
            self.icon_index = Some(index);
        }

        self.icon_index.as_ref().expect("icon index must be initialized")
    }

    fn prune_async_buffers(&mut self) {
        const TTL: Duration = Duration::from_secs(5);
        let now = Instant::now();
        self.async_buffers
            .retain(|_, entry| now.duration_since(entry.last_used_at) <= TTL);
    }
}

pub fn hash_icon_spec(spec: &IconSpec) -> u64 {
    let mut hasher = DefaultHasher::new();
    spec.rect.width.hash(&mut hasher);
    spec.rect.height.hash(&mut hasher);
    spec.app_id.hash(&mut hasher);
    if let Some(icon) = &spec.icon {
        icon.name.hash(&mut hasher);
        icon.bytes.as_ref().map(|bytes| bytes.len()).hash(&mut hasher);
        if let Some(bytes) = &icon.bytes {
            bytes.hash(&mut hasher);
        }
    }
    hasher.finish()
}

pub fn icon_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Ok(Vec::new());
    };
    let scale = OutputScale::from(output.current_scale().fractional_scale());
    let Some(decoration) = decorations.get(window) else {
        return Ok(Vec::new());
    };

    decoration
        .icon_buffers
        .iter()
        .filter_map(|icon| memory_icon_element(renderer, icon, output_geo, scale, alpha).transpose())
        .collect()
}

pub fn icon_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Vec<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    decoration
        .icon_buffers
        .iter()
        .filter_map(|icon| memory_icon_element(renderer, icon, output_geo, scale, alpha).transpose())
        .collect()
}

fn memory_icon_element(
    renderer: &mut GlesRenderer,
    icon: &CachedDecorationIcon,
    output_geo: Rectangle<i32, Logical>,
    scale: OutputScale<f64>,
    alpha: f32,
) -> Result<Option<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    if intersect_logical_rect(icon.rect, output_geo).is_none() {
        return Ok(None);
    }

    let local = Point::from((
        icon.rect.x - output_geo.loc.x,
        icon.rect.y - output_geo.loc.y,
    ));
    let physical: Point<i32, Physical> = local.to_f64().to_physical_precise_round(scale);
    let element = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        physical.to_f64(),
        &icon.buffer,
        Some(alpha.clamp(0.0, 1.0)),
        None,
        None,
        Kind::Unspecified,
    )?;
    Ok(Some(element))
}

fn icon_source_key(spec: &IconSpec) -> Option<String> {
    if let Some(icon) = spec.icon.as_ref() {
        if let Some(name) = icon.name.as_ref() {
            return Some(format!("name:{name}"));
        }
        if let Some(bytes) = icon.bytes.as_ref() {
            return Some(format!("bytes:{}", bytes.len()));
        }
    }

    spec.app_id.as_ref().map(|app_id| format!("app:{app_id}"))
}

fn icon_candidate_names(spec: &IconSpec) -> Vec<String> {
    let mut names = Vec::new();

    if let Some(icon) = spec.icon.as_ref().and_then(|icon| icon.name.as_ref()) {
        push_unique_name(&mut names, icon);
    }

    if let Some(app_id) = spec.app_id.as_deref() {
        push_unique_name(&mut names, app_id);
        if let Some(last) = app_id.rsplit('.').next() {
            push_unique_name(&mut names, last);
        }
        if let Some(stripped) = app_id.strip_suffix(".desktop") {
            push_unique_name(&mut names, stripped);
        }
    }

    names
}

fn push_unique_name(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|candidate| candidate == name) {
        names.push(name.to_string());
    }
}

fn find_icon_path_in_index(
    index: &HashMap<String, Vec<PathBuf>>,
    names: &[String],
    width: i32,
    height: i32,
) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, i32)> = None;
    let target = width.max(height);

    for name in names {
        let Some(paths) = index.get(name) else {
            continue;
        };
        for path in paths {
            consider_icon_path(path, target, &mut best);
        }
    }

    best.map(|(path, _)| path)
}

fn build_icon_index(
    dir: &Path,
    index: &mut HashMap<String, Vec<PathBuf>>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            build_icon_index(&path, index);
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(extension.to_ascii_lowercase().as_str(), "png" | "svg") {
            continue;
        }

        index.entry(stem.to_string()).or_default().push(path);
    }
}

fn consider_icon_path(path: &Path, target: i32, best: &mut Option<(PathBuf, i32)>) {
    let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
        return;
    };
    let size_penalty = guess_icon_size_from_path(path)
        .map(|size| (size - target).abs())
        .unwrap_or(512);
    let extension_penalty = if extension.eq_ignore_ascii_case("png") {
        0
    } else {
        10
    };
    let path_bonus = if path.to_string_lossy().contains("/apps/") {
        0
    } else if path.to_string_lossy().contains("/pixmaps/") {
        25
    } else {
        50
    };
    let score = size_penalty + path_bonus + extension_penalty;

    match best {
        Some((_, current_score)) if *current_score <= score => {}
        _ => *best = Some((path.to_path_buf(), score)),
    }
}

fn icon_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".local/share/icons"));
        roots.push(PathBuf::from(&home).join(".icons"));
    }

    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]);

    for dir in data_dirs {
        roots.push(dir.join("icons"));
        roots.push(dir.join("pixmaps"));
    }

    roots
}

fn guess_icon_size_from_path(path: &Path) -> Option<i32> {
    path.components().find_map(|component| {
        let component = component.as_os_str().to_string_lossy();
        let (width, height) = component.split_once('x')?;
        let width = width.parse::<i32>().ok()?;
        let height = height.parse::<i32>().ok()?;
        (width == height).then_some(width)
    })
}

fn decode_png_and_scale(bytes: &[u8], target_width: i32, target_height: i32) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    let source = &buffer[..info.buffer_size()];
    let rgba = match info.color_type {
        ColorType::Rgba => source.to_vec(),
        ColorType::Rgb => source
            .chunks_exact(3)
            .flat_map(|chunk| [chunk[0], chunk[1], chunk[2], 255])
            .collect(),
        ColorType::GrayscaleAlpha => source
            .chunks_exact(2)
            .flat_map(|chunk| [chunk[0], chunk[0], chunk[0], chunk[1]])
            .collect(),
        ColorType::Grayscale => source
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        _ => return None,
    };

    Some(scale_rgba(
        &rgba,
        info.width as i32,
        info.height as i32,
        target_width,
        target_height,
    ))
}

fn decode_svg_and_scale(bytes: &[u8], target_width: i32, target_height: i32) -> Option<Vec<u8>> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(target_width as u32, target_height as u32)?;

    let size = tree.size();
    let sx = target_width as f32 / size.width();
    let sy = target_height as f32 / size.height();
    let transform = tiny_skia::Transform::from_scale(sx, sy);

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    Some(pixmap.data().to_vec())
}

fn decode_icon_and_scale(
    bytes: &[u8],
    extension: Option<&str>,
    target_width: i32,
    target_height: i32,
) -> Option<Vec<u8>> {
    match extension {
        Some("png") => decode_png_and_scale(bytes, target_width, target_height),
        Some("svg") => decode_svg_and_scale(bytes, target_width, target_height),
        _ => decode_png_and_scale(bytes, target_width, target_height),
    }
}

fn scale_rgba(
    rgba: &[u8],
    source_width: i32,
    source_height: i32,
    target_width: i32,
    target_height: i32,
) -> Vec<u8> {
    if source_width == target_width && source_height == target_height {
        return rgba.to_vec();
    }

    let mut scaled = vec![0u8; (target_width * target_height * 4) as usize];

    for y in 0..target_height {
        for x in 0..target_width {
            let source_x = ((x as f32 / target_width as f32) * source_width as f32).floor() as i32;
            let source_y = ((y as f32 / target_height as f32) * source_height as f32).floor() as i32;
            let source_x = source_x.clamp(0, source_width - 1);
            let source_y = source_y.clamp(0, source_height - 1);

            let source_index = ((source_y * source_width + source_x) * 4) as usize;
            let target_index = ((y * target_width + x) * 4) as usize;
            scaled[target_index..target_index + 4]
                .copy_from_slice(&rgba[source_index..source_index + 4]);
        }
    }

    scaled
}

fn rgba_to_argb8888(rgba: &[u8]) -> Vec<u8> {
    let mut argb = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        let alpha = chunk[3] as u16;
        let red = ((chunk[0] as u16 * alpha) / 255) as u8;
        let green = ((chunk[1] as u16 * alpha) / 255) as u8;
        let blue = ((chunk[2] as u16 * alpha) / 255) as u8;
        argb.extend_from_slice(&[blue, green, red, chunk[3]]);
    }
    argb
}

fn intersect_logical_rect(
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
) -> Option<LogicalRect> {
    let left = rect.x.max(output_geo.loc.x);
    let top = rect.y.max(output_geo.loc.y);
    let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
    let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);

    (right > left && bottom > top).then(|| LogicalRect::new(left, top, right - left, bottom - top))
}
