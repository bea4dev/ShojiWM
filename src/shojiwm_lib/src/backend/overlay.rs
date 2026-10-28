//! Output-only effects. Control messages never pass through workspace configuration.
use std::{
    collections::{BTreeMap, HashMap},
    io,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use crate::runtime_api::RuntimeHost;
use smithay::{
    backend::renderer::{
        ContextId, Renderer, Texture,
        damage::OutputDamageTracker,
        element::{Id, Kind, RenderElement, texture::TextureRenderElement},
        gles::{GlesRenderer, GlesTexture},
    },
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale, Transform},
};
use tokio::sync::Notify;

use super::{
    shader_effect::{EffectInstancePipelineCache, apply_overlay_effect},
    snapshot,
};
use crate::ssd::{CompiledEffect, EffectInput, EffectInvalidationPolicy, EffectStage, LogicalRect};

// The existing named-input mechanism keeps the window/layer shader context unchanged.
pub const SNAPSHOT_NAME: &str = "__shoji_overlay_snapshot";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Placement {
    Top,
    BelowLayers,
}

struct ControlState {
    effect: Arc<CompiledEffect>,
    revision: u64,
    ready: bool,
    deadline: Option<Instant>,
    closed: Option<String>,
}

pub struct Control {
    id: u32,
    output: String,
    placement: Placement,
    owner: Arc<AtomicBool>,
    host: RuntimeHost,
    persistent: bool,
    snapshot: bool,
    unlock: bool,
    state: Mutex<ControlState>,
    changed: Notify,
}

static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static CONTROLS: OnceLock<Mutex<BTreeMap<u32, Arc<Control>>>> = OnceLock::new();
static DIRTY: AtomicBool = AtomicBool::new(false);

fn controls() -> &'static Mutex<BTreeMap<u32, Arc<Control>>> {
    CONTROLS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(crate) fn wake() {
    DIRTY.store(true, Ordering::Release);
    for control in controls().lock().unwrap().values() {
        control.host.notify();
    }
}

pub(crate) fn uses_snapshot(effect: &CompiledEffect) -> bool {
    fn input(value: &EffectInput) -> bool {
        match value {
            EffectInput::Named(name) => name == SNAPSHOT_NAME,
            EffectInput::Shader(shader) => shader.textures.values().any(input),
            _ => false,
        }
    }
    input(&effect.input)
        || effect.pipeline.iter().any(|stage| match stage {
            EffectStage::Shader(shader) => shader.textures.values().any(input),
            EffectStage::Blend { input: source, .. } => input(source),
            EffectStage::Unit(effect) | EffectStage::RenderTo { effect, .. } => {
                uses_snapshot(effect)
            }
            _ => false,
        })
}

fn validate(effect: &CompiledEffect) -> io::Result<()> {
    if effect.capture_padding != 0
        || effect.uses_xray_backdrop_input()
        || effect.uses_window_source_input()
        || effect.uses_layer_source_input()
        || effect.uses_popup_source_input()
    {
        return Err(io::Error::other(
            "Output effects require zero capturePadding and output, image, state or shader inputs",
        ));
    }
    Ok(())
}

pub fn create(
    owner: &Arc<AtomicBool>,
    output: String,
    placement: String,
    duration: f64,
    effect: CompiledEffect,
    persistent: bool,
    host: RuntimeHost,
) -> io::Result<u32> {
    validate(&effect)?;
    if !owner.load(Ordering::Acquire)
        || output.is_empty()
        || !duration.is_finite()
        || duration <= 0.0
        || duration > i32::MAX as f64
    {
        return Err(io::Error::other(
            "Invalid output overlay request or inactive runtime",
        ));
    }
    let unlock = placement == "unlock";
    if unlock && (!super::lock_scene::is_locked() || persistent || duration > 3000.0
        || !uses_snapshot(&effect)) {
        return Err(io::Error::other("Unlock overlays require an active lock, a snapshot and a nonpersistent deadline <= 3000ms"));
    }
    let placement = match placement.as_str() {
        "top" => Placement::Top,
        "unlock" => Placement::Top,
        "below-layers" => Placement::BelowLayers,
        _ => return Err(io::Error::other("Unknown output overlay placement")),
    };
    let mut registry = controls().lock().unwrap();
    if registry.len() >= 64 {
        return Err(io::Error::other("Too many output overlay requests"));
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    if registry.contains_key(&id) {
        return Err(io::Error::other("Output overlay ID exhausted"));
    }
    registry.insert(
        id,
        Arc::new(Control {
            id,
            output,
            placement,
            owner: owner.clone(),
            host,
            persistent,
            snapshot: uses_snapshot(&effect),
            unlock,
            state: Mutex::new(ControlState {
                effect: Arc::new(effect),
                revision: 1,
                ready: false,
                closed: None,
                deadline: Some(Instant::now() + Duration::from_secs_f64(duration / 1000.0)),
            }),
            changed: Notify::new(),
        }),
    );
    drop(registry);
    wake();
    Ok(id)
}

pub fn get(owner: &Arc<AtomicBool>, id: u32) -> io::Result<Arc<Control>> {
    controls()
        .lock()
        .unwrap()
        .get(&id)
        .filter(|control| Arc::ptr_eq(owner, &control.owner))
        .cloned()
        .ok_or_else(|| io::Error::other("Output overlay is closed"))
}

impl Control {
    fn close(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        if state.closed.is_none() {
            state.closed = Some(reason.to_owned());
            self.changed.notify_waiters();
            DIRTY.store(true, Ordering::Release);
            // A runtime that polls (`is_closed`) instead of awaiting learns it next tick.
            self.host.wake();
        }
    }

    fn alive(&self) -> bool {
        if !self.owner.load(Ordering::Acquire) {
            self.close("Runtime stopped");
        }
        let mut state = self.state.lock().unwrap();
        if state.closed.is_none()
            && state
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            state.closed = Some("Output overlay timed out".into());
            self.changed.notify_waiters();
            DIRTY.store(true, Ordering::Release);
            self.host.wake();
        }
        state.closed.is_none()
    }

    fn mark_ready(&self) {
        let mut state = self.state.lock().unwrap();
        state.ready = true;
        if self.persistent {
            state.deadline = None;
        }
        self.changed.notify_waiters();
        drop(state);
        self.host.wake();
    }

    /// The first frame with the overlay has been rendered (what `wait(false)` awaits).
    pub fn is_ready(&self) -> bool {
        self.state.lock().unwrap().ready
    }

    /// Disposed, replaced, timed out or its runtime stopped (what `wait(true)` awaits).
    pub fn is_closed(&self) -> bool {
        self.alive();
        self.state.lock().unwrap().closed.is_some()
    }

    pub fn update(&self, effect: CompiledEffect) -> io::Result<()> {
        validate(&effect)?;
        if uses_snapshot(&effect) != self.snapshot {
            return Err(io::Error::other(
                "Create a new overlay to change snapshot inputs",
            ));
        }
        let mut state = self.state.lock().unwrap();
        if state.closed.is_some() {
            return Ok(());
        }
        if *state.effect != effect {
            // ponytail: coalesce complete descriptors; add uniform patches if profiling warrants it.
            state.effect = Arc::new(effect);
            state.revision = state.revision.wrapping_add(1);
            drop(state);
            wake();
        }
        Ok(())
    }

    pub fn dispose(&self) {
        self.close("Disposed");
        wake();
    }

    pub async fn wait(&self, closed: bool) -> io::Result<()> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let deadline = {
                let state = self.state.lock().unwrap();
                if let Some(reason) = &state.closed {
                    return if closed {
                        Ok(())
                    } else {
                        Err(io::Error::other(reason.clone()))
                    };
                }
                if !closed && state.ready {
                    return Ok(());
                }
                state.deadline
            };
            if let Some(deadline) = deadline {
                if tokio::time::timeout_at(deadline.into(), notified)
                    .await
                    .is_err()
                {
                    self.alive();
                    wake();
                }
            } else {
                notified.await;
            }
        }
    }
}

pub(crate) fn has_output(name: &str) -> bool {
    controls()
        .lock()
        .unwrap()
        .values()
        .any(|control| control.output == name && control.alive())
}

pub(crate) fn close_all(reason: &str) {
    for control in controls().lock().unwrap().values() {
        control.close(reason);
    }
    wake();
}

pub fn close_owner(owner: &Arc<AtomicBool>) {
    owner.store(false, Ordering::Release);
    for control in controls().lock().unwrap().values() {
        if Arc::ptr_eq(owner, &control.owner) {
            control.close("Runtime stopped");
        }
    }
    wake();
}

pub(crate) fn any() -> bool {
    !controls().lock().unwrap().is_empty()
}

pub(crate) fn deadline_interval_ms() -> u64 {
    let now = Instant::now();
    controls()
        .lock()
        .unwrap()
        .values()
        .filter_map(|control| control.state.lock().unwrap().deadline)
        .map(|deadline| deadline.saturating_duration_since(now).as_millis() as u64)
        .min()
        .unwrap_or(250)
        .clamp(1, 250)
}

#[derive(Default)]
pub(crate) struct OutputOverlays {
    slots: HashMap<(String, Placement), Instance>,
}

struct Instance {
    context: ContextId<GlesTexture>,
    control: Arc<Control>,
    geometry: String,
    snapshot: Option<GlesTexture>,
    cache: EffectInstancePipelineCache,
    cached: Option<(Id, GlesTexture)>,
    revision: u64,
    source_signature: u64,
}

fn geometry(output: &Output) -> String {
    format!(
        "{:?}/{:?}/{:?}",
        output.current_mode(),
        output.current_scale(),
        output.current_transform()
    )
}

fn continuous(policy: &EffectInvalidationPolicy) -> bool {
    invalidated(policy, false)
}

fn invalidated(policy: &EffectInvalidationPolicy, source_changed: bool) -> bool {
    match policy {
        EffectInvalidationPolicy::Always => true,
        EffectInvalidationPolicy::OnSourceDamageBox { .. } => source_changed,
        EffectInvalidationPolicy::Manual { dirty_when, base } => {
            *dirty_when
                || base
                    .as_deref()
                    .is_some_and(|base| invalidated(base, source_changed))
        }
    }
}

impl OutputOverlays {
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
    }

    /// Nothing to tick: no requests, no GPU slots and no pending cleanup redraw.
    pub(crate) fn is_idle(&self) -> bool {
        self.slots.is_empty() && !any() && !DIRTY.load(Ordering::Acquire)
    }

    /// Runs independently of the JS scheduler, including idle desktop deadlines.
    pub(crate) fn tick(&mut self, outputs: &[Output], locked: bool) -> bool {
        let mut registry = controls().lock().unwrap();
        for control in registry.values() {
            if locked && !(control.unlock && super::lock_scene::is_locked()) {
                control.close("Session unavailable");
            }
            if !outputs.iter().any(|output| output.name() == control.output) {
                control.close("Output removed or disabled");
            }
        }
        self.slots.retain(|(name, _), instance| {
            if !outputs
                .iter()
                .any(|output| output.name() == *name && geometry(output) == instance.geometry)
            {
                instance
                    .control
                    .close("Output mode, scale or transform changed");
            }
            instance.control.alive()
        });
        registry.retain(|_, control| control.alive());
        let redraw = registry.values().any(|control| {
            let state = control.state.lock().unwrap();
            !state.ready || continuous(&state.effect.invalidate)
        });
        DIRTY.swap(false, Ordering::AcqRel) || redraw
    }

    pub(crate) fn render<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
        logical_size: (i32, i32),
        scale: Scale<f64>,
        scene: &mut Vec<E>,
        below_layers: usize,
        wrap: impl Fn(TextureRenderElement<GlesTexture>) -> E,
    ) {
        let pending: Vec<_> = controls()
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.output == output.name() && c.alive()
                && (!super::lock_scene::is_locked() || c.unlock))
            .cloned()
            .collect();
        // Front-to-back scene: compose the lower slot first so the top sees its result.
        for (placement, index) in [(Placement::BelowLayers, below_layers), (Placement::Top, 0)] {
            let key = (output.name(), placement);
            let mut occupied = false;
            if let Some(instance) = self.slots.get_mut(&key) {
                if instance.control.alive()
                    && instance.geometry == geometry(output)
                    && instance.context == renderer.context_id()
                {
                    match instance.draw(renderer, logical_size, scale, &scene[index..]) {
                        Ok(element) => {
                            scene.insert(index, wrap(element));
                            occupied = true;
                        }
                        Err(error) => instance.control.close(&error),
                    }
                } else {
                    instance.control.close("Output changed");
                }
            }
            if !occupied {
                self.slots.remove(&key);
            }
            for control in pending.iter().filter(|c| c.placement == placement) {
                if !control.alive()
                    || self
                        .slots
                        .get(&key)
                        .is_some_and(|i| i.control.id == control.id)
                {
                    continue;
                }
                // Capture the previous visible effect before replacing it, enabling interruption.
                let frozen = if control.snapshot {
                    match capture(renderer, logical_size, scale, &scene[index..]) {
                        Ok(texture) => Some(texture),
                        Err(error) => {
                            control.close(&error);
                            continue;
                        }
                    }
                } else {
                    None
                };
                let mut instance = Instance {
                    context: renderer.context_id(),
                    control: control.clone(),
                    geometry: geometry(output),
                    snapshot: frozen,
                    cache: EffectInstancePipelineCache::default(),
                    cached: None,
                    revision: 0,
                    source_signature: 0,
                };
                match instance.draw(
                    renderer,
                    logical_size,
                    scale,
                    &scene[index + usize::from(occupied)..],
                ) {
                    Ok(element) if control.alive() => {
                        if occupied {
                            scene.remove(index);
                        }
                        scene.insert(index, wrap(element));
                        occupied = true;
                        if let Some(previous) = self.slots.insert(key.clone(), instance) {
                            previous.control.close("Replaced");
                        }
                        control.mark_ready();
                    }
                    Ok(_) => {}
                    Err(error) => control.close(&error),
                }
            }
        }
    }
}

fn capture<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    size: (i32, i32),
    scale: Scale<f64>,
    elements: &[E],
) -> Result<GlesTexture, String> {
    let rect = LogicalRect::new(0, 0, size.0, size.1);
    let physical = Rectangle::<i32, Logical>::from_size(size.into())
        .to_physical_precise_round(scale)
        .size;
    let mut tracker = OutputDamageTracker::new(physical, scale, Transform::Normal);
    snapshot::capture_snapshot(renderer, None, &mut tracker, rect, 0, true, scale, elements)
        .map_err(|error| error.to_string())?
        .map(|snapshot| snapshot.texture)
        .ok_or_else(|| "Output capture is empty".into())
}

impl Instance {
    fn draw<E: RenderElement<GlesRenderer>>(
        &mut self,
        renderer: &mut GlesRenderer,
        size: (i32, i32),
        scale: Scale<f64>,
        behind: &[E],
    ) -> Result<TextureRenderElement<GlesTexture>, String> {
        let (effect, revision) = {
            let state = self.control.state.lock().unwrap();
            (state.effect.clone(), state.revision)
        };
        let live = effect.uses_backdrop_input();
        let signature = if live {
            snapshot::render_element_scene_signature(behind, scale)
        } else {
            0
        };
        if self.cached.is_none()
            || self.revision != revision
            || invalidated(&effect.invalidate, self.source_signature != signature)
        {
            let backdrop = if live {
                Some(capture(renderer, size, scale, behind)?)
            } else {
                None
            };
            let physical = Rectangle::<i32, Logical>::from_size(size.into())
                .to_physical_precise_round(scale)
                .size;
            let texture = apply_overlay_effect(
                renderer,
                self.snapshot.clone(),
                backdrop,
                (physical.w, physical.h),
                scale.x,
                &effect,
                &mut self.cache,
            )
            .map_err(|error| error.to_string())?;
            // A fresh element id marks changed pixels even when the pipeline reuses its FBO.
            self.cached = Some((Id::new(), texture));
            self.revision = revision;
            self.source_signature = signature;
        }
        let (id, texture) = self.cached.as_ref().expect("successful pipeline is cached");
        // With buffer scale 1, the source covers physical texture pixels even
        // when the destination is a smaller logical output on a scaled display.
        let src = Rectangle::from_size(texture.size().to_logical(1, Transform::Normal).to_f64());
        Ok(TextureRenderElement::from_static_texture(
            id.clone(),
            renderer.context_id(),
            Point::<f64, Physical>::from((0.0, 0.0)),
            texture.clone(),
            1,
            Transform::Normal,
            Some(1.0),
            Some(src),
            Some(size.into()),
            None,
            Kind::Unspecified,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{EffectAlphaMode, ShaderModule, ShaderStage, ShaderUniformValue};

    fn snapshot_effect() -> CompiledEffect {
        CompiledEffect {
            input: EffectInput::Named(SNAPSHOT_NAME.into()),
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::OnSourceDamageBox { damage_padding: 0 },
            pipeline: vec![],
            alpha: EffectAlphaMode::Preserve,
        }
    }

    #[test]
    #[ignore = "requires surfaceless EGL; renders only offscreen buffers"]
    fn output_overlay_gpu_capture_replacement_and_live_alpha() {
        use smithay::backend::renderer::{ExportMem, ImportMem, Texture};
        use smithay::backend::{
            allocator::Fourcc,
            egl::{EGLContext, EGLDisplay, native::EGLSurfacelessDisplay},
        };
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        let size = (8, 4);
        let scale = Scale::from(1.5);
        let pixel_size = (12, 6);
        let red = renderer
            .import_memory(
                &[255, 0, 0, 255].repeat(72),
                Fourcc::Abgr8888,
                pixel_size.into(),
                false,
            )
            .unwrap();
        let blue = renderer
            .import_memory(
                &[0, 0, 255, 255].repeat(72),
                Fourcc::Abgr8888,
                pixel_size.into(),
                false,
            )
            .unwrap();
        let source_id = Id::new();
        let element = |renderer: &GlesRenderer, texture: &GlesTexture, alpha| {
            TextureRenderElement::from_static_texture(
                source_id.clone(),
                renderer.context_id(),
                (0.0, 0.0),
                texture.clone(),
                1,
                Transform::Normal,
                Some(alpha),
                Some(Rectangle::from_size(
                    texture.size().to_logical(1, Transform::Normal).to_f64(),
                )),
                Some(size.into()),
                None,
                Kind::Unspecified,
            )
        };
        let pixels = |renderer: &mut GlesRenderer, texture: &GlesTexture| {
            let map = renderer
                .copy_texture(
                    texture,
                    Rectangle::from_size(texture.size()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&map).unwrap().to_vec()
        };
        let pixel = |renderer: &mut GlesRenderer, texture: &GlesTexture| {
            pixels(renderer, texture)[..4].to_vec()
        };
        let output = Output::new(
            "overlay-gpu-test".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "offscreen".into(),
                serial_number: String::new(),
            },
        );
        let owner = Arc::new(AtomicBool::new(true));
        let mut overlays = OutputOverlays::default();
        let key = (output.name(), Placement::Top);
        let mut effect = snapshot_effect();
        effect.pipeline = vec![EffectStage::Shader(ShaderStage {
            shader: ShaderModule {
                path: format!(
                    "{}/../../examples/output-overlay/output-dissolve.frag",
                    env!("CARGO_MANIFEST_DIR")
                ),
            },
            uniforms: [("progress".into(), ShaderUniformValue::Float(0.5))].into(),
            textures: Default::default(),
        })];
        let first = create(
            &owner,
            output.name(),
            "top".into(),
            10_000.0,
            effect.clone(),
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let mut scene = vec![element(&renderer, &red, 1.0)];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
        assert!(get(&owner, first).unwrap().state.lock().unwrap().ready);
        let frozen = &overlays.slots[&key].snapshot.as_ref().unwrap();
        assert_eq!(pixel(&mut renderer, frozen), [255, 0, 0, 255]);

        let second = create(
            &owner,
            output.name(),
            "top".into(),
            10_000.0,
            snapshot_effect(),
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let mut scene = vec![element(&renderer, &blue, 1.0)];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
        let rgba = pixel(
            &mut renderer,
            overlays.slots[&key].snapshot.as_ref().unwrap(),
        );
        assert!(
            (126..=129).contains(&rgba[0]) && (126..=129).contains(&rgba[2]),
            "replacement must capture the half-finished transition: {rgba:?}"
        );
        assert!(!get(&owner, first).unwrap().alive());

        effect.pipeline = vec![EffectStage::Shader(ShaderStage {
            shader: ShaderModule {
                path: "/dev/null/invalid.frag".into(),
            },
            uniforms: Default::default(),
            textures: Default::default(),
        })];
        let failed = create(
            &owner,
            output.name(),
            "top".into(),
            10_000.0,
            effect,
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let mut scene = vec![element(&renderer, &blue, 1.0)];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
        assert_eq!(overlays.slots[&key].control.id, second);
        assert!(!get(&owner, failed).unwrap().alive());
        get(&owner, second).unwrap().dispose();
        overlays.tick(&[output.clone()], false);

        let mut live = snapshot_effect();
        live.input = EffectInput::Backdrop;
        let live_id = create(
            &owner,
            output.name(),
            "top".into(),
            10_000.0,
            live.clone(),
            true,
            RuntimeHost::detached(),
        )
        .unwrap();
        for alpha in [1.0, 0.5] {
            let mut scene = vec![element(&renderer, &red, alpha)];
            overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
            let rgba = pixel(
                &mut renderer,
                &overlays.slots[&key].cached.as_ref().unwrap().1,
            );
            assert!(
                (rgba[0] as f32 - alpha * 255.0).abs() <= 1.0,
                "live capture must follow alpha without a new buffer: {rgba:?}"
            );
            assert!(
                overlays.slots[&key]
                    .control
                    .state
                    .lock()
                    .unwrap()
                    .deadline
                    .is_none()
            );
        }
        let cached_id = overlays.slots[&key].cached.as_ref().unwrap().0.clone();
        let mut scene = vec![element(&renderer, &red, 0.5)];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
        assert_eq!(
            overlays.slots[&key].cached.as_ref().unwrap().0,
            cached_id,
            "an idle live source must reuse its result"
        );
        overlays.tick(&[output.clone()], false); // drain the creation wake
        assert!(
            !overlays.tick(&[output.clone()], false),
            "idle persistent effects must not request animation frames"
        );

        let other = Output::new(
            "overlay-gpu-other".into(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "offscreen".into(),
                serial_number: String::new(),
            },
        );
        let other_id = create(
            &owner,
            other.name(),
            "top".into(),
            10_000.0,
            live,
            true,
            RuntimeHost::detached(),
        )
        .unwrap();
        let other_control = get(&owner, other_id).unwrap();
        let live_control = get(&owner, live_id).unwrap();
        let other_key = (other.name(), Placement::Top);
        let mut scene = vec![element(&renderer, &blue, 1.0)];
        overlays.render(&mut renderer, &other, size, scale, &mut scene, 0, |e| e);
        assert_eq!(overlays.slots.len(), 2);
        assert_eq!(
            pixel(
                &mut renderer,
                &overlays.slots[&other_key].cached.as_ref().unwrap().1
            ),
            [0, 0, 255, 255]
        );
        output.change_current_state(None, Some(Transform::_180), None, None);
        overlays.tick(&[output.clone(), other.clone()], false);
        assert!(!live_control.alive());
        assert!(!overlays.slots.contains_key(&key));
        assert!(
            other_control.alive(),
            "changing one output must not disable the other"
        );
        overlays.tick(&[output.clone()], false);
        assert!(!other_control.alive());
        assert!(
            !overlays.slots.contains_key(&other_key),
            "removing an output releases its GPU slot"
        );
        let mut live = snapshot_effect();
        live.input = EffectInput::Backdrop;
        let locked_id = create(
            &owner,
            output.name(),
            "top".into(),
            10_000.0,
            live,
            true,
            RuntimeHost::detached(),
        )
        .unwrap();
        let locked_control = get(&owner, locked_id).unwrap();
        let mut scene = vec![element(&renderer, &red, 1.0)];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 0, |e| e);
        overlays.tick(&[output.clone()], true);
        assert!(!locked_control.alive());
        assert!(
            overlays.slots.is_empty(),
            "locking releases the live GPU slots"
        );
        overlays.tick(&[], false); // drain cleanup damage
        assert!(!overlays.tick(&[], false));
        output.change_current_state(None, Some(Transform::Normal), None, None);
        create(
            &owner,
            output.name(),
            "below-layers".into(),
            10_000.0,
            snapshot_effect(),
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let mut scene = vec![
            element(&renderer, &red, 1.0),
            element(&renderer, &blue, 1.0),
        ];
        overlays.render(&mut renderer, &output, size, scale, &mut scene, 1, |e| e);
        let below_key = (output.name(), Placement::BelowLayers);
        assert_eq!(
            pixel(
                &mut renderer,
                overlays.slots[&below_key].snapshot.as_ref().unwrap()
            ),
            [0, 0, 255, 255]
        );
        let composed = capture(&mut renderer, size, scale, &scene).unwrap();
        assert_eq!(
            pixel(&mut renderer, &composed),
            [255, 0, 0, 255],
            "layer content must stay above a below-layers overlay"
        );
        close_owner(&owner);
        overlays.tick(&[output], false);
        assert!(overlays.slots.is_empty());
    }
    #[tokio::test]
    async fn deadline_rejects_without_a_compositor_tick() {
        let owner = Arc::new(AtomicBool::new(true));
        let effect = CompiledEffect {
            input: EffectInput::Named(SNAPSHOT_NAME.into()),
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![],
            alpha: crate::ssd::EffectAlphaMode::Preserve,
        };
        for persistent in [false, true] {
            let id = create(
                &owner,
                "test-output".into(),
                "top".into(),
                5.0,
                effect.clone(),
                persistent,
                RuntimeHost::detached(),
            )
            .unwrap();
            let control = get(&owner, id).unwrap();
            let result = tokio::time::timeout(Duration::from_secs(1), control.wait(false))
                .await
                .unwrap();
            assert!(result.unwrap_err().to_string().contains("timed out"));
            control.wait(true).await.unwrap();
            controls().lock().unwrap().remove(&id);
        }
    }

    #[tokio::test]
    async fn ready_overlay_lifetimes_and_cleanup() {
        let owner = Arc::new(AtomicBool::new(true));
        let id = create(
            &owner,
            "persistent-output".into(),
            "top".into(),
            50.0,
            snapshot_effect(),
            true,
            RuntimeHost::detached(),
        )
        .unwrap();
        let control = get(&owner, id).unwrap();
        control.mark_ready();
        control.wait(false).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(75), control.wait(true))
                .await
                .is_err()
        );
        assert!(control.alive());
        control.dispose();
        control.wait(true).await.unwrap();
        assert!(!control.alive());
        controls().lock().unwrap().remove(&id);

        let id = create(
            &owner,
            "temporary-output".into(),
            "top".into(),
            5.0,
            snapshot_effect(),
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let control = get(&owner, id).unwrap();
        control.mark_ready();
        tokio::time::timeout(Duration::from_secs(1), control.wait(true))
            .await
            .expect("a ready temporary overlay must still expire")
            .unwrap();
        assert!(!control.alive());
        controls().lock().unwrap().remove(&id);

        let id = create(
            &owner,
            "persistent-output".into(),
            "top".into(),
            50.0,
            snapshot_effect(),
            true,
            RuntimeHost::detached(),
        )
        .unwrap();
        let control = get(&owner, id).unwrap();
        control.mark_ready();
        close_owner(&owner); // runtime teardown/reload uses this path
        control.wait(true).await.unwrap();
        assert!(!control.alive());
        controls().lock().unwrap().remove(&id);
    }

    #[tokio::test]
    async fn runtime_teardown_rejects_capture_and_wakes_closed_waiters() {
        let owner = Arc::new(AtomicBool::new(true));
        let effect = CompiledEffect {
            input: EffectInput::Named(SNAPSHOT_NAME.into()),
            capture_padding: 0,
            invalidate: EffectInvalidationPolicy::Always,
            pipeline: vec![],
            alpha: crate::ssd::EffectAlphaMode::Preserve,
        };
        let id = create(
            &owner,
            "test-output".into(),
            "top".into(),
            10_000.0,
            effect,
            false,
            RuntimeHost::detached(),
        )
        .unwrap();
        let control = get(&owner, id).unwrap();
        assert!(get(&Arc::new(AtomicBool::new(true)), id).is_err());
        let captured = control.wait(false);
        let closed = control.wait(true);
        let cancel = async {
            tokio::task::yield_now().await;
            close_owner(&owner);
        };
        let (captured, closed, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(captured, closed, cancel)
        })
        .await
        .expect("runtime shutdown must wake pending native promises");
        assert!(captured.is_err());
        assert!(closed.is_ok());
        controls().lock().unwrap().remove(&id);
    }
}
