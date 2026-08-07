use std::cell::{Cell, RefCell};
use std::{
    cmp::max,
    collections::{HashMap, VecDeque},
    ffi::{CStr, CString},
    fs,
    io::Cursor,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use png::ColorType;
use resvg::{tiny_skia, usvg};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ContextId, ExportMem, Frame as _, FrameContext as _, ImportMem, Offscreen,
            Renderer, Texture,
            damage::OutputDamageTracker,
            element::texture::TextureRenderElement,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture,
                Uniform, UniformName, ffi, link_program,
            },
            utils::{CommitCounter, OpaqueRegions},
        },
    },
    utils::{
        Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap,
    },
};
use tracing::{info, warn};

use crate::backend::visual::{PreciseLogicalRect, SnappedLogicalRect};
use crate::ssd::{
    BlendMode, CompiledEffect, EffectInput, EffectInvalidationPolicy, EffectStage,
    EffectStateResizePolicy, EffectStateTexture, EffectStateTextureFormat, LogicalRect, NoiseKind,
    NoiseStage, ShaderModule, ShaderStage, ShaderUniformValue,
};

#[derive(Debug, Clone)]
pub struct CachedShaderEffect {
    pub owner_node_id: Option<String>,
    pub stable_key: String,
    pub order: usize,
    pub rect: LogicalRect,
    pub rect_precise: Option<PreciseLogicalRect>,
    pub shader: CompiledEffect,
    pub clip_rect: Option<LogicalRect>,
    pub clip_radius: i32,
    pub clip_rect_precise: Option<PreciseLogicalRect>,
    pub clip_radius_precise: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct CachedBackdropTexture {
    pub signature: u64,
    pub texture: GlesTexture,
    pub id: Id,
    pub commit_counter: CommitCounter,
    pub sub_elements: HashMap<String, CachedBackdropSubElement>,
}

#[derive(Debug, Clone)]
pub struct CachedBackdropSubElement {
    pub id: Id,
    pub commit_counter: CommitCounter,
}

#[derive(Debug, Clone)]
pub struct WindowEffectElementState {
    pub signature: u64,
    pub id: Id,
    pub commit_counter: CommitCounter,
}

impl Default for WindowEffectElementState {
    fn default() -> Self {
        Self {
            signature: 0,
            id: Id::new(),
            commit_counter: CommitCounter::default(),
        }
    }
}

impl Default for CachedBackdropSubElement {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShaderEffectSpec {
    pub rect: Rectangle<i32, Logical>,
    pub geometry: Rectangle<i32, Physical>,
    pub framebuffer_regions: Vec<BackdropFramebufferRegion>,
    pub framebuffer_capture_padding: i32,
    pub shader: CompiledEffect,
    pub alpha_bits: u32,
    pub render_scale: f32,
    pub clip_rect: Option<SnappedLogicalRect>,
    pub clip_radius: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackdropFramebufferRegion {
    pub area: Rectangle<i32, Logical>,
    pub geometry: Rectangle<i32, Physical>,
}

#[derive(Debug, Clone)]
pub struct ShaderEffectElementState {
    id: Id,
    commit_counter: CommitCounter,
    last_spec: Option<ShaderEffectSpec>,
    backdrop_pipeline: Arc<Mutex<EffectInstancePipelineCache>>,
}

impl Default for ShaderEffectElementState {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
            last_spec: None,
            backdrop_pipeline: Arc::new(Mutex::new(EffectInstancePipelineCache::default())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StableShaderEffectElement {
    shader: GlesPixelProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    additional_uniforms: Vec<Uniform<'static>>,
    kind: Kind,
}

#[derive(Debug, Clone)]
pub struct StableBackdropFramebufferElement {
    shader: CompiledEffect,
    program: GlesTexProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    framebuffer_regions: Vec<BackdropFramebufferRegion>,
    framebuffer_capture_padding: i32,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<SnappedLogicalRect>,
    clip_radius: f32,
    popup_source: Option<GlesTexture>,
    pipeline: Arc<Mutex<EffectInstancePipelineCache>>,
    kind: Kind,
}

#[derive(Debug, Clone)]
pub struct StableBackdropTextureElement {
    texture: GlesTexture,
    program: GlesTexProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    src: Rectangle<f64, Buffer>,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<SnappedLogicalRect>,
    clip_radius: f32,
    uv_offset: [f32; 2],
    uv_scale: [f32; 2],
    debug_label: String,
    kind: Kind,
}

thread_local! {
    /// Shared FBO reused across every off-screen blur / blend draw on this
    /// thread. Allocated lazily on first use, never freed — the FBO ID is
    /// just an integer handle (no GPU memory of its own; the attached
    /// texture is what holds the pixels), so leaking it for the program's
    /// lifetime is essentially free.
    ///
    /// Background: every `blur_texture_pass` / `blend_textures` call used
    /// to do `glGenFramebuffers` + `glDeleteFramebuffers` per draw. With
    /// dual-Kawase blur at `passes: 2` that is 4 GL FBO lifecycle pairs
    /// per backdrop element, and N backdrops × ≥60 fps multiplies that
    /// pressure. On the NVIDIA proprietary driver each pair triggers a
    /// driver-side flush + bookkeeping update — perf logs showed it as
    /// part of the dominant `libnvidia-eglcore` busy-wait. A single
    /// reusable scratch FBO with `glFramebufferTexture2D` re-attachment
    /// avoids the churn entirely.
    static BLUR_SCRATCH_FBO: Cell<u32> = const { Cell::new(0) };
    static GPU_TIMING_STATE: RefCell<GpuTimingState> = RefCell::new(GpuTimingState::default());
    static SHARED_EFFECT_PIPELINE_CACHES: RefCell<SharedEffectPipelineCaches> =
        RefCell::new(SharedEffectPipelineCaches::default());
}

const GPU_TIMING_QUERY_COUNT: usize = 2048;
const GPU_TIMING_REPORT_INTERVAL: Duration = Duration::from_secs(1);
const SHARED_EFFECT_PIPELINE_CACHE_LIMIT: usize = 128;

pub fn purge_shared_effect_pipeline_caches_for_window(window_id: &str) {
    SHARED_EFFECT_PIPELINE_CACHES.with(|caches| {
        let removed = caches.borrow_mut().purge_window(window_id);
        if removed > 0 {
            info!(
                window_id,
                removed, "purged closed window shader pipeline caches"
            );
        }
    });
}

#[derive(Debug, Default)]
struct SnapshotFallbackAggregate {
    samples: u64,
    total_scene_elements: u64,
    total_pixels: u64,
}

#[derive(Debug)]
struct SnapshotFallbackDebugState {
    aggregates: HashMap<&'static str, SnapshotFallbackAggregate>,
    last_report: Instant,
}

impl Default for SnapshotFallbackDebugState {
    fn default() -> Self {
        Self {
            aggregates: HashMap::new(),
            last_report: Instant::now(),
        }
    }
}

fn snapshot_fallback_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        gpu_timing_debug_enabled()
            || std::env::var_os("SHOJI_SNAPSHOT_FALLBACK_DEBUG")
                .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

pub(crate) fn record_snapshot_fallback(
    source: &'static str,
    size: (i32, i32),
    scene_elements: usize,
) {
    if !snapshot_fallback_debug_enabled() {
        return;
    }

    static STATE: OnceLock<Mutex<SnapshotFallbackDebugState>> = OnceLock::new();
    let state = STATE.get_or_init(|| Mutex::new(SnapshotFallbackDebugState::default()));
    let Ok(mut state) = state.lock() else {
        return;
    };

    let pixels = size.0.max(0) as u64 * size.1.max(0) as u64;
    let aggregate = state.aggregates.entry(source).or_default();
    aggregate.samples += 1;
    aggregate.total_scene_elements = aggregate
        .total_scene_elements
        .saturating_add(scene_elements as u64);
    aggregate.total_pixels = aggregate.total_pixels.saturating_add(pixels);

    if state.last_report.elapsed() < GPU_TIMING_REPORT_INTERVAL {
        return;
    }
    state.last_report = Instant::now();

    for (source, aggregate) in state.aggregates.drain() {
        info!(
            source,
            samples = aggregate.samples,
            average_scene_elements =
                aggregate.total_scene_elements as f64 / aggregate.samples.max(1) as f64,
            average_megapixels =
                aggregate.total_pixels as f64 / aggregate.samples.max(1) as f64 / 1_000_000.0,
            "snapshot fallback aggregate"
        );
    }
}

#[derive(Debug)]
struct PendingGpuTiming {
    label: &'static str,
    start_query: u32,
    end_query: u32,
    pixels: u64,
}

#[derive(Debug, Default)]
struct GpuTimingAggregate {
    samples: u64,
    total_ns: u64,
    max_ns: u64,
    total_pixels: u64,
}

#[derive(Debug)]
struct GpuTimingState {
    initialized: bool,
    supported: bool,
    free_queries: Vec<u32>,
    pending: VecDeque<PendingGpuTiming>,
    aggregates: HashMap<&'static str, GpuTimingAggregate>,
    last_report: Instant,
}

impl Default for GpuTimingState {
    fn default() -> Self {
        Self {
            initialized: false,
            supported: false,
            free_queries: Vec::new(),
            pending: VecDeque::new(),
            aggregates: HashMap::new(),
            last_report: Instant::now(),
        }
    }
}

impl GpuTimingState {
    fn ensure_initialized(&mut self, gl: &ffi::Gles2) {
        if self.initialized {
            return;
        }
        self.initialized = true;

        let extensions = unsafe {
            let ptr = gl.GetString(ffi::EXTENSIONS);
            (!ptr.is_null()).then(|| CStr::from_ptr(ptr.cast()).to_string_lossy())
        };
        self.supported = extensions.as_deref().is_some_and(|extensions| {
            extensions
                .split_ascii_whitespace()
                .any(|extension| extension == "GL_EXT_disjoint_timer_query")
        });
        if !self.supported {
            warn!("GPU timing debug requested but GL_EXT_disjoint_timer_query is unavailable");
            return;
        }

        self.free_queries.resize(GPU_TIMING_QUERY_COUNT, 0);
        unsafe {
            gl.GenQueriesEXT(
                self.free_queries.len() as ffi::types::GLsizei,
                self.free_queries.as_mut_ptr(),
            );
        }
        info!(
            query_count = self.free_queries.len(),
            "GPU timing debug initialized"
        );
    }

    fn collect(&mut self, gl: &ffi::Gles2) {
        while let Some(front) = self.pending.front() {
            let mut available = 0;
            unsafe {
                gl.GetQueryObjectuivEXT(
                    front.end_query,
                    ffi::QUERY_RESULT_AVAILABLE,
                    &mut available,
                );
            }
            if available == 0 {
                break;
            }

            let pending = self.pending.pop_front().expect("front should exist");
            let mut start_ns = 0;
            let mut end_ns = 0;
            unsafe {
                gl.GetQueryObjecti64vEXT(pending.start_query, ffi::QUERY_RESULT, &mut start_ns);
                gl.GetQueryObjecti64vEXT(pending.end_query, ffi::QUERY_RESULT, &mut end_ns);
            }
            self.free_queries.push(pending.start_query);
            self.free_queries.push(pending.end_query);

            let elapsed_ns = end_ns.saturating_sub(start_ns) as u64;
            let aggregate = self.aggregates.entry(pending.label).or_default();
            aggregate.samples += 1;
            aggregate.total_ns = aggregate.total_ns.saturating_add(elapsed_ns);
            aggregate.max_ns = aggregate.max_ns.max(elapsed_ns);
            aggregate.total_pixels = aggregate.total_pixels.saturating_add(pending.pixels);
        }

        if self.last_report.elapsed() < GPU_TIMING_REPORT_INTERVAL || self.aggregates.is_empty() {
            return;
        }
        self.last_report = Instant::now();

        for (label, aggregate) in self.aggregates.drain() {
            info!(
                label,
                samples = aggregate.samples,
                total_gpu_ms = aggregate.total_ns as f64 / 1_000_000.0,
                average_gpu_ms =
                    aggregate.total_ns as f64 / aggregate.samples.max(1) as f64 / 1_000_000.0,
                max_gpu_ms = aggregate.max_ns as f64 / 1_000_000.0,
                average_megapixels =
                    aggregate.total_pixels as f64 / aggregate.samples.max(1) as f64 / 1_000_000.0,
                pending_spans = self.pending.len(),
                "GPU timing aggregate"
            );
        }
    }

    fn begin(
        &mut self,
        gl: &ffi::Gles2,
        label: &'static str,
        size: (i32, i32),
    ) -> Option<PendingGpuTiming> {
        self.ensure_initialized(gl);
        if !self.supported {
            return None;
        }
        self.collect(gl);

        if self.free_queries.len() < 2 {
            return None;
        }
        let end_query = self
            .free_queries
            .pop()
            .expect("query pool should have two entries");
        let start_query = self.free_queries.pop()?;
        unsafe {
            gl.QueryCounterEXT(start_query, ffi::TIMESTAMP_EXT);
        }
        Some(PendingGpuTiming {
            label,
            start_query,
            end_query,
            pixels: size.0.max(0) as u64 * size.1.max(0) as u64,
        })
    }

    fn end(&mut self, gl: &ffi::Gles2, pending: PendingGpuTiming) {
        unsafe {
            gl.QueryCounterEXT(pending.end_query, ffi::TIMESTAMP_EXT);
        }
        self.pending.push_back(pending);
    }
}

pub(crate) fn gpu_timing_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("SHOJI_GPU_TIMING_DEBUG")
            .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

pub(crate) fn gpu_element_timing_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        gpu_timing_debug_enabled()
            && std::env::var_os("SHOJI_GPU_ELEMENT_TIMING_DEBUG")
                .is_some_and(|value| value != "0" && !value.is_empty())
    })
}

pub(crate) struct GpuTimingFrameSpan(PendingGpuTiming);

pub(crate) fn begin_gpu_timing_frame_span(
    frame: &mut GlesFrame<'_, '_>,
    label: &'static str,
    size: (i32, i32),
) -> Option<GpuTimingFrameSpan> {
    if !gpu_timing_debug_enabled() {
        return None;
    }

    frame
        .with_context(|gl| GPU_TIMING_STATE.with(|state| state.borrow_mut().begin(gl, label, size)))
        .ok()
        .flatten()
        .map(GpuTimingFrameSpan)
}

pub(crate) fn end_gpu_timing_frame_span(
    frame: &mut GlesFrame<'_, '_>,
    span: Option<GpuTimingFrameSpan>,
) {
    let Some(GpuTimingFrameSpan(pending)) = span else {
        return;
    };
    let _ = frame.with_context(|gl| {
        GPU_TIMING_STATE.with(|state| state.borrow_mut().end(gl, pending));
    });
}

fn with_gpu_timing_gl_span<R>(
    gl: &ffi::Gles2,
    label: &'static str,
    size: (i32, i32),
    func: impl FnOnce() -> R,
) -> R {
    if !gpu_timing_debug_enabled() {
        return func();
    }

    let pending = GPU_TIMING_STATE.with(|state| state.borrow_mut().begin(gl, label, size));
    let result = func();
    if let Some(pending) = pending {
        GPU_TIMING_STATE.with(|state| state.borrow_mut().end(gl, pending));
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Cached captures may include a blur halo and need a cropped opaque texture for reuse. Direct
// framebuffer captures already match the displayed rectangle, so the final display draw can force
// opacity without materializing another full-area texture.
enum BackdropFinishMode {
    Materialize,
    DeferToDisplay,
}

pub(crate) fn with_gpu_timing_renderer_span<R>(
    renderer: &mut GlesRenderer,
    label: &'static str,
    size: (i32, i32),
    func: impl FnOnce(&mut GlesRenderer) -> R,
) -> R {
    if !gpu_timing_debug_enabled() {
        return func(renderer);
    }

    let pending = renderer
        .with_context(|gl| GPU_TIMING_STATE.with(|state| state.borrow_mut().begin(gl, label, size)))
        .ok()
        .flatten();
    let result = func(renderer);
    if let Some(pending) = pending {
        let _ = renderer.with_context(|gl| {
            GPU_TIMING_STATE.with(|state| state.borrow_mut().end(gl, pending));
        });
    }
    result
}

pub fn framebuffer_backdrop_element_for_output_rect(
    renderer: &mut GlesRenderer,
    state: &mut ShaderEffectElementState,
    rect: LogicalRect,
    effect: CompiledEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<StableBackdropFramebufferElement, ShaderEffectError> {
    framebuffer_backdrop_element_for_output_rects(
        renderer,
        state,
        &[rect],
        effect,
        output_geo,
        scale,
        alpha,
    )?
    .ok_or(ShaderEffectError::Gles(GlesError::FramebufferBindingError))
}

pub fn framebuffer_backdrop_element_for_output_rects(
    renderer: &mut GlesRenderer,
    state: &mut ShaderEffectElementState,
    rects: &[LogicalRect],
    effect: CompiledEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableBackdropFramebufferElement>, ShaderEffectError> {
    framebuffer_backdrop_element_for_output_rects_with_popup_source(
        renderer, state, rects, effect, output_geo, scale, alpha, None,
    )
}

pub fn framebuffer_backdrop_element_for_output_rects_with_popup_source(
    renderer: &mut GlesRenderer,
    state: &mut ShaderEffectElementState,
    rects: &[LogicalRect],
    effect: CompiledEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
    popup_source: Option<GlesTexture>,
) -> Result<Option<StableBackdropFramebufferElement>, ShaderEffectError> {
    let Some(rect) = crate::backend::window::bounding_box_for_rects(rects) else {
        return Ok(None);
    };
    let area = LogicalRect::new(
        rect.x - output_geo.loc.x,
        rect.y - output_geo.loc.y,
        rect.width,
        rect.height,
    );
    let geometry =
        crate::backend::visual::logical_rect_to_physical_rect(rect, output_geo.loc, scale);
    let framebuffer_capture_padding = framebuffer_capture_padding(&effect, scale.x as f32);
    let framebuffer_regions = if rects.len() == 1 && rects[0] == rect {
        Vec::new()
    } else {
        rects
            .iter()
            .copied()
            .map(|region| {
                let region_area = Rectangle::new(
                    Point::from((region.x - rect.x, region.y - rect.y)),
                    (region.width, region.height).into(),
                );
                let mut region_geometry = crate::backend::visual::logical_rect_to_physical_rect(
                    region,
                    output_geo.loc,
                    scale,
                );
                region_geometry.loc -= geometry.loc;
                BackdropFramebufferRegion {
                    area: region_area,
                    geometry: region_geometry,
                }
            })
            .collect()
    };
    let mut element = state.backdrop_element(
        renderer,
        ShaderEffectSpec {
            rect: Rectangle::new(
                Point::from((area.x, area.y)),
                (area.width, area.height).into(),
            ),
            geometry,
            framebuffer_regions,
            framebuffer_capture_padding,
            shader: effect,
            alpha_bits: alpha.to_bits(),
            render_scale: scale.x as f32,
            clip_rect: None,
            clip_radius: 0.0,
        },
    )?;
    element.popup_source = popup_source;
    Ok(Some(element))
}

/// Ensures a thread-local scratch FBO exists and returns its id. Must be
/// called from inside a GL context (`with_context`). Re-binding the FBO and
/// re-attaching textures is the caller's responsibility.
///
/// # Safety
/// The caller guarantees they hold a current GL context for the calling
/// thread.
#[inline]
unsafe fn ensure_blur_scratch_fbo(gl: &smithay::backend::renderer::gles::ffi::Gles2) -> u32 {
    BLUR_SCRATCH_FBO.with(|cell| {
        let mut fbo = cell.get();
        if fbo == 0 {
            unsafe { gl.GenFramebuffers(1, &mut fbo as *mut _) };
            cell.set(fbo);
        }
        fbo
    })
}

#[derive(Debug, Default)]
struct BackdropFramebufferCache {
    framebuffer: Option<GlesTexture>,
    rendered: Option<GlesTexture>,
    sample_src: Option<Rectangle<f64, Buffer>>,
}

/// Per-element render targets for a compiled effect pipeline. Slots are
/// consumed in execution order and reused on subsequent frames. This keeps
/// the cache independent of JSX shape while avoiding per-frame texture
/// allocation for shader, blend, crop, finish, and blur stages.
#[derive(Debug, Default)]
struct EffectPipelineCache {
    targets: Vec<EffectPipelineTarget>,
    next_target: usize,
    blur_pyramids: Vec<Vec<GlesTexture>>,
    next_blur_pyramid: usize,
    states: HashMap<String, EffectStateSlot>,
    target_format: Option<Fourcc>,
}

#[derive(Debug, Default)]
struct EffectInstancePipelineCache {
    renderer_context_id: Option<ContextId<GlesTexture>>,
    pipeline: EffectPipelineCache,
}

impl EffectInstancePipelineCache {
    fn begin_frame(&mut self, renderer: &GlesRenderer) -> &mut EffectPipelineCache {
        let renderer_context_id = renderer.context_id();
        if self.renderer_context_id.as_ref() != Some(&renderer_context_id) {
            self.renderer_context_id = Some(renderer_context_id);
            self.pipeline = EffectPipelineCache::default();
        }
        self.pipeline.begin_frame();
        &mut self.pipeline
    }
}

#[derive(Debug)]
struct EffectPipelineTarget {
    texture: GlesTexture,
    format: Fourcc,
}

#[derive(Debug)]
struct EffectStateSlot {
    descriptor: EffectStateTexture,
    textures: [GlesTexture; 2],
    current: usize,
}

impl EffectPipelineCache {
    fn begin_frame(&mut self) {
        self.next_target = 0;
        self.next_blur_pyramid = 0;
    }

    fn target(
        &mut self,
        renderer: &mut GlesRenderer,
        size: (i32, i32),
    ) -> Result<GlesTexture, ShaderEffectError> {
        let index = self.next_target;
        self.next_target += 1;
        let expected = Size::<i32, Buffer>::from(size);
        let format = self.target_format.unwrap_or(Fourcc::Abgr8888);
        if self
            .targets
            .get(index)
            .is_none_or(|target| target.texture.size() != expected || target.format != format)
        {
            let target = EffectPipelineTarget {
                texture: Offscreen::<GlesTexture>::create_buffer(renderer, format, expected)?,
                format,
            };
            if index == self.targets.len() {
                self.targets.push(target);
            } else {
                self.targets[index] = target;
            }
        }
        Ok(self.targets[index].texture.clone())
    }

    fn blur_pyramid(&mut self) -> &mut Vec<GlesTexture> {
        let index = self.next_blur_pyramid;
        self.next_blur_pyramid += 1;
        if index == self.blur_pyramids.len() {
            self.blur_pyramids.push(Vec::new());
        }
        &mut self.blur_pyramids[index]
    }

    fn state_texture(
        &mut self,
        renderer: &mut GlesRenderer,
        descriptor: &EffectStateTexture,
        base_size: (i32, i32),
    ) -> Result<GlesTexture, ShaderEffectError> {
        self.ensure_state(renderer, descriptor, base_size)?;
        let slot = self
            .states
            .get(&descriptor.name)
            .expect("state was ensured");
        Ok(slot.textures[slot.current].clone())
    }

    fn commit_state(
        &mut self,
        renderer: &mut GlesRenderer,
        descriptor: &EffectStateTexture,
        base_size: (i32, i32),
        source: &GlesTexture,
    ) -> Result<(), ShaderEffectError> {
        self.ensure_state(renderer, descriptor, base_size)?;
        let slot = self
            .states
            .get_mut(&descriptor.name)
            .expect("state was ensured");
        let next = 1 - slot.current;
        copy_effect_texture(renderer, source, &slot.textures[next])?;
        slot.current = next;
        Ok(())
    }

    fn ensure_state(
        &mut self,
        renderer: &mut GlesRenderer,
        descriptor: &EffectStateTexture,
        base_size: (i32, i32),
    ) -> Result<(), ShaderEffectError> {
        let size = effect_state_size(base_size, descriptor.scale);
        let needs_reallocate = self.states.get(&descriptor.name).is_none_or(|slot| {
            slot.descriptor != *descriptor
                || slot.textures[slot.current].size() != Size::<i32, Buffer>::from(size)
        });
        if !needs_reallocate {
            return Ok(());
        }

        let previous = self
            .states
            .get(&descriptor.name)
            .map(|slot| slot.textures[slot.current].clone());
        let format = effect_state_fourcc(descriptor.format);
        let textures = [
            Offscreen::<GlesTexture>::create_buffer(renderer, format, size.into())?,
            Offscreen::<GlesTexture>::create_buffer(renderer, format, size.into())?,
        ];
        clear_effect_texture(renderer, &textures[0])?;
        clear_effect_texture(renderer, &textures[1])?;
        if descriptor.resize == EffectStateResizePolicy::Stretch
            && let Some(previous) = previous
        {
            copy_effect_texture(renderer, &previous, &textures[0])?;
        }
        self.states.insert(
            descriptor.name.clone(),
            EffectStateSlot {
                descriptor: descriptor.clone(),
                textures,
                current: 0,
            },
        );
        Ok(())
    }
}

fn effect_state_size(base_size: (i32, i32), scale: f32) -> (i32, i32) {
    (
        ((base_size.0 as f32 * scale).round() as i32).max(1),
        ((base_size.1 as f32 * scale).round() as i32).max(1),
    )
}

fn effect_state_fourcc(format: EffectStateTextureFormat) -> Fourcc {
    match format {
        EffectStateTextureFormat::Rgba8 => Fourcc::Abgr8888,
        // Smithay currently exposes RGBA16F, but not an RG16F offscreen
        // target. Keep the RG API semantics while storing unused BA channels.
        EffectStateTextureFormat::Rg16f | EffectStateTextureFormat::Rgba16f => {
            Fourcc::Abgr16161616f
        }
    }
}

fn clear_effect_texture(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
) -> Result<(), ShaderEffectError> {
    renderer.with_context(|gl| unsafe {
        let fbo = ensure_blur_scratch_fbo(gl);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::DRAW_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            texture.tex_id(),
            0,
        );
        gl.Disable(ffi::SCISSOR_TEST);
        gl.ClearColor(0.0, 0.0, 0.0, 0.0);
        gl.Clear(ffi::COLOR_BUFFER_BIT);
        gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
        gl.Enable(ffi::SCISSOR_TEST);
        Ok::<_, GlesError>(())
    })??;
    Ok(())
}

fn copy_effect_texture(
    renderer: &mut GlesRenderer,
    source: &GlesTexture,
    target: &GlesTexture,
) -> Result<(), ShaderEffectError> {
    renderer.render_texture_to_texture(
        source,
        target,
        Rectangle::from_size(source.size().to_f64()),
        None,
        &[],
    )?;
    Ok(())
}

#[derive(Debug)]
struct SharedEffectPipelineCache {
    renderer_context_id: ContextId<GlesTexture>,
    pipeline: EffectPipelineCache,
    last_used: u64,
}

#[derive(Debug, Default)]
struct SharedEffectPipelineCaches {
    generation: u64,
    entries: HashMap<String, SharedEffectPipelineCache>,
}

impl SharedEffectPipelineCaches {
    fn purge_window(&mut self, window_id: &str) -> usize {
        let before = self.entries.len();
        let window_token = format!(":{window_id}:");
        self.entries.retain(|key, _| {
            !key.starts_with(&format!("winit:window-backdrop:{window_id}:"))
                && !key.starts_with(&format!("winit:protocol-window:{window_id}:"))
                && !key.contains(&window_token)
        });
        before.saturating_sub(self.entries.len())
    }

    fn pipeline<'a>(
        &'a mut self,
        renderer: &GlesRenderer,
        cache_key: String,
    ) -> &'a mut EffectPipelineCache {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let renderer_context_id = renderer.context_id();

        if !self.entries.contains_key(&cache_key)
            && self.entries.len() >= SHARED_EFFECT_PIPELINE_CACHE_LIMIT
            && let Some(oldest_key) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest_key);
        }

        let entry = self
            .entries
            .entry(cache_key)
            .or_insert_with(|| SharedEffectPipelineCache {
                renderer_context_id: renderer_context_id.clone(),
                pipeline: EffectPipelineCache::default(),
                last_used: generation,
            });
        if entry.renderer_context_id != renderer_context_id {
            *entry = SharedEffectPipelineCache {
                renderer_context_id,
                pipeline: EffectPipelineCache::default(),
                last_used: generation,
            };
        } else {
            entry.last_used = generation;
        }
        entry.pipeline.begin_frame();
        &mut entry.pipeline
    }
}

#[derive(Debug, Default)]
struct ShaderProgramCache(Mutex<HashMap<String, GlesPixelProgram>>);
#[derive(Debug, Clone)]
struct BlurShaderPrograms {
    down: BlurProgramInternal,
    up: BlurProgramInternal,
    renderer_context_id: ContextId<GlesTexture>,
}

#[derive(Debug, Default)]
struct BlurShaderProgramCache(Mutex<Option<BlurShaderPrograms>>);
#[derive(Debug, Default)]
struct TextureStageProgramCache(Mutex<HashMap<String, GlesTexProgram>>);
#[derive(Debug, Clone)]
struct MultiTextureStageProgram {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_texture_size: ffi::types::GLint,
    uniform_content_rect: ffi::types::GLint,
    texture_uniforms: Vec<(String, ffi::types::GLint)>,
    value_uniforms: Vec<(String, ffi::types::GLint)>,
    attrib_vert: ffi::types::GLint,
    renderer_context_id: ContextId<GlesTexture>,
}
#[derive(Debug, Default)]
struct MultiTextureStageProgramCache(Mutex<HashMap<String, MultiTextureStageProgram>>);
#[derive(Debug)]
struct DisplayTextureProgram(GlesTexProgram);
#[derive(Debug)]
struct DisplayTextureProgramPreserveAlpha(GlesTexProgram);
#[derive(Debug)]
struct NoiseSaltProgram(GlesTexProgram);
#[derive(Debug)]
struct OpaqueFinishProgram(GlesTexProgram);
#[derive(Debug)]
struct AlphaPreservingFinishProgram(GlesTexProgram);
#[derive(Debug, Default)]
struct ImageTextureCache(Mutex<HashMap<(String, i32, i32), GlesTexture>>);

struct EffectExecutionContext {
    backdrop: GlesTexture,
    xray_backdrop: Option<GlesTexture>,
    layer_source: Option<GlesTexture>,
    popup_source: Option<GlesTexture>,
    size: (i32, i32),
    state_base_size: (i32, i32),
    content_rect: Rectangle<i32, Buffer>,
    named: HashMap<String, GlesTexture>,
}

#[derive(Debug, Clone, Copy)]
struct BlurProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_half_pixel: ffi::types::GLint,
    uniform_offset: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

#[derive(Debug, Clone)]
struct BlendProgramInternal {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_tex2: ffi::types::GLint,
    uniform_blend_mode: ffi::types::GLint,
    uniform_blend_alpha: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

#[derive(Debug, Clone)]
struct BlendPrograms {
    program: BlendProgramInternal,
    renderer_context_id: ContextId<GlesTexture>,
}

#[derive(Debug, Default)]
struct BlendProgramCache(Mutex<Option<BlendPrograms>>);

#[derive(Debug, Default)]
struct SolidWhiteTextureCache(Mutex<Option<GlesTexture>>);

#[derive(Debug, thiserror::Error)]
pub enum ShaderEffectError {
    #[error("failed to read shader source at {path}: {source}")]
    ReadShader {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("persistent effect state requires an instance pipeline cache")]
    StateRequiresCache,
    #[error("effect input `{input}` is not available in this effect placement")]
    UnavailableInput { input: &'static str },
    #[error("named texture `{name}` was not written by a save() stage before get()")]
    MissingNamedTexture { name: String },
    #[error("effect shader program was compiled for a different renderer context")]
    RendererContextMismatch,
    #[error("failed to decode effect image `{path}`")]
    ImageDecode { path: String },
    #[error(transparent)]
    Gles(#[from] GlesError),
}

impl ShaderEffectElementState {
    pub fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: ShaderEffectSpec,
    ) -> Result<StableShaderEffectElement, ShaderEffectError> {
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec.clone());
        }

        let shader = compile_shader_program(renderer, &spec.shader)?;
        Ok(StableShaderEffectElement {
            shader,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            area: spec.rect,
            geometry: spec.geometry,
            alpha: f32::from_bits(spec.alpha_bits).clamp(0.0, 1.0),
            additional_uniforms: uniforms_for_spec(&spec),
            kind: Kind::Unspecified,
        })
    }

    pub fn backdrop_element(
        &mut self,
        renderer: &mut GlesRenderer,
        spec: ShaderEffectSpec,
    ) -> Result<StableBackdropFramebufferElement, ShaderEffectError> {
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec.clone());
        }

        // Framebuffer effects run their pipeline with DeferToDisplay (no
        // materialize finish pass), so this display program is the only place
        // the effect's alpha mode is applied. Effects declaring
        // `alpha: "preserve"` (e.g. popup blur masks) must keep the
        // pipeline's alpha; forcing it opaque would turn masked-out regions
        // into opaque black.
        let program = match spec.shader.alpha {
            crate::ssd::EffectAlphaMode::Preserve => {
                compile_display_texture_program_preserve_alpha(renderer)?
            }
            crate::ssd::EffectAlphaMode::Opaque => compile_display_texture_program(renderer)?,
        };
        Ok(StableBackdropFramebufferElement {
            shader: spec.shader,
            program,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            area: spec.rect,
            geometry: spec.geometry,
            framebuffer_regions: spec.framebuffer_regions,
            framebuffer_capture_padding: spec.framebuffer_capture_padding.max(0),
            alpha: f32::from_bits(spec.alpha_bits).clamp(0.0, 1.0),
            render_scale: spec.render_scale,
            clip_rect: spec.clip_rect,
            clip_radius: spec.clip_radius,
            popup_source: None,
            pipeline: self.backdrop_pipeline.clone(),
            kind: Kind::Unspecified,
        })
    }
}

impl Element for StableShaderEffectElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let _ = scale;
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

impl RenderElement<GlesRenderer> for StableShaderEffectElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_pixel_shader_to(
            &self.shader,
            src,
            dst,
            self.area.size.to_buffer(1, Transform::Normal),
            Some(damage),
            self.alpha,
            &self.additional_uniforms,
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl Element for StableBackdropFramebufferElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.area.size.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let _ = scale;
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }

    fn is_framebuffer_effect(&self) -> bool {
        true
    }

    fn framebuffer_capture_region(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        // capture_framebuffer samples dst expanded by the blur padding; the
        // damage tracker must treat that whole rect as the capture source so
        // the padding ring is redrawn (below this element) before capture.
        let padding = self.framebuffer_capture_padding.max(0);
        Rectangle::new(
            Point::from((self.geometry.loc.x - padding, self.geometry.loc.y - padding)),
            (
                self.geometry
                    .size
                    .w
                    .saturating_add(padding.saturating_mul(2)),
                self.geometry
                    .size
                    .h
                    .saturating_add(padding.saturating_mul(2)),
            )
                .into(),
        )
    }
}

impl RenderElement<GlesRenderer> for StableBackdropFramebufferElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let Some(cache) = cache else {
            return Ok(());
        };
        let Some(inner) = cache.get::<RefCell<BackdropFramebufferCache>>() else {
            return Ok(());
        };
        let inner = inner.borrow();
        let Some(texture) = inner.rendered.as_ref().or(inner.framebuffer.as_ref()) else {
            return Ok(());
        };
        let sample_src = inner
            .sample_src
            .unwrap_or_else(|| Rectangle::from_size(texture.size().to_f64()));

        let clip_rect = self
            .clip_rect
            .map(|clip| [clip.x, clip.y, clip.width, clip.height])
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let radius = self.clip_radius.max(0.0);
        let full_size = texture.size();
        let uv_offset = [
            sample_src.loc.x as f32 / full_size.w.max(1) as f32,
            sample_src.loc.y as f32 / full_size.h.max(1) as f32,
        ];
        let uv_scale = [
            sample_src.size.w as f32 / full_size.w.max(1) as f32,
            sample_src.size.h as f32 / full_size.h.max(1) as f32,
        ];

        if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
            // Rate-limited: these run every frame per backdrop element.
            use std::sync::atomic::{AtomicUsize, Ordering};
            static DRAW_LOG_TICK: AtomicUsize = AtomicUsize::new(0);
            if DRAW_LOG_TICK
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(240)
            {
                tracing::info!(
                    dst = ?dst,
                    damage = ?damage,
                    area = ?self.area,
                    geometry = ?self.geometry,
                    clip_rect = ?self.clip_rect,
                    clip_radius = radius,
                    render_scale = self.render_scale,
                    sample_src = ?sample_src,
                    texture_size = ?full_size,
                    uv_offset = ?uv_offset,
                    uv_scale = ?uv_scale,
                    used_rendered = inner.rendered.is_some(),
                    "gap debug framebuffer backdrop display draw"
                );
                if std::env::var_os("SHOJI_GAP_TEXTURE_READBACK").is_some() {
                    // Read the right-edge columns of the pipeline output and
                    // the raw capture. If the output's last column matches the
                    // raw capture instead of blurred content, the blur/effect
                    // chain is not writing that column.
                    let rendered_tex = texture.clone();
                    let capture_tex = inner.framebuffer.clone();
                    let _ = frame.with_context(|gl| unsafe {
                        let mut prev_read_fbo = 0i32;
                        gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut prev_read_fbo);
                        let fbo = ensure_blur_scratch_fbo(gl);
                        let dump = |label: &str, tex: &GlesTexture| {
                            let size = tex.size();
                            gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, fbo);
                            gl.FramebufferTexture2D(
                                ffi::READ_FRAMEBUFFER,
                                ffi::COLOR_ATTACHMENT0,
                                ffi::TEXTURE_2D,
                                tex.tex_id(),
                                0,
                            );
                            let w = 3.min(size.w);
                            let rows = [size.h / 4, size.h / 2, size.h * 3 / 4];
                            let mut pixels = [0u8; 3 * 4];
                            let mut out: Vec<[[u8; 4]; 3]> = Vec::new();
                            for y in rows {
                                gl.ReadPixels(
                                    size.w - w,
                                    y.clamp(0, size.h - 1),
                                    w,
                                    1,
                                    ffi::RGBA,
                                    ffi::UNSIGNED_BYTE,
                                    pixels.as_mut_ptr().cast(),
                                );
                                let mut row = [[0u8; 4]; 3];
                                for (i, px) in pixels.chunks_exact(4).enumerate() {
                                    row[i] = [px[0], px[1], px[2], px[3]];
                                }
                                out.push(row);
                            }
                            tracing::info!(
                                label,
                                tex_size = ?size,
                                right_edge_rows = ?out,
                                "gap debug texture right-edge readback"
                            );
                        };
                        dump("pipeline-output", &rendered_tex);
                        if let Some(capture_tex) = capture_tex.as_ref() {
                            dump("raw-capture", capture_tex);
                        }
                        gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, prev_read_fbo as u32);
                    });
                }
            }
        }

        let timing =
            begin_gpu_timing_frame_span(frame, "backdrop-display-draw", (dst.size.w, dst.size.h));
        let result = if self.framebuffer_regions.is_empty() {
            frame.render_texture_from_to(
                texture,
                sample_src,
                dst,
                damage,
                opaque_regions,
                Transform::Normal,
                self.alpha,
                Some(&self.program),
                &[
                    Uniform::new("uv_offset", uv_offset),
                    Uniform::new("uv_scale", uv_scale),
                    Uniform::new(
                        "rect_size",
                        [self.area.size.w as f32, self.area.size.h as f32],
                    ),
                    Uniform::new("render_scale", self.render_scale.max(1.0)),
                    Uniform::new(
                        "clip_enabled",
                        if clip_rect[2] > 0.0 && clip_rect[3] > 0.0 {
                            1.0f32
                        } else {
                            0.0f32
                        },
                    ),
                    Uniform::new("clip_rect", clip_rect),
                    Uniform::new("clip_radius", [radius, radius, radius, radius]),
                ],
            )
        } else {
            self.draw_framebuffer_regions(frame, texture, sample_src, dst, damage, opaque_regions)
        };
        end_gpu_timing_frame_span(frame, timing);
        result
    }

    fn capture_framebuffer(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), GlesError> {
        let inner = cache.get_or_insert::<RefCell<BackdropFramebufferCache>, _>(|| {
            RefCell::new(BackdropFramebufferCache::default())
        });
        let mut inner = inner.borrow_mut();
        let output_rect = Rectangle::from_size(frame.output_size());
        let padding = self.framebuffer_capture_padding;
        let capture_rect = Rectangle::new(
            Point::from((dst.loc.x - padding, dst.loc.y - padding)),
            (
                dst.size.w.saturating_add(padding.saturating_mul(2)),
                dst.size.h.saturating_add(padding.saturating_mul(2)),
            )
                .into(),
        );
        let actual_capture_rect = match capture_rect.intersection(output_rect) {
            Some(clamped) => clamped,
            None => return Ok(()),
        };
        let size =
            Size::<i32, Buffer>::from((actual_capture_rect.size.w, actual_capture_rect.size.h));
        let sample_src = Rectangle::new(
            Point::from((
                (dst.loc.x - actual_capture_rect.loc.x) as f64,
                (dst.loc.y - actual_capture_rect.loc.y) as f64,
            )),
            (dst.size.w as f64, dst.size.h as f64).into(),
        );

        {
            let mut guard = frame.renderer();
            let renderer = guard.as_mut();
            let recreate = inner
                .framebuffer
                .as_ref()
                .is_none_or(|fb| fb.size() != size);
            if recreate {
                inner.framebuffer = Some(renderer.create_buffer(Fourcc::Abgr8888, size)?);
            }
            inner.rendered = None;
            inner.sample_src = Some(sample_src);
        }

        let framebuffer_texture = inner
            .framebuffer
            .as_ref()
            .expect("framebuffer texture should exist")
            .clone();

        // Reuse the thread-local scratch FBO instead of `glGenFramebuffers`
        // / `glDeleteFramebuffers` per backdrop per frame. See the
        // `BLUR_SCRATCH_FBO` definition for why this is the perf-critical
        // change for NVIDIA proprietary.
        let target_tex_id = framebuffer_texture.tex_id();
        frame.with_context(|gl| unsafe {
            with_gpu_timing_gl_span(gl, "backdrop-capture-blit", (size.w, size.h), || {
                while gl.GetError() != ffi::NO_ERROR {}

                let mut current_fbo = 0i32;
                gl.GetIntegerv(ffi::DRAW_FRAMEBUFFER_BINDING, &mut current_fbo as *mut _);
                let mut clear_color = [0.0f32; 4];
                gl.GetFloatv(ffi::COLOR_CLEAR_VALUE, clear_color.as_mut_ptr());
                gl.Disable(ffi::SCISSOR_TEST);

                // The blit must read from the framebuffer this frame is being
                // composited into. Never rely on the ambient READ binding:
                // effect pipelines and snapshot captures running earlier in
                // the same frame leave READ_FRAMEBUFFER pointing at their own
                // offscreen targets (or an older swapchain buffer), and a blit
                // from there silently captures a stale composite — including
                // this element itself and everything above it.
                let mut prev_read_fbo = 0i32;
                gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut prev_read_fbo as *mut _);
                if prev_read_fbo != current_fbo {
                    use std::sync::atomic::{AtomicUsize, Ordering};
                    static MISMATCH_LOG: AtomicUsize = AtomicUsize::new(0);
                    if MISMATCH_LOG
                        .fetch_add(1, Ordering::Relaxed)
                        .is_multiple_of(120)
                    {
                        tracing::warn!(
                            prev_read_fbo,
                            draw_fbo = current_fbo,
                            "backdrop capture: READ framebuffer was not the frame target; rebinding"
                        );
                    }
                }
                gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, current_fbo as u32);

                let fbo = ensure_blur_scratch_fbo(gl);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
                gl.FramebufferTexture2D(
                    ffi::DRAW_FRAMEBUFFER,
                    ffi::COLOR_ATTACHMENT0,
                    ffi::TEXTURE_2D,
                    target_tex_id,
                    0,
                );
                gl.Viewport(0, 0, size.w, size.h);
                gl.ClearColor(0.0, 0.0, 0.0, 0.0);
                gl.Clear(ffi::COLOR_BUFFER_BIT);
                gl.BlitFramebuffer(
                    actual_capture_rect.loc.x,
                    actual_capture_rect.loc.y,
                    actual_capture_rect.loc.x + actual_capture_rect.size.w,
                    actual_capture_rect.loc.y + actual_capture_rect.size.h,
                    0,
                    0,
                    actual_capture_rect.size.w,
                    actual_capture_rect.size.h,
                    ffi::COLOR_BUFFER_BIT,
                    ffi::LINEAR,
                );
                gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, prev_read_fbo as u32);
                gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, current_fbo as u32);
                gl.ClearColor(
                    clear_color[0],
                    clear_color[1],
                    clear_color[2],
                    clear_color[3],
                );
                gl.Enable(ffi::SCISSOR_TEST);
            });
        })?;

        let sample_src = inner.sample_src;
        let mut guard = frame.renderer();
        let renderer = guard.as_mut();
        let mut pipeline = self
            .pipeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pipeline = pipeline.begin_frame(renderer);
        let result = if let Some(popup_source) = self.popup_source.clone() {
            apply_effect_pipeline_cached_with_popup_source_and_finish_mode(
                renderer,
                framebuffer_texture,
                None,
                popup_source,
                (size.w, size.h),
                sample_src,
                Some((dst.size.w, dst.size.h)),
                &self.shader,
                pipeline,
                BackdropFinishMode::DeferToDisplay,
            )
        } else {
            apply_effect_pipeline_cached_with_finish_mode(
                renderer,
                framebuffer_texture,
                None,
                (size.w, size.h),
                sample_src,
                Some((dst.size.w, dst.size.h)),
                &self.shader,
                pipeline,
                BackdropFinishMode::DeferToDisplay,
            )
        };
        match result {
            Ok(texture) => {
                inner.sample_src = if texture.size() == size {
                    sample_src
                } else {
                    Some(Rectangle::from_size(texture.size().to_f64()))
                };
                inner.rendered = Some(texture);
            }
            Err(err) => {
                warn!(
                    ?err,
                    "failed to render backdrop framebuffer effect pipeline"
                );
            }
        }

        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

impl StableBackdropFramebufferElement {
    fn draw_framebuffer_regions(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        texture: &GlesTexture,
        sample_src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), GlesError> {
        let full_geometry = Rectangle::from_size(self.geometry.size);
        for region in &self.framebuffer_regions {
            let region_geometry =
                scale_physical_subrect(region.geometry, full_geometry.size, dst.size);
            let Some(region_damage) = relative_intersections(damage, region_geometry) else {
                continue;
            };
            let region_opaque =
                relative_intersections(opaque_regions, region_geometry).unwrap_or_default();
            let src = scale_buffer_subrect(sample_src, region.geometry, full_geometry.size);
            let full_size = texture.size();
            let uv_offset = [
                src.loc.x as f32 / full_size.w.max(1) as f32,
                src.loc.y as f32 / full_size.h.max(1) as f32,
            ];
            let uv_scale = [
                src.size.w as f32 / full_size.w.max(1) as f32,
                src.size.h as f32 / full_size.h.max(1) as f32,
            ];
            frame.render_texture_from_to(
                texture,
                src,
                Rectangle::new(dst.loc + region_geometry.loc, region_geometry.size),
                &region_damage,
                &region_opaque,
                Transform::Normal,
                self.alpha,
                Some(&self.program),
                &[
                    Uniform::new("uv_offset", uv_offset),
                    Uniform::new("uv_scale", uv_scale),
                    Uniform::new(
                        "rect_size",
                        [region.area.size.w as f32, region.area.size.h as f32],
                    ),
                    Uniform::new("render_scale", self.render_scale.max(1.0)),
                    Uniform::new("clip_enabled", 0.0f32),
                    Uniform::new("clip_rect", [0.0, 0.0, 0.0, 0.0]),
                    Uniform::new("clip_radius", [0.0, 0.0, 0.0, 0.0]),
                ],
            )?;
        }
        Ok(())
    }
}

pub(crate) fn framebuffer_capture_padding(effect: &CompiledEffect, render_scale: f32) -> i32 {
    ((effect.capture_padding.max(0) as f32) * render_scale.max(1.0)).ceil() as i32
}

fn scale_physical_subrect(
    rect: Rectangle<i32, Physical>,
    source_size: Size<i32, Physical>,
    target_size: Size<i32, Physical>,
) -> Rectangle<i32, Physical> {
    let scale_edge = |value: i32, source: i32, target: i32| {
        if source <= 0 {
            0
        } else {
            ((value as f64) * target as f64 / source as f64).round() as i32
        }
    };
    let left = scale_edge(rect.loc.x, source_size.w, target_size.w);
    let top = scale_edge(rect.loc.y, source_size.h, target_size.h);
    let right = scale_edge(rect.loc.x + rect.size.w, source_size.w, target_size.w);
    let bottom = scale_edge(rect.loc.y + rect.size.h, source_size.h, target_size.h);
    Rectangle::new(
        Point::from((left, top)),
        (right - left, bottom - top).into(),
    )
}

fn scale_buffer_subrect(
    source: Rectangle<f64, Buffer>,
    rect: Rectangle<i32, Physical>,
    full_size: Size<i32, Physical>,
) -> Rectangle<f64, Buffer> {
    let scale_edge = |value: i32, offset: f64, source_size: f64, target_size: i32| {
        if target_size <= 0 {
            offset
        } else {
            offset + value as f64 * source_size / target_size as f64
        }
    };
    let left = scale_edge(rect.loc.x, source.loc.x, source.size.w, full_size.w);
    let top = scale_edge(rect.loc.y, source.loc.y, source.size.h, full_size.h);
    let right = scale_edge(
        rect.loc.x + rect.size.w,
        source.loc.x,
        source.size.w,
        full_size.w,
    );
    let bottom = scale_edge(
        rect.loc.y + rect.size.h,
        source.loc.y,
        source.size.h,
        full_size.h,
    );
    Rectangle::new(
        Point::from((left, top)),
        (right - left, bottom - top).into(),
    )
}

fn relative_intersections(
    rects: &[Rectangle<i32, Physical>],
    region: Rectangle<i32, Physical>,
) -> Option<Vec<Rectangle<i32, Physical>>> {
    let intersections: Vec<_> = rects
        .iter()
        .filter_map(|rect| rect.intersection(region))
        .map(|mut rect| {
            rect.loc -= region.loc;
            rect
        })
        .collect();
    (!intersections.is_empty()).then_some(intersections)
}

impl Element for StableBackdropTextureElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.src
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let _ = scale;
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        self.kind
    }
}

impl RenderElement<GlesRenderer> for StableBackdropTextureElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let clip_rect = self
            .clip_rect
            .map(|clip| [clip.x, clip.y, clip.width, clip.height])
            .unwrap_or([0.0, 0.0, 0.0, 0.0]);
        let radius = self.clip_radius.max(0.0);

        let timing =
            begin_gpu_timing_frame_span(frame, "backdrop-display-draw", (dst.size.w, dst.size.h));
        let result = frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            opaque_regions,
            Transform::Normal,
            self.alpha,
            Some(&self.program),
            &[
                Uniform::new("uv_offset", self.uv_offset),
                Uniform::new("uv_scale", self.uv_scale),
                Uniform::new(
                    "rect_size",
                    [self.area.size.w as f32, self.area.size.h as f32],
                ),
                Uniform::new("render_scale", self.render_scale.max(1.0)),
                Uniform::new(
                    "clip_enabled",
                    if clip_rect[2] > 0.0 && clip_rect[3] > 0.0 {
                        1.0f32
                    } else {
                        0.0f32
                    },
                ),
                Uniform::new("clip_rect", clip_rect),
                Uniform::new("clip_radius", [radius, radius, radius, radius]),
            ],
        );
        end_gpu_timing_frame_span(frame, timing);
        result
    }
}

impl StableBackdropTextureElement {
    pub fn debug_label(&self) -> &str {
        &self.debug_label
    }
}

fn shader_uniform_type(
    value: &ShaderUniformValue,
) -> smithay::backend::renderer::gles::UniformType {
    match value {
        ShaderUniformValue::Float(_) | ShaderUniformValue::FloatArray(_) => {
            smithay::backend::renderer::gles::UniformType::_1f
        }
        ShaderUniformValue::Vec2(_) | ShaderUniformValue::Vec2Array(_) => {
            smithay::backend::renderer::gles::UniformType::_2f
        }
        ShaderUniformValue::Vec3(_) | ShaderUniformValue::Vec3Array(_) => {
            smithay::backend::renderer::gles::UniformType::_3f
        }
        ShaderUniformValue::Vec4(_) | ShaderUniformValue::Vec4Array(_) => {
            smithay::backend::renderer::gles::UniformType::_4f
        }
    }
}

fn append_shader_uniform_names(
    uniforms: &mut Vec<UniformName>,
    name: &str,
    value: &ShaderUniformValue,
) {
    let ty = shader_uniform_type(value);
    let array_len = match value {
        ShaderUniformValue::FloatArray(values) => Some(values.len()),
        ShaderUniformValue::Vec2Array(values) => Some(values.len()),
        ShaderUniformValue::Vec3Array(values) => Some(values.len()),
        ShaderUniformValue::Vec4Array(values) => Some(values.len()),
        _ => None,
    };
    if let Some(array_len) = array_len {
        uniforms
            .extend((0..array_len).map(|index| UniformName::new(format!("{name}[{index}]"), ty)));
    } else {
        uniforms.push(UniformName::new(name.to_owned(), ty));
    }
}

fn append_shader_uniform_values(
    uniforms: &mut Vec<Uniform<'static>>,
    name: &str,
    value: &ShaderUniformValue,
) {
    match value {
        ShaderUniformValue::Float(value) => {
            uniforms.push(Uniform::new(name.to_owned(), *value));
        }
        ShaderUniformValue::Vec2(value) => {
            uniforms.push(Uniform::new(name.to_owned(), *value));
        }
        ShaderUniformValue::Vec3(value) => {
            uniforms.push(Uniform::new(name.to_owned(), *value));
        }
        ShaderUniformValue::Vec4(value) => {
            uniforms.push(Uniform::new(name.to_owned(), *value));
        }
        ShaderUniformValue::FloatArray(values) => {
            uniforms.extend(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Uniform::new(format!("{name}[{index}]"), *value)),
            );
        }
        ShaderUniformValue::Vec2Array(values) => {
            uniforms.extend(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Uniform::new(format!("{name}[{index}]"), *value)),
            );
        }
        ShaderUniformValue::Vec3Array(values) => {
            uniforms.extend(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Uniform::new(format!("{name}[{index}]"), *value)),
            );
        }
        ShaderUniformValue::Vec4Array(values) => {
            uniforms.extend(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| Uniform::new(format!("{name}[{index}]"), *value)),
            );
        }
    }
}

fn compile_shader_program(
    renderer: &mut GlesRenderer,
    shader: &CompiledEffect,
) -> Result<GlesPixelProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(ShaderProgramCache::default);
    }

    let shader_module = shader
        .last_shader_stage()
        .expect("pixel shader effects should always have a final shader stage");
    let mut cache_key = format!("pixel:{}", shader_module.shader.path);
    for (name, value) in &shader_module.uniforms {
        let kind = value.shape_key();
        cache_key.push(':');
        cache_key.push_str(name);
        cache_key.push(':');
        cache_key.push_str(&kind);
    }
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .expect("shader effect cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source = fs::read_to_string(&shader_module.shader.path).map_err(|source| {
        ShaderEffectError::ReadShader {
            path: shader_module.shader.path.clone(),
            source,
        }
    })?;
    let mut uniform_names = vec![
        UniformName::new(
            "render_scale",
            smithay::backend::renderer::gles::UniformType::_1f,
        ),
        UniformName::new(
            "clip_enabled",
            smithay::backend::renderer::gles::UniformType::_1f,
        ),
        UniformName::new(
            "clip_rect",
            smithay::backend::renderer::gles::UniformType::_4f,
        ),
        UniformName::new(
            "clip_radius",
            smithay::backend::renderer::gles::UniformType::_4f,
        ),
    ];
    for (name, value) in &shader_module.uniforms {
        append_shader_uniform_names(&mut uniform_names, name, value);
    }
    let program =
        renderer.compile_custom_pixel_shader(wrap_pixel_shader_source(&source), &uniform_names)?;
    renderer
        .egl_context()
        .user_data()
        .get::<ShaderProgramCache>()
        .expect("shader effect cache should be initialized")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, program.clone());
    Ok(program)
}

pub fn compile_backdrop_shader_program(
    renderer: &mut GlesRenderer,
    shader: &ShaderModule,
) -> Result<GlesTexProgram, ShaderEffectError> {
    compile_texture_program(renderer, &shader.path, "display", true, None)
}

fn compile_display_texture_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_backdrop_shader_source(
                r#"
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    color.a = 1.0;
    return color;
}
"#,
            ),
            &[
                UniformName::new(
                    "uv_offset",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "uv_scale",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "rect_size",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "render_scale",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "clip_enabled",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "clip_rect",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "clip_radius",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| DisplayTextureProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgram>()
        .expect("display texture shader should be initialized")
        .0
        .clone())
}

// Same as compile_display_texture_program but keeps the texture's alpha
// channel. Used for effects whose pipeline intentionally produces transparent
// regions (e.g. layer-source masks).
fn compile_display_texture_program_preserve_alpha(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgramPreserveAlpha>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_backdrop_shader_source(
                r#"
vec4 shader_main(EffectContext effect) {
    return texture2D(tex, effect.texture_uv);
}
"#,
            ),
            &[
                UniformName::new(
                    "uv_offset",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "uv_scale",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "rect_size",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "render_scale",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "clip_enabled",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
                UniformName::new(
                    "clip_rect",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "clip_radius",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| DisplayTextureProgramPreserveAlpha(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<DisplayTextureProgramPreserveAlpha>()
        .expect("display texture shader should be initialized")
        .0
        .clone())
}

fn compile_noise_salt_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<NoiseSaltProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_texture_stage_source(include_str!("noise_salt.frag")),
            &[
                UniformName::new(
                    "effect_texture_size_px",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "effect_content_rect_px",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
                UniformName::new(
                    "noise_amount",
                    smithay::backend::renderer::gles::UniformType::_1f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| NoiseSaltProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<NoiseSaltProgram>()
        .expect("noise salt shader should be initialized")
        .0
        .clone())
}

fn compile_opaque_finish_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<OpaqueFinishProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_texture_stage_source(
                r#"
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    color.a = 1.0;
    return color;
}
"#,
            ),
            &[
                UniformName::new(
                    "effect_texture_size_px",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "effect_content_rect_px",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| OpaqueFinishProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<OpaqueFinishProgram>()
        .expect("opaque finish shader should be initialized")
        .0
        .clone())
}

// Same as compile_opaque_finish_program but keeps the texture's alpha.
// Used for effects whose pipeline intentionally produces transparent
// regions (e.g. layer-source masks).
fn compile_alpha_preserving_finish_program(
    renderer: &mut GlesRenderer,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<AlphaPreservingFinishProgram>()
        .is_none()
    {
        let program = renderer.compile_custom_texture_shader(
            wrap_texture_stage_source(
                r#"
vec4 shader_main(EffectContext effect) {
    return texture2D(tex, effect.texture_uv);
}
"#,
            ),
            &[
                UniformName::new(
                    "effect_texture_size_px",
                    smithay::backend::renderer::gles::UniformType::_2f,
                ),
                UniformName::new(
                    "effect_content_rect_px",
                    smithay::backend::renderer::gles::UniformType::_4f,
                ),
            ],
        )?;
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| AlphaPreservingFinishProgram(program));
    }

    Ok(renderer
        .egl_context()
        .user_data()
        .get::<AlphaPreservingFinishProgram>()
        .expect("alpha preserving finish shader should be initialized")
        .0
        .clone())
}

fn blend_shader_programs(renderer: &mut GlesRenderer) -> Result<BlendPrograms, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(BlendProgramCache::default);
    }

    if let Some(programs) = renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .expect("blend shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(programs);
    }

    let renderer_context_id = renderer.context_id();
    let programs = renderer.with_context(|gl| unsafe {
        let program = link_program(
            gl,
            include_str!("backdrop_blur.vert"),
            include_str!("blend_raw.frag"),
        )?;
        let vert = c"vert";
        let tex = c"tex";
        let tex2 = c"tex2";
        let blend_mode = c"blend_mode";
        let blend_alpha = c"blend_alpha";

        Ok::<_, GlesError>(BlendPrograms {
            program: BlendProgramInternal {
                program,
                uniform_tex: gl.GetUniformLocation(program, tex.as_ptr()),
                uniform_tex2: gl.GetUniformLocation(program, tex2.as_ptr()),
                uniform_blend_mode: gl.GetUniformLocation(program, blend_mode.as_ptr()),
                uniform_blend_alpha: gl.GetUniformLocation(program, blend_alpha.as_ptr()),
                attrib_vert: gl.GetAttribLocation(program, vert.as_ptr()),
            },
            renderer_context_id,
        })
    })??;

    *renderer
        .egl_context()
        .user_data()
        .get::<BlendProgramCache>()
        .expect("blend shader cache should be initialized")
        .0
        .lock()
        .unwrap() = Some(programs.clone());

    Ok(programs)
}

fn compile_texture_stage_program(
    renderer: &mut GlesRenderer,
    stage: &ShaderStage,
) -> Result<GlesTexProgram, ShaderEffectError> {
    compile_texture_program(
        renderer,
        &stage.shader.path,
        "stage",
        false,
        Some(&stage.uniforms),
    )
}

fn wrap_multi_texture_stage_source(source: &str) -> String {
    format!(
        r#"#version 100

precision highp float;

uniform sampler2D tex;
uniform vec2 effect_texture_size_px;
uniform vec4 effect_content_rect_px;

varying vec2 v_coords;

{effect_context}

{source}

void main() {{
    EffectContext effect = make_effect_context(
        v_coords,
        effect_texture_size_px,
        effect_content_rect_px
    );
    gl_FragColor = shader_main(effect);
}}
"#,
        effect_context = effect_context_shader_prelude(),
    )
}

fn multi_texture_stage_program(
    renderer: &mut GlesRenderer,
    stage: &ShaderStage,
) -> Result<MultiTextureStageProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<MultiTextureStageProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(MultiTextureStageProgramCache::default);
    }
    let mut cache_key = format!("effect-context-v1:multi-texture:{}", stage.shader.path);
    for name in stage.textures.keys() {
        cache_key.push_str(":texture:");
        cache_key.push_str(name);
    }
    for (name, value) in &stage.uniforms {
        let kind = value.shape_key();
        cache_key.push_str(":uniform:");
        cache_key.push_str(name);
        cache_key.push(':');
        cache_key.push_str(&kind);
    }
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<MultiTextureStageProgramCache>()
        .expect("multi texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source =
        fs::read_to_string(&stage.shader.path).map_err(|source| ShaderEffectError::ReadShader {
            path: stage.shader.path.clone(),
            source,
        })?;
    let wrapped = wrap_multi_texture_stage_source(&source);
    let renderer_context_id = renderer.context_id();
    let program = renderer.with_context(|gl| unsafe {
        let program = link_program(gl, include_str!("backdrop_blur.vert"), &wrapped)?;
        let location = |name: &str| {
            CString::new(name)
                .ok()
                .map(|name| gl.GetUniformLocation(program, name.as_ptr()))
                .unwrap_or(-1)
        };
        Ok::<_, GlesError>(MultiTextureStageProgram {
            program,
            uniform_tex: location("tex"),
            uniform_texture_size: location("effect_texture_size_px"),
            uniform_content_rect: location("effect_content_rect_px"),
            texture_uniforms: stage
                .textures
                .keys()
                .map(|name| (name.clone(), location(name)))
                .collect(),
            value_uniforms: stage
                .uniforms
                .iter()
                .map(|(name, value)| {
                    let location_name = match value {
                        ShaderUniformValue::FloatArray(_)
                        | ShaderUniformValue::Vec2Array(_)
                        | ShaderUniformValue::Vec3Array(_)
                        | ShaderUniformValue::Vec4Array(_) => format!("{name}[0]"),
                        _ => name.clone(),
                    };
                    (name.clone(), location(&location_name))
                })
                .collect(),
            attrib_vert: gl.GetAttribLocation(program, c"vert".as_ptr()),
            renderer_context_id,
        })
    })??;
    renderer
        .egl_context()
        .user_data()
        .get::<MultiTextureStageProgramCache>()
        .expect("multi texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, program.clone());
    Ok(program)
}

fn compile_texture_program(
    renderer: &mut GlesRenderer,
    path: &str,
    namespace: &str,
    with_clip: bool,
    uniforms: Option<&std::collections::BTreeMap<String, ShaderUniformValue>>,
) -> Result<GlesTexProgram, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(TextureStageProgramCache::default);
    }

    let mut cache_key = format!("effect-context-v1:{namespace}:{path}:{with_clip}");
    if let Some(uniforms) = uniforms {
        for (name, value) in uniforms {
            let kind = value.shape_key();
            cache_key.push(':');
            cache_key.push_str(name);
            cache_key.push(':');
            cache_key.push_str(&kind);
        }
    }
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .expect("texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(program);
    }

    let source = fs::read_to_string(path).map_err(|source| ShaderEffectError::ReadShader {
        path: path.to_string(),
        source,
    })?;
    let wrapped = if with_clip {
        wrap_backdrop_shader_source(&source)
    } else {
        wrap_texture_stage_source(&source)
    };
    let mut uniform_names = if with_clip {
        vec![
            UniformName::new(
                "uv_offset",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "uv_scale",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "rect_size",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "render_scale",
                smithay::backend::renderer::gles::UniformType::_1f,
            ),
            UniformName::new(
                "clip_enabled",
                smithay::backend::renderer::gles::UniformType::_1f,
            ),
            UniformName::new(
                "clip_rect",
                smithay::backend::renderer::gles::UniformType::_4f,
            ),
            UniformName::new(
                "clip_radius",
                smithay::backend::renderer::gles::UniformType::_4f,
            ),
        ]
    } else {
        vec![
            UniformName::new(
                "effect_texture_size_px",
                smithay::backend::renderer::gles::UniformType::_2f,
            ),
            UniformName::new(
                "effect_content_rect_px",
                smithay::backend::renderer::gles::UniformType::_4f,
            ),
        ]
    };
    if let Some(uniforms) = uniforms {
        for (name, value) in uniforms {
            append_shader_uniform_names(&mut uniform_names, name, value);
        }
    }
    let program = renderer.compile_custom_texture_shader(wrapped, &uniform_names)?;
    renderer
        .egl_context()
        .user_data()
        .get::<TextureStageProgramCache>()
        .expect("texture stage cache should be initialized")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, program.clone());
    Ok(program)
}

fn blur_shader_programs(
    renderer: &mut GlesRenderer,
) -> Result<BlurShaderPrograms, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(BlurShaderProgramCache::default);
    }

    if let Some(programs) = renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(programs);
    }

    let renderer_context_id = renderer.context_id();
    let programs = renderer.with_context(|gl| unsafe {
        let down = compile_blur_program(gl, include_str!("backdrop_blur_down.frag"))?;
        let up = compile_blur_program(gl, include_str!("backdrop_blur_up.frag"))?;
        Ok::<_, GlesError>(BlurShaderPrograms {
            down,
            up,
            renderer_context_id,
        })
    })??;
    *renderer
        .egl_context()
        .user_data()
        .get::<BlurShaderProgramCache>()
        .expect("blur shader cache should be initialized")
        .0
        .lock()
        .unwrap() = Some(programs.clone());
    Ok(programs)
}

unsafe fn compile_blur_program(
    gl: &ffi::Gles2,
    src: &str,
) -> Result<BlurProgramInternal, GlesError> {
    let program = unsafe { link_program(gl, include_str!("backdrop_blur.vert"), src)? };

    let vert = c"vert";
    let tex = c"tex";
    let half_pixel = c"half_pixel";
    let offset = c"offset";

    Ok(BlurProgramInternal {
        program,
        uniform_tex: unsafe { gl.GetUniformLocation(program, tex.as_ptr()) },
        uniform_half_pixel: unsafe { gl.GetUniformLocation(program, half_pixel.as_ptr()) },
        uniform_offset: unsafe { gl.GetUniformLocation(program, offset.as_ptr()) },
        attrib_vert: unsafe { gl.GetAttribLocation(program, vert.as_ptr()) },
    })
}

fn wrap_pixel_shader_source(source: &str) -> String {
    format!(
        r#"
precision highp float;

uniform float alpha;
uniform vec2 size;
uniform float render_scale;
uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

{effect_context}

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {{
    vec2 half_size = rect_size * 0.5;
    vec2 p = coords - half_size;
    float r;
    if (p.x >= 0.0) {{
        r = p.y >= 0.0 ? radius.z : radius.y;
    }} else {{
        r = p.y >= 0.0 ? radius.w : radius.x;
    }}
    vec2 q = abs(p) - (half_size - vec2(r));
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
    float half_px = 0.5 / max(render_scale, 1.0);
    return 1.0 - smoothstep(-half_px, half_px, dist);
}}

{source}

void main() {{
    vec2 coords = v_coords * size;
    EffectContext effect = make_effect_context(v_coords, size, vec4(0.0, 0.0, size));
    vec4 color = shader_main(effect);
    color.a *= alpha;
    color.rgb *= color.a;
    if (clip_enabled > 0.5) {{
        vec2 clip_coords = coords - clip_rect.xy;
        color *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }}
    gl_FragColor = color;
}}
"#,
        effect_context = effect_context_shader_prelude(),
    )
}

fn wrap_backdrop_shader_source(source: &str) -> String {
    format!(
        r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Unconditional highp: with the GL_FRAGMENT_PRECISION_HIGH fallback the
// NVIDIA GLES driver ended up evaluating these shaders in fp16, whose ulp
// is 2 at coordinates ~2454 — uv * rect_size quantized to even pixels and
// produced visible seams at effect edges. Smithay's own texture.frag also
// declares highp unconditionally, so every supported driver accepts this.
precision highp float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform vec2 uv_offset;
uniform vec2 uv_scale;
uniform vec2 rect_size;
uniform float render_scale;
uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

{effect_context}

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {{
    vec2 half_size = rect_size * 0.5;
    vec2 p = coords - half_size;
    float r;
    if (p.x >= 0.0) {{
        r = p.y >= 0.0 ? radius.z : radius.y;
    }} else {{
        r = p.y >= 0.0 ? radius.w : radius.x;
    }}
    vec2 q = abs(p) - (half_size - vec2(r));
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
    float half_px = 0.5 / max(render_scale, 1.0);
    return 1.0 - smoothstep(-half_px, half_px, dist);
}}

{source}

void main() {{
    EffectContext effect = make_effect_context(
        v_coords,
        rect_size,
        vec4(0.0, 0.0, rect_size)
    );
    vec4 color = shader_main(effect);
    color.a *= alpha;
    color.rgb *= color.a;

    if (clip_enabled > 0.5) {{
        vec2 coords = v_coords * rect_size;
        vec2 clip_coords = coords - clip_rect.xy;
        color *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }}

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}}
"#,
        effect_context = effect_context_shader_prelude(),
    )
}

fn effect_context_shader_prelude() -> &'static str {
    r#"
struct EffectContext {
    vec2 texture_uv;
    vec2 texture_size_px;
    vec4 content_rect_px;
};

EffectContext make_effect_context(
    vec2 texture_uv,
    vec2 texture_size_px,
    vec4 content_rect_px
) {
    return EffectContext(texture_uv, texture_size_px, content_rect_px);
}

vec2 effect_texture_px(EffectContext effect) {
    return effect.texture_uv * effect.texture_size_px;
}

vec2 effect_content_px(EffectContext effect) {
    return effect_texture_px(effect) - effect.content_rect_px.xy;
}

vec2 effect_content_uv(EffectContext effect) {
    return effect_content_px(effect) / max(effect.content_rect_px.zw, vec2(1.0));
}

vec2 effect_texture_uv_from_content_px(EffectContext effect, vec2 content_px) {
    return (effect.content_rect_px.xy + content_px) /
        max(effect.texture_size_px, vec2(1.0));
}
"#
}

fn wrap_texture_stage_source(source: &str) -> String {
    format!(
        r#"
//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Unconditional highp — see wrap_backdrop_shader_source for the rationale
// (fp16 fallback quantized uv * rect_size to even pixels on NVIDIA).
precision highp float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform vec2 effect_texture_size_px;
uniform vec4 effect_content_rect_px;

varying vec2 v_coords;

{effect_context}

{source}

void main() {{
    EffectContext effect = make_effect_context(
        v_coords,
        effect_texture_size_px,
        effect_content_rect_px
    );
    gl_FragColor = shader_main(effect);
}}
"#,
        effect_context = effect_context_shader_prelude(),
    )
}

fn uniforms_for_spec(spec: &ShaderEffectSpec) -> Vec<Uniform<'static>> {
    let clip_rect = spec
        .clip_rect
        .map(|rect| [rect.x, rect.y, rect.width, rect.height])
        .unwrap_or([0.0f32, 0.0f32, 0.0f32, 0.0f32]);
    let clip_radius = spec.clip_radius.max(0.0);
    let mut uniforms = vec![
        Uniform::new("render_scale", spec.render_scale.max(1.0)),
        Uniform::new(
            "clip_enabled",
            if spec.clip_rect.is_some() {
                1.0f32
            } else {
                0.0f32
            },
        ),
        Uniform::new("clip_rect", clip_rect),
        Uniform::new(
            "clip_radius",
            [clip_radius, clip_radius, clip_radius, clip_radius],
        ),
    ];
    if let Some(stage) = spec.shader.last_shader_stage() {
        uniforms.extend(uniforms_for_shader_stage(stage));
    }
    uniforms
}

fn uniforms_for_shader_stage(stage: &ShaderStage) -> Vec<Uniform<'static>> {
    let mut uniforms = Vec::new();
    for (name, value) in &stage.uniforms {
        append_shader_uniform_values(&mut uniforms, name, value);
    }
    uniforms
}

pub fn backdrop_shader_element(
    renderer: &mut GlesRenderer,
    element_id: Id,
    commit_counter: CommitCounter,
    texture: GlesTexture,
    display_rect: Rectangle<i32, Logical>,
    sample_rect: Rectangle<i32, Logical>,
    captured_rect: Rectangle<i32, Logical>,
    _shader: &CompiledEffect,
    alpha: f32,
    render_scale: f32,
    clip_rect: Option<SnappedLogicalRect>,
    clip_radius: f32,
    debug_label: String,
) -> Result<StableBackdropTextureElement, ShaderEffectError> {
    backdrop_shader_element_with_geometry(
        renderer,
        element_id,
        commit_counter,
        texture,
        display_rect,
        crate::backend::visual::logical_rect_to_physical_rect(
            crate::ssd::LogicalRect::new(
                display_rect.loc.x,
                display_rect.loc.y,
                display_rect.size.w,
                display_rect.size.h,
            ),
            Point::from((0, 0)),
            Scale::from((render_scale as f64, render_scale as f64)),
        ),
        sample_rect,
        captured_rect,
        _shader,
        alpha,
        render_scale,
        [0.0, 0.0],
        clip_rect,
        clip_radius,
        debug_label,
    )
}

pub fn backdrop_shader_element_with_geometry(
    renderer: &mut GlesRenderer,
    element_id: Id,
    commit_counter: CommitCounter,
    texture: GlesTexture,
    display_rect: Rectangle<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    sample_rect: Rectangle<i32, Logical>,
    captured_rect: Rectangle<i32, Logical>,
    shader: &CompiledEffect,
    alpha: f32,
    render_scale: f32,
    sample_uv_phase: [f32; 2],
    clip_rect: Option<SnappedLogicalRect>,
    clip_radius: f32,
    debug_label: String,
) -> Result<StableBackdropTextureElement, ShaderEffectError> {
    // Plain backdrop blur is fully opaque, so the default display program
    // forces alpha to 1.0 to hide capture/blur alpha noise at the edges (see
    // EffectAlphaMode). Effects that declare `alpha: "preserve"` keep the
    // pipeline's alpha intact — otherwise masked-out areas would show up as
    // opaque black. The mode is an explicit opt-in from the config, never
    // inferred from the pipeline contents.
    let program = match shader.alpha {
        crate::ssd::EffectAlphaMode::Preserve => {
            compile_display_texture_program_preserve_alpha(renderer)?
        }
        crate::ssd::EffectAlphaMode::Opaque => compile_display_texture_program(renderer)?,
    };
    let texture_size = texture.size();
    let captured_width_px = texture_size.w.max(1);
    let captured_height_px = texture_size.h.max(1);
    let logical_to_texture_px = |value: f64, logical_size: i32, texture_size: i32| -> i32 {
        if logical_size <= 0 {
            return 0;
        }
        (value * texture_size as f64 / logical_size as f64).round() as i32
    };
    let sample_left_px = logical_to_texture_px(
        (sample_rect.loc.x - captured_rect.loc.x) as f64,
        captured_rect.size.w,
        captured_width_px,
    )
    .clamp(0, captured_width_px);
    let sample_top_px = logical_to_texture_px(
        (sample_rect.loc.y - captured_rect.loc.y) as f64,
        captured_rect.size.h,
        captured_height_px,
    )
    .clamp(0, captured_height_px);
    let sample_right_px = logical_to_texture_px(
        (sample_rect.loc.x + sample_rect.size.w - captured_rect.loc.x) as f64,
        captured_rect.size.w,
        captured_width_px,
    )
    .clamp(0, captured_width_px);
    let sample_bottom_px = logical_to_texture_px(
        (sample_rect.loc.y + sample_rect.size.h - captured_rect.loc.y) as f64,
        captured_rect.size.h,
        captured_height_px,
    )
    .clamp(0, captured_height_px);
    let sample_width_px = (sample_right_px - sample_left_px).max(0);
    let sample_height_px = (sample_bottom_px - sample_top_px).max(0);
    let src = Rectangle::new(
        smithay::utils::Point::from((sample_left_px as f64, sample_top_px as f64)),
        (sample_width_px as f64, sample_height_px as f64).into(),
    );
    let uv_offset = [
        (sample_left_px as f32 + sample_uv_phase[0]) / captured_width_px.max(1) as f32,
        (sample_top_px as f32 + sample_uv_phase[1]) / captured_height_px.max(1) as f32,
    ];
    let uv_scale = [
        sample_width_px as f32 / captured_width_px.max(1) as f32,
        sample_height_px as f32 / captured_height_px.max(1) as f32,
    ];
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        tracing::info!(
            debug_label = %debug_label,
            texture_size = ?texture_size,
            display_rect = ?display_rect,
            geometry = ?geometry,
            sample_rect = ?sample_rect,
            captured_rect = ?captured_rect,
            sample_px = ?(sample_left_px, sample_top_px, sample_width_px, sample_height_px),
            captured_px = ?(captured_width_px, captured_height_px),
            src = ?src,
            uv_offset = ?uv_offset,
            uv_scale = ?uv_scale,
            sample_uv_phase = ?sample_uv_phase,
            render_scale,
            clip_rect = ?clip_rect,
            clip_radius = clip_radius.max(0.0),
            "gap debug backdrop texture element params"
        );
    }
    Ok(StableBackdropTextureElement {
        texture,
        program,
        id: element_id,
        commit_counter,
        area: display_rect,
        geometry,
        src,
        alpha: alpha.clamp(0.0, 1.0),
        render_scale,
        clip_rect,
        clip_radius: clip_radius.max(0.0),
        uv_offset,
        uv_scale,
        debug_label,
        kind: Kind::Unspecified,
    })
}

pub fn apply_effect_pipeline(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    apply_effect_pipeline_with_cache(
        renderer,
        texture,
        xray_texture,
        size,
        sample_region,
        output_size,
        effect,
        None,
        BackdropFinishMode::Materialize,
    )
}

pub fn apply_effect_pipeline_cached_for_key(
    renderer: &mut GlesRenderer,
    cache_key: String,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    SHARED_EFFECT_PIPELINE_CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        let cache = caches.pipeline(renderer, cache_key);
        apply_effect_pipeline_cached(
            renderer,
            texture,
            xray_texture,
            size,
            sample_region,
            output_size,
            effect,
            cache,
        )
    })
}

fn apply_effect_pipeline_cached(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
    cache: &mut EffectPipelineCache,
) -> Result<GlesTexture, ShaderEffectError> {
    apply_effect_pipeline_cached_with_finish_mode(
        renderer,
        texture,
        xray_texture,
        size,
        sample_region,
        output_size,
        effect,
        cache,
        BackdropFinishMode::Materialize,
    )
}

fn apply_effect_pipeline_cached_with_finish_mode(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
    cache: &mut EffectPipelineCache,
    finish_mode: BackdropFinishMode,
) -> Result<GlesTexture, ShaderEffectError> {
    apply_effect_pipeline_with_cache(
        renderer,
        texture,
        xray_texture,
        size,
        sample_region,
        output_size,
        effect,
        Some(cache),
        finish_mode,
    )
}

fn apply_effect_pipeline_with_cache(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
    cache: Option<&mut EffectPipelineCache>,
    finish_mode: BackdropFinishMode,
) -> Result<GlesTexture, ShaderEffectError> {
    timescope::scope!("effect pipeline");
    let content_rect = effect_content_rect(size, sample_region);
    let mut ctx = EffectExecutionContext {
        backdrop: texture,
        xray_backdrop: xray_texture,
        layer_source: None,
        popup_source: None,
        size,
        state_base_size: size,
        content_rect,
        named: HashMap::new(),
    };
    with_gpu_timing_renderer_span(renderer, "effect-pipeline-total", size, |renderer| {
        run_effect_pipeline(
            renderer,
            effect,
            &mut ctx,
            sample_region,
            output_size,
            cache,
            finish_mode,
        )
    })
}

/// Variant for the capture-based `replace` / `inFront` / `behindRootSurface`
/// slots: the captured subject (window, layer, or popup) is the pipeline
/// input, so it is exposed through every subject alias. Only the alias
/// matching the slot's input kind is required by the slot gate; the others
/// are populated so `layerSource()` / `popupSource()` also resolve as extra
/// shader textures inside these slots.
pub fn apply_effect_pipeline_cached_for_key_with_captured_subject(
    renderer: &mut GlesRenderer,
    cache_key: String,
    subject: GlesTexture,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    SHARED_EFFECT_PIPELINE_CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        let cache = caches.pipeline(renderer, cache_key);
        timescope::scope!("effect pipeline");
        let mut ctx = EffectExecutionContext {
            backdrop: subject.clone(),
            xray_backdrop: None,
            layer_source: Some(subject.clone()),
            popup_source: Some(subject),
            size,
            state_base_size: size,
            content_rect: effect_content_rect(size, sample_region),
            named: HashMap::new(),
        };
        with_gpu_timing_renderer_span(renderer, "effect-pipeline-total", size, |renderer| {
            run_effect_pipeline(
                renderer,
                effect,
                &mut ctx,
                sample_region,
                output_size,
                Some(cache),
                BackdropFinishMode::Materialize,
            )
        })
    })
}

pub fn apply_effect_pipeline_cached_for_key_with_layer_source(
    renderer: &mut GlesRenderer,
    cache_key: String,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    layer_source: GlesTexture,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    SHARED_EFFECT_PIPELINE_CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        let cache = caches.pipeline(renderer, cache_key);
        let mut ctx = EffectExecutionContext {
            backdrop: texture,
            xray_backdrop: xray_texture,
            layer_source: Some(layer_source),
            popup_source: None,
            size,
            state_base_size: size,
            content_rect: effect_content_rect(size, sample_region),
            named: HashMap::new(),
        };
        run_effect_pipeline(
            renderer,
            effect,
            &mut ctx,
            sample_region,
            output_size,
            Some(cache),
            BackdropFinishMode::Materialize,
        )
    })
}

pub fn apply_effect_pipeline_cached_for_key_with_popup_source(
    renderer: &mut GlesRenderer,
    cache_key: String,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    popup_source: GlesTexture,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
) -> Result<GlesTexture, ShaderEffectError> {
    SHARED_EFFECT_PIPELINE_CACHES.with(|caches| {
        let mut caches = caches.borrow_mut();
        let cache = caches.pipeline(renderer, cache_key);
        apply_effect_pipeline_cached_with_popup_source_and_finish_mode(
            renderer,
            texture,
            xray_texture,
            popup_source,
            size,
            sample_region,
            output_size,
            effect,
            cache,
            BackdropFinishMode::Materialize,
        )
    })
}

fn apply_effect_pipeline_cached_with_popup_source_and_finish_mode(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    xray_texture: Option<GlesTexture>,
    popup_source: GlesTexture,
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    effect: &CompiledEffect,
    cache: &mut EffectPipelineCache,
    finish_mode: BackdropFinishMode,
) -> Result<GlesTexture, ShaderEffectError> {
    let mut ctx = EffectExecutionContext {
        backdrop: texture,
        xray_backdrop: xray_texture,
        layer_source: None,
        popup_source: Some(popup_source),
        size,
        state_base_size: size,
        content_rect: effect_content_rect(size, sample_region),
        named: HashMap::new(),
    };
    run_effect_pipeline(
        renderer,
        effect,
        &mut ctx,
        sample_region,
        output_size,
        Some(cache),
        finish_mode,
    )
}

pub fn log_gap_texture_region_readback(
    renderer: &mut GlesRenderer,
    texture: &GlesTexture,
    src_region: Option<Rectangle<f64, Buffer>>,
    output_size: (i32, i32),
    subject: &str,
    label: &str,
    output_name: &str,
    window_id: &str,
) {
    if output_size.0 <= 0 || output_size.1 <= 0 {
        return;
    }

    let Ok(mut target) =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, output_size.into())
    else {
        return;
    };
    let element = TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        Point::<f64, Physical>::from((0.0, 0.0)),
        texture.clone(),
        1,
        Transform::Normal,
        Some(1.0),
        src_region.map(|region| {
            Rectangle::new(
                Point::from((region.loc.x, region.loc.y)),
                (region.size.w, region.size.h).into(),
            )
        }),
        Some(output_size.into()),
        None,
        Kind::Unspecified,
    );
    let Ok(mut framebuffer) = renderer.bind(&mut target) else {
        return;
    };
    let mut damage_tracker = OutputDamageTracker::new(output_size, 1.0, Transform::Normal);
    let Ok(_) = damage_tracker.render_output(
        renderer,
        &mut framebuffer,
        0,
        &[element],
        [0.0, 0.0, 0.0, 0.0],
    ) else {
        return;
    };

    let output_rect = Rectangle::from_size(Size::<i32, Buffer>::from(output_size));
    let Ok(mapping) = renderer.copy_framebuffer(&framebuffer, output_rect, Fourcc::Abgr8888) else {
        return;
    };
    let Ok(bytes) = renderer.map_texture(&mapping) else {
        return;
    };
    drop(framebuffer);

    let width = output_size.0 as usize;
    let height = output_size.1 as usize;
    if width == 0 || height == 0 {
        return;
    }

    let mut left_gap_px = 0usize;
    while left_gap_px < width && column_is_fully_transparent(bytes, width, height, left_gap_px) {
        left_gap_px += 1;
    }

    let mut right_gap_px = 0usize;
    while right_gap_px < width
        && column_is_fully_transparent(bytes, width, height, width - 1 - right_gap_px)
    {
        right_gap_px += 1;
    }

    let mut top_gap_px = 0usize;
    while top_gap_px < height && row_is_fully_transparent(bytes, width, top_gap_px) {
        top_gap_px += 1;
    }

    let mut bottom_gap_px = 0usize;
    while bottom_gap_px < height
        && row_is_fully_transparent(bytes, width, height - 1 - bottom_gap_px)
    {
        bottom_gap_px += 1;
    }

    let nonzero_bounds = first_last_nonzero_alpha(bytes, width, height);
    let first_nonzero = nonzero_bounds.map(|(x, y, _, _)| (x as i32, y as i32));
    let last_nonzero = nonzero_bounds.map(|(_, _, x, y)| (x as i32, y as i32));
    let left_columns = summarize_edge_columns(bytes, width, height, false);
    let right_columns = summarize_edge_columns(bytes, width, height, true);
    let top_rows = summarize_edge_rows(bytes, width, height, false);
    let bottom_rows = summarize_edge_rows(bytes, width, height, true);

    tracing::info!(
        output = output_name,
        window_id,
        subject,
        label,
        src_region = ?src_region,
        output_size = ?output_size,
        first_nonzero = ?first_nonzero,
        last_nonzero = ?last_nonzero,
        left_gap_px,
        right_gap_px,
        top_gap_px,
        bottom_gap_px,
        left_columns = ?left_columns,
        right_columns = ?right_columns,
        top_rows = ?top_rows,
        bottom_rows = ?bottom_rows,
        "gap readback shader texture summary"
    );
}

pub fn invalidation_sample_rect(
    effect: &CompiledEffect,
    visible_rect: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    invalidation_sample_rect_for_policy(&effect.invalidate, visible_rect)
}

pub fn source_damage_intersects_rect(
    effect: &CompiledEffect,
    visible_rect: Rectangle<i32, Logical>,
    source_damage: &[crate::state::OwnedDamageRect],
) -> bool {
    source_damage_intersects_policy(&effect.invalidate, visible_rect, source_damage)
}

fn invalidation_sample_rect_for_policy(
    policy: &EffectInvalidationPolicy,
    visible_rect: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    match policy {
        EffectInvalidationPolicy::OnSourceDamageBox { damage_padding } => {
            let margin = (*damage_padding).max(0);
            Rectangle::new(
                Point::from((visible_rect.loc.x - margin, visible_rect.loc.y - margin)),
                (
                    visible_rect.size.w.saturating_add(margin.saturating_mul(2)),
                    visible_rect.size.h.saturating_add(margin.saturating_mul(2)),
                )
                    .into(),
            )
        }
        EffectInvalidationPolicy::Always => visible_rect,
        EffectInvalidationPolicy::Manual { base, .. } => base
            .as_deref()
            .map(|policy| invalidation_sample_rect_for_policy(policy, visible_rect))
            .unwrap_or(visible_rect),
    }
}

fn column_is_fully_transparent(bytes: &[u8], width: usize, height: usize, x: usize) -> bool {
    (0..height).all(|y| alpha_at(bytes, width, x, y) == 0)
}

fn row_is_fully_transparent(bytes: &[u8], width: usize, y: usize) -> bool {
    (0..width).all(|x| alpha_at(bytes, width, x, y) == 0)
}

fn alpha_at(bytes: &[u8], width: usize, x: usize, y: usize) -> u8 {
    let idx = (y * width + x) * 4 + 3;
    bytes.get(idx).copied().unwrap_or(0)
}

fn first_last_nonzero_alpha(
    bytes: &[u8],
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let mut first = None;
    let mut last = None;
    for y in 0..height {
        for x in 0..width {
            if alpha_at(bytes, width, x, y) != 0 {
                first.get_or_insert((x, y));
                last = Some((x, y));
            }
        }
    }
    first.zip(last).map(|((fx, fy), (lx, ly))| (fx, fy, lx, ly))
}

fn summarize_edge_columns(
    bytes: &[u8],
    width: usize,
    height: usize,
    from_right: bool,
) -> Vec<String> {
    let sample_count = width.min(4);
    (0..sample_count)
        .map(|offset| {
            let x = if from_right {
                width - 1 - offset
            } else {
                offset
            };
            summarize_column(bytes, width, height, x, offset)
        })
        .collect()
}

fn summarize_edge_rows(
    bytes: &[u8],
    width: usize,
    height: usize,
    from_bottom: bool,
) -> Vec<String> {
    let sample_count = height.min(4);
    (0..sample_count)
        .map(|offset| {
            let y = if from_bottom {
                height - 1 - offset
            } else {
                offset
            };
            summarize_row(bytes, width, y, offset)
        })
        .collect()
}

fn summarize_column(bytes: &[u8], width: usize, height: usize, x: usize, offset: usize) -> String {
    let mut transparent = 0usize;
    let mut min_alpha = u8::MAX;
    let mut max_alpha = 0u8;
    for y in 0..height {
        let alpha = alpha_at(bytes, width, x, y);
        if alpha == 0 {
            transparent += 1;
        }
        min_alpha = min_alpha.min(alpha);
        max_alpha = max_alpha.max(alpha);
    }
    format!(
        "offset={offset},x={x},transparent={transparent}/{height},min_alpha={min_alpha},max_alpha={max_alpha}"
    )
}

fn summarize_row(bytes: &[u8], width: usize, y: usize, offset: usize) -> String {
    let mut transparent = 0usize;
    let mut min_alpha = u8::MAX;
    let mut max_alpha = 0u8;
    for x in 0..width {
        let alpha = alpha_at(bytes, width, x, y);
        if alpha == 0 {
            transparent += 1;
        }
        min_alpha = min_alpha.min(alpha);
        max_alpha = max_alpha.max(alpha);
    }
    format!(
        "offset={offset},y={y},transparent={transparent}/{width},min_alpha={min_alpha},max_alpha={max_alpha}"
    )
}

fn source_damage_intersects_policy(
    policy: &EffectInvalidationPolicy,
    visible_rect: Rectangle<i32, Logical>,
    source_damage: &[crate::state::OwnedDamageRect],
) -> bool {
    match policy {
        EffectInvalidationPolicy::Always => true,
        EffectInvalidationPolicy::OnSourceDamageBox { .. } => {
            let sample_rect = invalidation_sample_rect_for_policy(policy, visible_rect);
            source_damage.iter().any(|damage| {
                let sample_right = sample_rect.loc.x.saturating_add(sample_rect.size.w);
                let sample_bottom = sample_rect.loc.y.saturating_add(sample_rect.size.h);
                let damage_right = damage.rect.x.saturating_add(damage.rect.width);
                let damage_bottom = damage.rect.y.saturating_add(damage.rect.height);
                let left = sample_rect.loc.x.max(damage.rect.x);
                let top = sample_rect.loc.y.max(damage.rect.y);
                let right = sample_right.min(damage_right);
                let bottom = sample_bottom.min(damage_bottom);
                right > left && bottom > top
            })
        }
        EffectInvalidationPolicy::Manual { dirty_when, base } => {
            *dirty_when
                || base.as_deref().is_some_and(|policy| {
                    source_damage_intersects_policy(policy, visible_rect, source_damage)
                })
        }
    }
}

fn run_effect_pipeline(
    renderer: &mut GlesRenderer,
    effect: &CompiledEffect,
    ctx: &mut EffectExecutionContext,
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
    mut cache: Option<&mut EffectPipelineCache>,
    finish_mode: BackdropFinishMode,
) -> Result<GlesTexture, ShaderEffectError> {
    timescope::scope!("effect pipeline stages");
    let requested_output_size = requested_effect_output_size(sample_region, output_size);
    let input_uses_requested_size = effect_input_renders_directly_to_requested_size(&effect.input);
    let initial_input_size = if input_uses_requested_size {
        requested_output_size.unwrap_or(ctx.size)
    } else {
        ctx.size
    };
    let mut current = resolve_effect_input(
        renderer,
        &effect.input,
        ctx,
        initial_input_size,
        cache.as_deref_mut(),
    )?;
    let current_size = initial_input_size;
    let final_sample_region = if input_uses_requested_size {
        None
    } else {
        sample_region
    };

    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        tracing::info!(
            effect_input = ?effect.input,
            ctx_size = ?ctx.size,
            requested_output_size = ?requested_output_size,
            initial_input_size = ?initial_input_size,
            input_uses_requested_size,
            final_sample_region = ?final_sample_region,
            "gap debug shader effect pipeline sizing"
        );
    }

    // Rate-limit per input size so cheap always-running pipelines (layer
    // bars) do not starve the rarely-invalidated window pipelines out of the
    // dump budget.
    let stage_readback = std::env::var_os("SHOJI_GAP_STAGE_READBACK").is_some() && {
        use std::collections::HashMap;
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static LAST_DUMP: Mutex<Option<HashMap<(i32, i32), Instant>>> = Mutex::new(None);
        let mut guard = LAST_DUMP.lock().unwrap();
        let map = guard.get_or_insert_with(HashMap::new);
        let now = Instant::now();
        match map.get(&initial_input_size) {
            Some(last) if now.duration_since(*last) < Duration::from_secs(2) => false,
            _ => {
                map.insert(initial_input_size, now);
                true
            }
        }
    };
    if stage_readback {
        gap_stage_readback(renderer, "pipeline-input", &current);
    }

    for stage in &effect.pipeline {
        let stage_label = match stage {
            EffectStage::Noise(_) => "noise",
            EffectStage::DualKawaseBlur(_) => "blur",
            EffectStage::Shader(_) => "shader",
            EffectStage::Save(_) => "save",
            EffectStage::Blend { .. } => "blend",
            EffectStage::Unit(_) => "unit",
            EffectStage::RenderTo { .. } => "render-to",
        };
        current = match stage {
            EffectStage::Noise(noise) => apply_noise_stage(
                renderer,
                current,
                current_size,
                ctx.content_rect,
                noise.clone(),
                cache.as_deref_mut(),
            )?,
            EffectStage::DualKawaseBlur(blur) => {
                let pyramid = cache.as_deref_mut().map(EffectPipelineCache::blur_pyramid);
                preblur_backdrop_texture(
                    renderer,
                    current,
                    current_size,
                    blur.radius,
                    blur.passes,
                    pyramid,
                )?
            }
            EffectStage::Shader(shader) => apply_texture_shader_stage(
                renderer,
                current,
                current_size,
                shader,
                ctx,
                cache.as_deref_mut(),
            )?,
            EffectStage::Save(name) => {
                ctx.named.insert(name.clone(), current.clone());
                current
            }
            EffectStage::Blend { input, mode, alpha } => {
                let other =
                    resolve_effect_input(renderer, input, ctx, current_size, cache.as_deref_mut())?;
                apply_blend_stage(
                    renderer,
                    current,
                    other,
                    current_size,
                    *mode,
                    *alpha,
                    cache.as_deref_mut(),
                )?
            }
            EffectStage::Unit(effect) => {
                let _ = run_effect_pipeline(
                    renderer,
                    effect,
                    ctx,
                    final_sample_region,
                    output_size,
                    cache.as_deref_mut(),
                    BackdropFinishMode::Materialize,
                )?;
                current
            }
            EffectStage::RenderTo { target, effect } => {
                let state_size = effect_state_size(ctx.state_base_size, target.scale);
                let target_format = effect_state_fourcc(target.format);
                let previous_target_format = cache
                    .as_deref_mut()
                    .ok_or(ShaderEffectError::StateRequiresCache)?
                    .target_format
                    .replace(target_format);
                let previous_size = ctx.size;
                let previous_content_rect = ctx.content_rect;
                ctx.size = state_size;
                ctx.content_rect = Rectangle::from_size(state_size.into());
                let rendered = run_effect_pipeline(
                    renderer,
                    effect,
                    ctx,
                    None,
                    Some(state_size),
                    cache.as_deref_mut(),
                    BackdropFinishMode::Materialize,
                );
                cache
                    .as_deref_mut()
                    .expect("state cache was checked")
                    .target_format = previous_target_format;
                ctx.size = previous_size;
                ctx.content_rect = previous_content_rect;
                let rendered = rendered?;
                cache
                    .as_deref_mut()
                    .ok_or(ShaderEffectError::StateRequiresCache)?
                    .commit_state(renderer, target, ctx.state_base_size, &rendered)?;
                current
            }
        };
        if stage_readback {
            gap_stage_readback(renderer, stage_label, &current);
        }
    }

    if effect.is_backdrop() && finish_mode == BackdropFinishMode::Materialize {
        // The opaque finish hides capture/blur alpha noise for plain backdrop
        // blurs (see EffectAlphaMode for the full rationale). Effects that
        // declare `alpha: "preserve"` intentionally produce transparency
        // (e.g. layer-source masks) — forcing alpha to 1.0 there would turn
        // masked-out areas into opaque black. The mode is an explicit opt-in
        // from the config, never inferred from the pipeline contents.
        let program = match effect.alpha {
            crate::ssd::EffectAlphaMode::Preserve => {
                compile_alpha_preserving_finish_program(renderer)?
            }
            crate::ssd::EffectAlphaMode::Opaque => compile_opaque_finish_program(renderer)?,
        };
        if let Some(region) = final_sample_region {
            let target_size =
                output_size.unwrap_or((region.size.w.round() as i32, region.size.h.round() as i32));
            current = apply_texture_program_region(
                renderer,
                current,
                target_size,
                Some(region),
                program,
                effect_context_uniforms(
                    current_size,
                    effect_content_rect(current_size, Some(region)),
                ),
                cache.as_deref_mut(),
                "effect-crop-finish",
            )?;
        } else {
            current = apply_texture_program(
                renderer,
                current,
                current_size,
                program,
                effect_context_uniforms(current_size, ctx.content_rect),
                cache,
                "effect-finish",
            )?;
        }
        if stage_readback {
            gap_stage_readback(renderer, "finish", &current);
        }
    }

    Ok(current)
}

fn resolve_effect_input(
    renderer: &mut GlesRenderer,
    input: &EffectInput,
    ctx: &mut EffectExecutionContext,
    requested_size: (i32, i32),
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    if let EffectInput::State(state) = input {
        return cache
            .ok_or(ShaderEffectError::StateRequiresCache)?
            .state_texture(renderer, state, ctx.state_base_size);
    }
    let texture = match input {
        EffectInput::Backdrop | EffectInput::WindowSource(_) => Ok(ctx.backdrop.clone()),
        EffectInput::LayerSource(_) => {
            ctx.layer_source
                .clone()
                .ok_or(ShaderEffectError::UnavailableInput {
                    input: "layerSource",
                })
        }
        EffectInput::PopupSource(_) => {
            ctx.popup_source
                .clone()
                .ok_or(ShaderEffectError::UnavailableInput {
                    input: "popupSource",
                })
        }
        EffectInput::XrayBackdrop => {
            ctx.xray_backdrop
                .clone()
                .ok_or(ShaderEffectError::UnavailableInput {
                    input: "xrayBackdropSource",
                })
        }
        EffectInput::Shader(stage) if !stage.textures.is_empty() => {
            let texture = solid_white_texture(renderer)?;
            apply_texture_shader_stage(renderer, texture, requested_size, stage, ctx, cache)
        }
        EffectInput::Shader(stage) => {
            apply_shader_input_stage(renderer, requested_size, stage, cache)
        }
        EffectInput::Named(name) => ctx
            .named
            .get(name)
            .cloned()
            .ok_or_else(|| ShaderEffectError::MissingNamedTexture { name: name.clone() }),
        EffectInput::State(_) => unreachable!("state inputs return above"),
        EffectInput::Image(path) => load_image_texture(renderer, path, requested_size),
    }?;
    align_effect_input_texture(renderer, texture, requested_size, ctx.content_rect)
}

fn effect_content_rect(
    size: (i32, i32),
    sample_region: Option<Rectangle<f64, Buffer>>,
) -> Rectangle<i32, Buffer> {
    sample_region
        .map(|region| {
            Rectangle::new(
                Point::from((region.loc.x.round() as i32, region.loc.y.round() as i32)),
                (
                    region.size.w.round().max(1.0) as i32,
                    region.size.h.round().max(1.0) as i32,
                )
                    .into(),
            )
        })
        .unwrap_or_else(|| Rectangle::from_size(size.into()))
}

fn effect_context_uniforms(
    size: (i32, i32),
    content_rect: Rectangle<i32, Buffer>,
) -> Vec<Uniform<'static>> {
    vec![
        Uniform::new("effect_texture_size_px", [size.0 as f32, size.1 as f32]),
        Uniform::new(
            "effect_content_rect_px",
            [
                content_rect.loc.x as f32,
                content_rect.loc.y as f32,
                content_rect.size.w as f32,
                content_rect.size.h as f32,
            ],
        ),
    ]
}

fn align_effect_input_texture(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    canvas_size: (i32, i32),
    content_rect: Rectangle<i32, Buffer>,
) -> Result<GlesTexture, ShaderEffectError> {
    let texture_size = texture.size();
    if texture_size.w == canvas_size.0 && texture_size.h == canvas_size.1 {
        return Ok(texture);
    }

    let mut target =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, canvas_size.into())?;
    let element = TextureRenderElement::from_static_texture(
        Id::new(),
        renderer.context_id(),
        Point::<f64, Physical>::from((content_rect.loc.x as f64, content_rect.loc.y as f64)),
        texture,
        1,
        Transform::Normal,
        Some(1.0),
        None,
        Some((content_rect.size.w, content_rect.size.h).into()),
        None,
        Kind::Unspecified,
    );
    let mut framebuffer = renderer.bind(&mut target)?;
    let mut damage_tracker = OutputDamageTracker::new(canvas_size, 1.0, Transform::Normal);
    damage_tracker
        .render_output(
            renderer,
            &mut framebuffer,
            0,
            &[element],
            [0.0, 0.0, 0.0, 0.0],
        )
        .map_err(|_| GlesError::FramebufferBindingError)?;
    drop(framebuffer);
    Ok(target)
}

fn requested_effect_output_size(
    sample_region: Option<Rectangle<f64, Buffer>>,
    output_size: Option<(i32, i32)>,
) -> Option<(i32, i32)> {
    output_size.or_else(|| {
        sample_region.map(|region| (region.size.w.round() as i32, region.size.h.round() as i32))
    })
}

fn effect_input_renders_directly_to_requested_size(input: &EffectInput) -> bool {
    matches!(
        input,
        EffectInput::Shader(_) | EffectInput::Image(_) | EffectInput::State(_)
    )
}

fn apply_texture_shader_stage(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    stage: &ShaderStage,
    ctx: &mut EffectExecutionContext,
    mut cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    if !stage.textures.is_empty() {
        let mut textures = Vec::with_capacity(stage.textures.len());
        for (name, input) in &stage.textures {
            textures.push((
                name.clone(),
                resolve_effect_input(renderer, input, ctx, size, cache.as_deref_mut())?,
            ));
        }
        return apply_multi_texture_shader_stage(
            renderer,
            texture,
            textures,
            size,
            ctx.content_rect,
            stage,
            cache,
        );
    }
    let program = compile_texture_stage_program(renderer, stage)?;
    let mut uniforms = vec![
        Uniform::new("effect_texture_size_px", [size.0 as f32, size.1 as f32]),
        Uniform::new(
            "effect_content_rect_px",
            [
                ctx.content_rect.loc.x as f32,
                ctx.content_rect.loc.y as f32,
                ctx.content_rect.size.w as f32,
                ctx.content_rect.size.h as f32,
            ],
        ),
    ];
    for (name, value) in &stage.uniforms {
        append_shader_uniform_values(&mut uniforms, name, value);
    }
    apply_texture_program(
        renderer,
        texture,
        size,
        program,
        uniforms,
        cache,
        "effect-texture-shader",
    )
}

fn apply_multi_texture_shader_stage(
    renderer: &mut GlesRenderer,
    current: GlesTexture,
    textures: Vec<(String, GlesTexture)>,
    size: (i32, i32),
    content_rect: Rectangle<i32, Buffer>,
    stage: &ShaderStage,
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    let program = multi_texture_stage_program(renderer, stage)?;
    if program.renderer_context_id != renderer.context_id() {
        return Err(ShaderEffectError::RendererContextMismatch);
    }
    let target = effect_pipeline_target(renderer, size, cache)?;
    renderer.with_context(|gl| unsafe {
        with_gpu_timing_gl_span(gl, "effect-multi-texture-shader", size, || {
            while gl.GetError() != ffi::NO_ERROR {}
            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);

            let fbo = ensure_blur_scratch_fbo(gl);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                target.tex_id(),
                0,
            );
            gl.Viewport(0, 0, size.0, size.1);
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform2f(program.uniform_texture_size, size.0 as f32, size.1 as f32);
            gl.Uniform4f(
                program.uniform_content_rect,
                content_rect.loc.x as f32,
                content_rect.loc.y as f32,
                content_rect.size.w as f32,
                content_rect.size.h as f32,
            );

            for (index, ((_, location), (_, texture))) in program
                .texture_uniforms
                .iter()
                .zip(textures.iter())
                .enumerate()
            {
                let unit = index + 1;
                gl.ActiveTexture(ffi::TEXTURE0 + unit as u32);
                gl.BindTexture(ffi::TEXTURE_2D, texture.tex_id());
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.Uniform1i(*location, unit as i32);
            }
            for (name, location) in &program.value_uniforms {
                if let Some(value) = stage.uniforms.get(name) {
                    match value {
                        ShaderUniformValue::Float(value) => gl.Uniform1f(*location, *value),
                        ShaderUniformValue::Vec2(value) => {
                            gl.Uniform2f(*location, value[0], value[1])
                        }
                        ShaderUniformValue::Vec3(value) => {
                            gl.Uniform3f(*location, value[0], value[1], value[2])
                        }
                        ShaderUniformValue::Vec4(value) => {
                            gl.Uniform4f(*location, value[0], value[1], value[2], value[3])
                        }
                        ShaderUniformValue::FloatArray(values) => {
                            gl.Uniform1fv(*location, values.len() as i32, values.as_ptr())
                        }
                        ShaderUniformValue::Vec2Array(values) => {
                            gl.Uniform2fv(*location, values.len() as i32, values.as_ptr().cast())
                        }
                        ShaderUniformValue::Vec3Array(values) => {
                            gl.Uniform3fv(*location, values.len() as i32, values.as_ptr().cast())
                        }
                        ShaderUniformValue::Vec4Array(values) => {
                            gl.Uniform4fv(*location, values.len() as i32, values.as_ptr().cast())
                        }
                    }
                }
            }

            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, current.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            gl.EnableVertexAttribArray(program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.VertexAttribPointer(
                program.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                vertices.as_ptr().cast(),
            );
            gl.DrawArrays(ffi::TRIANGLES, 0, 6);

            for unit in 0..=textures.len() {
                gl.ActiveTexture(ffi::TEXTURE0 + unit as u32);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
            }
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.DisableVertexAttribArray(program.attrib_vert as u32);
            gl.UseProgram(0);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
            gl.Enable(ffi::SCISSOR_TEST);
        });
        Ok::<_, GlesError>(())
    })??;
    Ok(target)
}

fn apply_shader_input_stage(
    renderer: &mut GlesRenderer,
    size: (i32, i32),
    stage: &ShaderStage,
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    timescope::scope!("effect shader input stage");
    let effect = CompiledEffect {
        input: EffectInput::Shader(stage.clone()),
        capture_padding: 0,
        invalidate: EffectInvalidationPolicy::Always,
        pipeline: Vec::new(),
        alpha: crate::ssd::EffectAlphaMode::Opaque,
    };
    let spec = ShaderEffectSpec {
        rect: Rectangle::new(Point::from((0, 0)), size.into()),
        geometry: Rectangle::new(Point::from((0, 0)), size.into()),
        framebuffer_regions: Vec::new(),
        framebuffer_capture_padding: 0,
        shader: effect,
        alpha_bits: 1.0f32.to_bits(),
        render_scale: 1.0,
        clip_rect: None,
        clip_radius: 0.0,
    };
    let mut state = ShaderEffectElementState::default();
    let element = state.element(renderer, spec)?;
    with_gpu_timing_renderer_span(renderer, "effect-shader-input", size, |renderer| {
        renderer.with_deferred_frame_flushes(|renderer| {
            let mut target = effect_pipeline_target(renderer, size, cache)?;
            let mut framebuffer = renderer.bind(&mut target)?;
            let mut damage_tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            let _ = damage_tracker
                .render_output(
                    renderer,
                    &mut framebuffer,
                    0,
                    &[element],
                    [0.0, 0.0, 0.0, 0.0],
                )
                .map_err(|_| GlesError::FramebufferBindingError)?;
            drop(framebuffer);
            Ok(target)
        })
    })
}

fn apply_noise_stage(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    content_rect: Rectangle<i32, Buffer>,
    noise: NoiseStage,
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    match noise.kind {
        NoiseKind::Salt => {
            let program = compile_noise_salt_program(renderer)?;
            apply_texture_program(
                renderer,
                texture,
                size,
                program,
                {
                    let mut uniforms = effect_context_uniforms(size, content_rect);
                    uniforms.push(Uniform::new("noise_amount", noise.amount));
                    uniforms
                },
                cache,
                "effect-noise",
            )
        }
    }
}

fn apply_blend_stage(
    renderer: &mut GlesRenderer,
    current: GlesTexture,
    other: GlesTexture,
    size: (i32, i32),
    mode: BlendMode,
    alpha: f32,
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    timescope::scope!("effect blend stage");
    let programs = blend_shader_programs(renderer)?;
    if programs.renderer_context_id != renderer.context_id() {
        return Err(ShaderEffectError::RendererContextMismatch);
    }

    let target = effect_pipeline_target(renderer, size, cache)?;
    renderer.with_context(|gl| unsafe {
        with_gpu_timing_gl_span(gl, "effect-blend", size, || {
            while gl.GetError() != ffi::NO_ERROR {}

            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);
            gl.ActiveTexture(ffi::TEXTURE0);

            let fbo = ensure_blur_scratch_fbo(gl);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                target.tex_id(),
                0,
            );

            gl.Viewport(0, 0, size.0, size.1);
            gl.UseProgram(programs.program.program);
            gl.Uniform1i(programs.program.uniform_tex, 0);
            gl.Uniform1i(programs.program.uniform_tex2, 1);
            gl.Uniform1f(programs.program.uniform_blend_mode, blend_mode_value(mode));
            gl.Uniform1f(programs.program.uniform_blend_alpha, alpha.clamp(0.0, 1.0));

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            gl.EnableVertexAttribArray(programs.program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.VertexAttribPointer(
                programs.program.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                vertices.as_ptr().cast(),
            );

            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, current.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);

            gl.ActiveTexture(ffi::TEXTURE1);
            gl.BindTexture(ffi::TEXTURE_2D, other.tex_id());
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);

            gl.DrawArrays(ffi::TRIANGLES, 0, 6);

            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, 0);
            gl.DisableVertexAttribArray(programs.program.attrib_vert as u32);
            gl.UseProgram(0);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
            gl.Enable(ffi::SCISSOR_TEST);
        });
        Ok::<_, GlesError>(())
    })??;

    Ok(target)
}

fn blend_mode_value(mode: BlendMode) -> f32 {
    match mode {
        BlendMode::Normal => 0.0,
        BlendMode::Add => 1.0,
        BlendMode::Screen => 2.0,
        BlendMode::Multiply => 3.0,
    }
}

fn apply_texture_program(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    cache: Option<&mut EffectPipelineCache>,
    timing_label: &'static str,
) -> Result<GlesTexture, ShaderEffectError> {
    apply_texture_program_region(
        renderer,
        texture,
        size,
        None,
        program,
        uniforms,
        cache,
        timing_label,
    )
}

fn apply_texture_program_region(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    output_size: (i32, i32),
    source_region: Option<Rectangle<f64, Buffer>>,
    program: GlesTexProgram,
    uniforms: Vec<Uniform<'static>>,
    cache: Option<&mut EffectPipelineCache>,
    timing_label: &'static str,
) -> Result<GlesTexture, ShaderEffectError> {
    timescope::scope!("effect texture program");
    with_gpu_timing_renderer_span(renderer, timing_label, output_size, |renderer| {
        let target = effect_pipeline_target(renderer, output_size, cache)?;
        let source_region =
            source_region.unwrap_or_else(|| Rectangle::from_size(texture.size().to_f64()));
        renderer.render_texture_to_texture(
            &texture,
            &target,
            source_region,
            Some(&program),
            &uniforms,
        )?;
        Ok(target)
    })
}

fn effect_pipeline_target(
    renderer: &mut GlesRenderer,
    size: (i32, i32),
    cache: Option<&mut EffectPipelineCache>,
) -> Result<GlesTexture, ShaderEffectError> {
    match cache {
        Some(cache) => cache.target(renderer, size),
        None => Ok(Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            size.into(),
        )?),
    }
}

pub fn solid_white_texture(renderer: &mut GlesRenderer) -> Result<GlesTexture, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<SolidWhiteTextureCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(SolidWhiteTextureCache::default);
    }

    if let Some(texture) = renderer
        .egl_context()
        .user_data()
        .get::<SolidWhiteTextureCache>()
        .expect("solid white texture cache should exist")
        .0
        .lock()
        .unwrap()
        .clone()
    {
        return Ok(texture);
    }

    let rgba = [16u8, 19u8, 25u8, 255u8];
    let texture = renderer.import_memory(&rgba, Fourcc::Abgr8888, (1, 1).into(), false)?;
    *renderer
        .egl_context()
        .user_data()
        .get::<SolidWhiteTextureCache>()
        .expect("solid white texture cache should exist")
        .0
        .lock()
        .unwrap() = Some(texture.clone());
    Ok(texture)
}

fn load_image_texture(
    renderer: &mut GlesRenderer,
    path: &str,
    size: (i32, i32),
) -> Result<GlesTexture, ShaderEffectError> {
    if renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .is_none()
    {
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(ImageTextureCache::default);
    }

    let cache_key = (path.to_string(), size.0, size.1);
    if let Some(texture) = renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .expect("image texture cache should exist")
        .0
        .lock()
        .unwrap()
        .get(&cache_key)
        .cloned()
    {
        return Ok(texture);
    }

    let bytes = fs::read(path).map_err(|source| ShaderEffectError::ReadShader {
        path: path.to_string(),
        source,
    })?;
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    let rgba = match extension.as_deref() {
        Some("png") => decode_png_and_scale(&bytes, size.0, size.1),
        Some("svg") => decode_svg_and_scale(&bytes, size.0, size.1),
        _ => decode_png_and_scale(&bytes, size.0, size.1),
    }
    .ok_or_else(|| ShaderEffectError::ImageDecode {
        path: path.to_string(),
    })?;

    let texture = renderer.import_memory(&rgba, Fourcc::Abgr8888, size.into(), false)?;
    renderer
        .egl_context()
        .user_data()
        .get::<ImageTextureCache>()
        .expect("image texture cache should exist")
        .0
        .lock()
        .unwrap()
        .insert(cache_key, texture.clone());
    Ok(texture)
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
            let source_y =
                ((y as f32 / target_height as f32) * source_height as f32).floor() as i32;
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

/// Debug probe (SHOJI_GAP_STAGE_READBACK): read the 3 right-edge pixels of
/// the middle row of `tex` and log them, to find which pipeline stage stops
/// writing the last column. No-op cost is one env lookup at each gate site.
fn gap_stage_readback(renderer: &mut GlesRenderer, label: &str, tex: &GlesTexture) {
    let tex = tex.clone();
    let _ = renderer.with_context(|gl| unsafe {
        let size = tex.size();
        if size.w < 1 || size.h < 1 {
            return;
        }
        let mut prev_read_fbo = 0i32;
        gl.GetIntegerv(ffi::READ_FRAMEBUFFER_BINDING, &mut prev_read_fbo);
        let fbo = ensure_blur_scratch_fbo(gl);
        gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, fbo);
        gl.FramebufferTexture2D(
            ffi::READ_FRAMEBUFFER,
            ffi::COLOR_ATTACHMENT0,
            ffi::TEXTURE_2D,
            tex.tex_id(),
            0,
        );
        let w = 3.min(size.w);
        let mut pixels = [0u8; 3 * 4];
        gl.ReadPixels(
            size.w - w,
            size.h / 2,
            w,
            1,
            ffi::RGBA,
            ffi::UNSIGNED_BYTE,
            pixels.as_mut_ptr().cast(),
        );
        gl.BindFramebuffer(ffi::READ_FRAMEBUFFER, prev_read_fbo as u32);
        let mut row = [[0u8; 4]; 3];
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            row[i] = [px[0], px[1], px[2], px[3]];
        }
        tracing::info!(
            label,
            tex_size = ?size,
            right_mid_row = ?row,
            "gap debug stage right-edge readback"
        );
    });
}

pub fn preblur_backdrop_texture(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    size: (i32, i32),
    radius: i32,
    passes: i32,
    pyramid_cache: Option<&mut Vec<GlesTexture>>,
) -> Result<GlesTexture, ShaderEffectError> {
    if radius <= 0 || passes <= 0 {
        return Ok(texture);
    }

    let programs = blur_shader_programs(renderer)?;
    let passes = passes.clamp(1, 8) as usize;
    let offset = radius.max(1) as f32;
    if programs.renderer_context_id != renderer.context_id() {
        return Err(ShaderEffectError::RendererContextMismatch);
    }

    if let Some(pyramid) = pyramid_cache {
        return preblur_using_pyramid(renderer, texture, size, &programs, passes, offset, pyramid);
    }

    // Uncached fallback for callers that do not own persistent pipeline
    // state. Live framebuffer backdrops use the cached pyramid path above.
    let mut levels = Vec::with_capacity(passes + 1);
    let mut current = texture;
    let mut current_size = size;
    levels.push((current.clone(), current_size));

    for _ in 0..passes {
        let next_size = (max(1, current_size.0 / 2), max(1, current_size.1 / 2));
        current = blur_texture_pass(
            renderer,
            current,
            next_size,
            &programs.down,
            [0.5f32 / next_size.0 as f32, 0.5f32 / next_size.1 as f32],
            offset,
        )?;
        current_size = next_size;
        levels.push((current.clone(), current_size));
    }

    for idx in (1..levels.len()).rev() {
        let (src_texture, src_size) = levels[idx].clone();
        let dst_size = levels[idx - 1].1;
        current = blur_texture_pass(
            renderer,
            src_texture,
            dst_size,
            &programs.up,
            [0.5f32 / src_size.0 as f32, 0.5f32 / src_size.1 as f32],
            offset,
        )?;
        levels[idx - 1].0 = current.clone();
    }

    Ok(current)
}

/// Reuses textures held in `pyramid` across frames; only allocates on first
/// run or when the source size changes. Texture handle layout:
///
/// - `pyramid[0]` — output texture, same size as `source`
/// - `pyramid[1..=passes]` — progressively halved intermediates
///
/// Each render: source → pyramid[1] → … → pyramid[passes] (down) →
/// pyramid[passes-1] → … → pyramid[0] (up).
fn preblur_using_pyramid(
    renderer: &mut GlesRenderer,
    source: GlesTexture,
    source_size: (i32, i32),
    programs: &BlurShaderPrograms,
    passes: usize,
    offset: f32,
    pyramid: &mut Vec<GlesTexture>,
) -> Result<GlesTexture, ShaderEffectError> {
    prepare_blur_pyramid(renderer, pyramid, source_size, passes)?;

    let stage_readback = std::env::var_os("SHOJI_GAP_STAGE_READBACK").is_some() && {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static TICK: AtomicUsize = AtomicUsize::new(0);
        TICK.fetch_add(1, Ordering::Relaxed).is_multiple_of(600)
    };

    // Down-sample chain
    let mut current_tex = source;
    let mut current_size = source_size;
    // Indexed on purpose: state carries across iterations, both forward and reversed.
    #[allow(clippy::needless_range_loop)]
    for i in 1..=passes {
        let dst_size = (max(1, current_size.0 / 2), max(1, current_size.1 / 2));
        let dst_tex = pyramid[i].clone();
        blur_texture_pass_into(
            renderer,
            &current_tex,
            &dst_tex,
            dst_size,
            "blur-downsample-pass",
            &programs.down,
            [0.5f32 / dst_size.0 as f32, 0.5f32 / dst_size.1 as f32],
            offset,
        )?;
        current_tex = dst_tex;
        current_size = dst_size;
        if stage_readback {
            gap_stage_readback(renderer, &format!("blur-down-{i}"), &current_tex);
        }
    }

    // Up-sample chain, writing back into the larger pyramid level each step.
    // The final result lands in `pyramid[0]`.
    for i in (0..passes).rev() {
        let src_tex = pyramid[i + 1].clone();
        let src_size = (src_tex.size().w, src_tex.size().h);
        let dst_tex = pyramid[i].clone();
        let dst_size = (dst_tex.size().w, dst_tex.size().h);
        blur_texture_pass_into(
            renderer,
            &src_tex,
            &dst_tex,
            dst_size,
            "blur-upsample-pass",
            &programs.up,
            [0.5f32 / src_size.0 as f32, 0.5f32 / src_size.1 as f32],
            offset,
        )?;
        if stage_readback {
            gap_stage_readback(renderer, &format!("blur-up-{i}"), &dst_tex);
        }
    }

    Ok(pyramid[0].clone())
}

/// Ensures `pyramid` has `passes + 1` textures sized to match the
/// down-sample chain from `source_size`. Resets the entire pyramid when the
/// output size (`pyramid[0]`) no longer matches, then top-up creates any
/// missing levels. Excess levels are dropped so a `passes` decrease frees
/// the extras.
fn prepare_blur_pyramid(
    renderer: &mut GlesRenderer,
    pyramid: &mut Vec<GlesTexture>,
    source_size: (i32, i32),
    passes: usize,
) -> Result<(), ShaderEffectError> {
    if let Some(first) = pyramid.first() {
        let first_size = first.size();
        if (first_size.w, first_size.h) != source_size {
            pyramid.clear();
        }
    }

    let mut w = source_size.0;
    let mut h = source_size.1;
    for i in 0..=passes {
        let level_size = (w.max(1), h.max(1));
        if i >= pyramid.len() {
            let texture = <GlesRenderer as Offscreen<GlesTexture>>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                level_size.into(),
            )?;
            pyramid.push(texture);
        }
        w = max(1, w / 2);
        h = max(1, h / 2);
    }

    pyramid.truncate(passes + 1);
    Ok(())
}

/// Down/up-sample blur kernel that writes into a pre-allocated `target`
/// texture. The non-cached `blur_texture_pass` is a thin wrapper that
/// allocates `target` first then delegates here.
fn blur_texture_pass_into(
    renderer: &mut GlesRenderer,
    source: &GlesTexture,
    target: &GlesTexture,
    output_size: (i32, i32),
    timing_label: &'static str,
    program: &BlurProgramInternal,
    half_pixel: [f32; 2],
    offset: f32,
) -> Result<(), ShaderEffectError> {
    let source_tex_id = source.tex_id();
    let target_tex_id = target.tex_id();

    renderer.with_context(|gl| unsafe {
        with_gpu_timing_gl_span(gl, timing_label, output_size, || {
            while gl.GetError() != ffi::NO_ERROR {}

            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);
            gl.ActiveTexture(ffi::TEXTURE0);

            let fbo = ensure_blur_scratch_fbo(gl);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::DRAW_FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                target_tex_id,
                0,
            );

            gl.Viewport(0, 0, output_size.0, output_size.1);
            gl.UseProgram(program.program);
            gl.Uniform1i(program.uniform_tex, 0);
            gl.Uniform2f(program.uniform_half_pixel, half_pixel[0], half_pixel[1]);
            gl.Uniform1f(program.uniform_offset, offset);

            let vertices: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
            gl.EnableVertexAttribArray(program.attrib_vert as u32);
            gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
            gl.VertexAttribPointer(
                program.attrib_vert as u32,
                2,
                ffi::FLOAT,
                ffi::FALSE,
                0,
                vertices.as_ptr().cast(),
            );

            gl.BindTexture(ffi::TEXTURE_2D, source_tex_id);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_S,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.TexParameteri(
                ffi::TEXTURE_2D,
                ffi::TEXTURE_WRAP_T,
                ffi::CLAMP_TO_EDGE as i32,
            );
            gl.DrawArrays(ffi::TRIANGLES, 0, 6);

            gl.DisableVertexAttribArray(program.attrib_vert as u32);
            gl.BindFramebuffer(ffi::DRAW_FRAMEBUFFER, 0);
        });
    })?;

    Ok(())
}

fn blur_texture_pass(
    renderer: &mut GlesRenderer,
    texture: GlesTexture,
    output_size: (i32, i32),
    program: &BlurProgramInternal,
    half_pixel: [f32; 2],
    offset: f32,
) -> Result<GlesTexture, ShaderEffectError> {
    // Uncached fallback. Live framebuffer backdrops reuse textures via
    // `blur_texture_pass_into` directly.
    let target =
        Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, output_size.into())?;
    blur_texture_pass_into(
        renderer,
        &texture,
        &target,
        output_size,
        "blur-pass",
        program,
        half_pixel,
        offset,
    )?;
    Ok(target)
}
