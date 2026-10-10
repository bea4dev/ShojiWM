//! Custom output composition (`COMPOSITOR.rendering.composition`).
//!
//! The config describes how an output is put together as a plan: layer-shell
//! layers, the window stack, textures rendered from nested plans, and 3D scenes
//! that draw those textures. The runtime sends a new plan only when its shape or
//! values change; every frame the backend walks the plan it holds and collects
//! the same render elements it always did, so damage tracking, occlusion and
//! scanout keep working. An output without a custom plan uses [`default_nodes`],
//! the stacking the compositor has always used.
//!
//! Plans list nodes back to front (later nodes draw above earlier ones), the
//! order a config author writes them in. Render element lists are front to
//! back, so the walker iterates nodes in reverse.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ContextId, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{
                Element, Id, Kind, RenderElement, RenderElementPresentationState,
                RenderElementState, RenderElementStates, solid::SolidColorRenderElement,
                texture::TextureRenderElement,
            },
            gles::{GlesError, GlesRenderer, GlesTexture},
            utils::{CommitCounter, DamageBag},
        },
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
};

/// Column-major 4x4 matrix, as WebGL and glMatrix lay them out.
pub type Mat4 = [f32; 16];

pub const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

/// The most textures one plan may declare; plans are small, this only bounds abuse.
const MAX_TEXTURES: usize = 64;
const MAX_NODES: usize = 4096;
/// Texture edge limit in physical pixels.
const MAX_TEXTURE_EDGE: i32 = 16384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayerKind {
    Background,
    Bottom,
    Top,
    Overlay,
}

impl LayerKind {
    pub fn wlr(self) -> smithay::wayland::shell::wlr_layer::Layer {
        use smithay::wayland::shell::wlr_layer::Layer;
        match self {
            LayerKind::Background => Layer::Background,
            LayerKind::Bottom => Layer::Bottom,
            LayerKind::Top => Layer::Top,
            LayerKind::Overlay => Layer::Overlay,
        }
    }

    /// Top and Overlay draw above windows in the default stacking.
    pub fn is_upper(self) -> bool {
        matches!(self, LayerKind::Top | LayerKind::Overlay)
    }
}

/// A rectangle in logical pixels, relative to the composition's origin.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub struct RectF {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// To the composition's right edge when missing.
    #[serde(default)]
    pub width: Option<f64>,
    /// To the composition's bottom edge when missing.
    #[serde(default)]
    pub height: Option<f64>,
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

fn transparent() -> [f32; 4] {
    [0.0; 4]
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum CompositionNode {
    /// Layer-shell surfaces of the output, `layers` back to front.
    Layers { layers: Vec<LayerKind> },
    /// Popups of every layer-shell surface.
    LayerPopups,
    /// The window stack. Without `windows`: the windows the output shows,
    /// together with closing windows and decoration popups. With `windows`:
    /// exactly those windows (by id) in stacking order, also when hidden —
    /// e.g. the windows of another workspace.
    Windows {
        #[serde(default)]
        windows: Option<Vec<String>>,
        #[serde(default)]
        offset_x: i32,
        #[serde(default)]
        offset_y: i32,
    },
    /// A texture drawn flat, by default over the whole composition.
    TextureView {
        texture: usize,
        #[serde(default)]
        rect: Option<RectF>,
        #[serde(default = "one")]
        opacity: f32,
    },
    /// A solid color fill (premultiplied RGBA).
    Solid {
        #[serde(default)]
        rect: Option<RectF>,
        color: [f32; 4],
    },
    /// A 3D scene, rendered offscreen with a depth buffer and drawn as one texture.
    Scene3d(Scene3dSpec),
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene3dSpec {
    /// Where the scene is drawn; the whole composition by default.
    #[serde(default)]
    pub rect: Option<RectF>,
    pub projection: Mat4,
    pub view: Mat4,
    #[serde(default = "transparent")]
    pub clear_color: [f32; 4],
    /// Multisampled edges (4x MSAA where the GPU supports it).
    #[serde(default = "yes")]
    pub antialias: bool,
    pub objects: Vec<Object3d>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Object3d {
    /// A textured rectangle centred on its origin in the XY plane (Y up),
    /// `width` x `height` world units, placed by `model`.
    Plane {
        texture: usize,
        width: f32,
        height: f32,
        #[serde(default = "identity")]
        model: Mat4,
        #[serde(default = "one")]
        opacity: f32,
        #[serde(default = "yes")]
        double_sided: bool,
    },
}

fn identity() -> Mat4 {
    IDENTITY
}

/// A texture the plan renders from its own nested nodes.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureSpec {
    /// Stable identity across plan updates; keeps the GPU texture alive.
    pub key: String,
    /// Logical size; the output's by default.
    #[serde(default)]
    pub width: Option<f64>,
    #[serde(default)]
    pub height: Option<f64>,
    /// Pixel density; the output's by default.
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default = "transparent")]
    pub clear_color: [f32; 4],
    pub nodes: Vec<CompositionNode>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct OutputComposition {
    pub nodes: Vec<CompositionNode>,
    #[serde(default)]
    pub textures: Vec<TextureSpec>,
}

impl OutputComposition {
    /// The stacking every output has without a custom composition.
    pub fn default_plan() -> Arc<OutputComposition> {
        static DEFAULT: std::sync::OnceLock<Arc<OutputComposition>> = std::sync::OnceLock::new();
        DEFAULT
            .get_or_init(|| {
                Arc::new(OutputComposition {
                    nodes: default_nodes(),
                    textures: Vec::new(),
                })
            })
            .clone()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.textures.len() > MAX_TEXTURES {
            return Err(format!("a composition may declare at most {MAX_TEXTURES} textures"));
        }
        let mut count = 0usize;
        self.validate_nodes(&self.nodes, &mut count)?;
        let mut keys = HashSet::new();
        for texture in &self.textures {
            if !keys.insert(texture.key.as_str()) {
                return Err(format!("duplicate texture key {:?}", texture.key));
            }
            for value in [texture.width, texture.height, texture.scale].into_iter().flatten() {
                if !value.is_finite() || value <= 0.0 {
                    return Err(format!("texture {:?} has an invalid size or scale", texture.key));
                }
            }
            self.validate_nodes(&texture.nodes, &mut count)?;
        }
        // Textures may use other textures, but not themselves, directly or not.
        let mut state = vec![0u8; self.textures.len()];
        for index in 0..self.textures.len() {
            self.visit(index, &mut state)?;
        }
        Ok(())
    }

    fn visit(&self, index: usize, state: &mut [u8]) -> Result<(), String> {
        match state[index] {
            1 => {
                return Err(format!(
                    "texture {:?} uses itself (render textures cannot form a cycle)",
                    self.textures[index].key
                ));
            }
            2 => return Ok(()),
            _ => {}
        }
        state[index] = 1;
        let mut dependencies = Vec::new();
        collect_texture_uses(&self.textures[index].nodes, &mut dependencies);
        for dependency in dependencies {
            self.visit(dependency, state)?;
        }
        state[index] = 2;
        Ok(())
    }

    fn validate_nodes(&self, nodes: &[CompositionNode], count: &mut usize) -> Result<(), String> {
        *count += nodes.len();
        if *count > MAX_NODES {
            return Err(format!("a composition may contain at most {MAX_NODES} nodes"));
        }
        let texture = |index: usize| {
            if index < self.textures.len() {
                Ok(())
            } else {
                Err(format!("texture index {index} is out of range"))
            }
        };
        for node in nodes {
            match node {
                CompositionNode::TextureView { texture: index, rect, opacity } => {
                    texture(*index)?;
                    validate_rect(rect.as_ref())?;
                    validate_unit(*opacity, "opacity")?;
                }
                CompositionNode::Solid { rect, color } => {
                    validate_rect(rect.as_ref())?;
                    for channel in color {
                        validate_unit(*channel, "color")?;
                    }
                }
                CompositionNode::Scene3d(scene) => {
                    validate_rect(scene.rect.as_ref())?;
                    validate_matrix(&scene.projection)?;
                    validate_matrix(&scene.view)?;
                    for channel in scene.clear_color {
                        validate_unit(channel, "clearColor")?;
                    }
                    for object in &scene.objects {
                        match object {
                            Object3d::Plane { texture: index, width, height, model, opacity, .. } => {
                                texture(*index)?;
                                validate_matrix(model)?;
                                validate_unit(*opacity, "opacity")?;
                                if !(width.is_finite() && height.is_finite()) {
                                    return Err("plane size must be finite".into());
                                }
                            }
                        }
                    }
                }
                CompositionNode::Layers { .. }
                | CompositionNode::LayerPopups
                | CompositionNode::Windows { .. } => {}
            }
        }
        Ok(())
    }

    /// Whether the plan contains the output's own window stack at its root,
    /// which is what the fullscreen fast path replaces.
    pub fn has_default_windows(&self) -> bool {
        self.nodes
            .iter()
            .any(|node| matches!(node, CompositionNode::Windows { windows: None, .. }))
    }

    /// Ids of every window the plan selects explicitly, anywhere.
    pub fn selected_window_ids(&self) -> HashSet<String> {
        fn collect(nodes: &[CompositionNode], ids: &mut HashSet<String>) {
            for node in nodes {
                if let CompositionNode::Windows { windows: Some(windows), .. } = node {
                    ids.extend(windows.iter().cloned());
                }
            }
        }
        let mut ids = HashSet::new();
        collect(&self.nodes, &mut ids);
        for texture in &self.textures {
            collect(&texture.nodes, &mut ids);
        }
        ids
    }
}

fn validate_rect(rect: Option<&RectF>) -> Result<(), String> {
    if let Some(rect) = rect
        && ![Some(rect.x), Some(rect.y), rect.width, rect.height]
            .into_iter()
            .flatten()
            .all(|value| value.is_finite())
    {
        return Err("rect values must be finite".into());
    }
    Ok(())
}

fn validate_unit(value: f32, name: &str) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be between 0 and 1"))
    }
}

fn validate_matrix(matrix: &Mat4) -> Result<(), String> {
    if matrix.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err("matrices must be finite".into())
    }
}

/// Texture indices `nodes` draw, in order.
pub fn collect_texture_uses(nodes: &[CompositionNode], out: &mut Vec<usize>) {
    for node in nodes {
        match node {
            CompositionNode::TextureView { texture, .. } => out.push(*texture),
            CompositionNode::Scene3d(scene) => {
                for object in &scene.objects {
                    match object {
                        Object3d::Plane { texture, .. } => out.push(*texture),
                    }
                }
            }
            _ => {}
        }
    }
}

/// Background and Bottom layers, the windows, Top and Overlay layers, then
/// layer popups (GTK puts Waybar tooltips there; they must stay above windows
/// even when their bar lives on Bottom).
pub fn default_nodes() -> Vec<CompositionNode> {
    vec![
        CompositionNode::Layers {
            layers: vec![LayerKind::Background, LayerKind::Bottom],
        },
        CompositionNode::Windows {
            windows: None,
            offset_x: 0,
            offset_y: 0,
        },
        CompositionNode::Layers {
            layers: vec![LayerKind::Top, LayerKind::Overlay],
        },
        CompositionNode::LayerPopups,
    ]
}

/// Which windows a `Windows` node draws.
#[derive(Debug, Clone, Copy)]
pub enum WindowSelection<'a> {
    /// The output's own stack, closing windows and decoration popups included.
    Output,
    /// Exactly these window ids.
    Ids(&'a [String]),
    /// Nothing (a layer with no window node below it).
    None,
}

/// Something a plan draws below a node, as backdrop effects there see it.
///
/// Backdrop effects that re-render the scene behind a surface (rather than
/// read the framebuffer) capture these, nearest first. For the default
/// stacking that is what they always captured: the window stack, then the
/// Bottom and Background layers.
#[derive(Debug, Clone)]
pub enum BelowNode<'a> {
    Windows {
        selection: WindowSelection<'a>,
        /// The node's `offsetX`/`offsetY`.
        offset: Point<i32, Logical>,
    },
    /// Front to back.
    Layers(Vec<LayerKind>),
    Composited(Composited),
}

/// A texture view, 3D scene or solid fill a plan drew, kept so a backdrop
/// capture can draw it again.
#[derive(Debug, Clone)]
pub struct Composited {
    /// `None` for a solid fill.
    texture: Option<GlesTexture>,
    color: [f32; 4],
    /// In physical pixels of the composition it was drawn in.
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    /// Changes whenever what it shows, or where, does.
    pub signature: u64,
}

pub enum CompositedElement {
    Texture(TextureRenderElement<GlesTexture>),
    Solid(SolidColorRenderElement),
}

impl Composited {
    /// An element drawing this into a capture whose top-left corner is
    /// `origin`. Captures render from scratch, so the element is new each time.
    pub fn capture_element(
        &self,
        renderer: &GlesRenderer,
        origin: Point<i32, Physical>,
        scale: Scale<f64>,
    ) -> CompositedElement {
        let geometry = Rectangle::new(self.geometry.loc - origin, self.geometry.size);
        match &self.texture {
            Some(texture) => CompositedElement::Texture(texture_element(
                renderer,
                &Id::new(),
                texture,
                &Arc::new(Mutex::new(DamageBag::new(1))),
                geometry,
                scale,
                self.alpha,
            )),
            None => CompositedElement::Solid(SolidColorRenderElement::new(
                Id::new(),
                geometry,
                CommitCounter::default(),
                self.color,
                Kind::Unspecified,
            )),
        }
    }
}

/// A [`BelowNode`] resolved by a backend.
#[derive(Clone)]
pub enum BackdropItem {
    /// Top to bottom, placed as if the composition's origin were
    /// `output_origin` (a window node's offset moves it).
    Windows {
        windows: Vec<smithay::desktop::Window>,
        output_origin: Point<i32, Logical>,
        /// Picked by id: drawn even while the config hides them.
        force_visible: bool,
    },
    /// Front to back. `upper` (Top/Overlay) says which source damage list
    /// reports their changes.
    Layers {
        surfaces: Vec<smithay::desktop::LayerSurface>,
        upper: bool,
    },
    Composited(Composited),
}

/// Resolve `below` for a composition built in `ctx` on `output`.
/// `windows` picks the windows of a selection that reach into an area.
pub fn resolve_below(
    below: &[BelowNode<'_>],
    ctx: &SceneContext,
    output: &smithay::output::Output,
    windows: impl Fn(WindowSelection<'_>, Rectangle<i32, Logical>) -> Vec<smithay::desktop::Window>,
) -> Vec<BackdropItem> {
    let mut items = Vec::new();
    for node in below {
        match node {
            BelowNode::Windows { selection, offset } => {
                let area = Rectangle::new(ctx.output_geo.loc - *offset, ctx.output_geo.size);
                items.push(BackdropItem::Windows {
                    windows: windows(*selection, area),
                    output_origin: area.loc,
                    force_visible: matches!(selection, WindowSelection::Ids(_)),
                });
            }
            BelowNode::Layers(kinds) => {
                for kind in kinds {
                    let surfaces = super::window::layer_surfaces_on(output, &[kind.wlr()]);
                    if !surfaces.is_empty() {
                        items.push(BackdropItem::Layers { surfaces, upper: kind.is_upper() });
                    }
                }
            }
            BelowNode::Composited(composited) => {
                items.push(BackdropItem::Composited(composited.clone()));
            }
        }
    }
    items
}

/// What an xray backdrop samples of `items`: whatever lies below the nearest
/// window stack, or everything when there is none. For the default stacking
/// that is the Bottom and Background layers, as it always was.
pub fn xray_items<'a, 'b>(items: &'b [&'a BackdropItem]) -> &'b [&'a BackdropItem] {
    match items.iter().position(|item| matches!(item, BackdropItem::Windows { .. })) {
        Some(index) => &items[index + 1..],
        None => items,
    }
}

/// Where and how a plan is being built.
#[derive(Debug, Clone)]
pub struct SceneContext {
    /// The composition's area in global logical coordinates. For the output
    /// itself this is the output geometry.
    pub output_geo: Rectangle<i32, Logical>,
    pub scale: Scale<f64>,
    /// The output's own frame, as opposed to a render texture.
    pub primary: bool,
    /// Identifies the render target for per-target caches; the output name
    /// for the output itself.
    pub scope: String,
}

impl SceneContext {
    /// `rect` in physical pixels of this composition, clipped to it.
    fn physical_rect(&self, rect: Option<&RectF>) -> Rectangle<i32, Physical> {
        let (size_w, size_h) = (self.output_geo.size.w as f64, self.output_geo.size.h as f64);
        let (x, y) = rect.map_or((0.0, 0.0), |rect| (rect.x, rect.y));
        let width = rect.and_then(|rect| rect.width).unwrap_or(size_w - x);
        let height = rect.and_then(|rect| rect.height).unwrap_or(size_h - y);
        let left = (x.max(0.0) * self.scale.x).round() as i32;
        let top = (y.max(0.0) * self.scale.y).round() as i32;
        let right = ((x + width).min(size_w) * self.scale.x).round() as i32;
        let bottom = ((y + height).min(size_h) * self.scale.y).round() as i32;
        Rectangle::new((left, top).into(), ((right - left).max(0), (bottom - top).max(0)).into())
    }
}

/// What a backend provides to build plans: the leaf content (layers, windows,
/// popups) and how to wrap the elements the composition itself makes.
pub trait CompositionScene {
    type Element: RenderElement<GlesRenderer>;

    fn renderer(&mut self) -> &mut GlesRenderer;

    /// Front-to-back elements of the layers in `kinds` (front to back).
    /// `below` is what the plan draws under them, nearest first: what their
    /// backdrops sample besides the layers of `kinds` behind them.
    fn layers(
        &mut self,
        ctx: &SceneContext,
        kinds: &[LayerKind],
        below: &[BelowNode<'_>],
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>>;

    fn layer_popups(
        &mut self,
        ctx: &SceneContext,
        overlay_only: bool,
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>>;

    /// `below` is what the plan draws under the windows, nearest first.
    fn windows(
        &mut self,
        ctx: &SceneContext,
        selection: WindowSelection<'_>,
        below: &[BelowNode<'_>],
    ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>>;

    /// Whether the output's fullscreen fast path is active this frame.
    fn fullscreen_active(&self) -> bool {
        false
    }

    /// Damage-only elements for changes the content elements do not carry
    /// themselves (decoration updates), for a render texture.
    fn offscreen_damage(&mut self, _ctx: &SceneContext) -> Vec<Self::Element> {
        Vec::new()
    }

    /// Swap in the per-target effect caches of `scope` while a render texture
    /// builds, so a window shown both on the output and in a texture keeps
    /// separate backdrops. Calls nest; `leave_scope` restores the previous scope.
    fn enter_scope(&mut self, _scope: &str) {}
    fn leave_scope(&mut self, _scope: &str) {}

    fn texture(&self, element: TextureRenderElement<GlesTexture>) -> Self::Element;
    fn solid(&self, element: SolidColorRenderElement) -> Self::Element;
}

/// Result of building one plan.
pub struct BuiltScene<E> {
    /// Front to back.
    pub elements: Vec<E>,
    /// Index where the output's window stack starts (overlays placed
    /// "below layers" go here).
    pub below_layers: usize,
    /// Something is drawn in front of the fullscreen window.
    pub covers_fullscreen: bool,
    pub presentation: CompositionPresentation,
}

#[derive(Default)]
pub struct CompositionPresentation {
    content: HashMap<Id, (usize, RenderElementStates)>,
    snapshots: HashSet<Id>,
}

impl CompositionPresentation {
    /// Only a presented path to the output makes an offscreen source visible.
    /// Merely building a texture also happens for occluded and transparent nodes.
    /// Areas are conservative after partial clipping: scalar render states cannot
    /// locate the remaining pixels inside a sampled texture.
    pub fn presented_states(&self, output: &RenderElementStates) -> RenderElementStates {
        self.expand(output, false)
    }

    pub fn source_states(&self, output: &RenderElementStates) -> RenderElementStates {
        self.expand(output, true)
    }

    pub fn add_snapshot(&mut self, snapshot: &super::snapshot::LiveWindowSnapshot) {
        let size = snapshot.texture.size();
        self.snapshots.insert(snapshot.id.clone());
        self.content.insert(
            snapshot.id.clone(),
            (
                size.w as usize * size.h as usize,
                snapshot.render_states.clone(),
            ),
        );
    }

    fn expand(&self, output: &RenderElementStates, include_snapshots: bool) -> RenderElementStates {
        let mut result = RenderElementStates::default();
        for (id, state) in &output.states {
            if include_snapshots || !self.snapshots.contains(id) {
                self.collect(id, state, &mut result);
            }
        }
        result
    }

    fn collect(&self, id: &Id, parent: &RenderElementState, result: &mut RenderElementStates) {
        if parent.presentation_state == RenderElementPresentationState::Skipped
            || parent.visible_area == 0
        {
            return;
        }
        let Some((area, content)) = self.content.get(id) else {
            return;
        };
        for (child, state) in &content.states {
            if state.presentation_state == RenderElementPresentationState::Skipped
                || state.visible_area == 0
            {
                continue;
            }
            let visible_area = ((parent.visible_area as u128 * state.visible_area as u128)
                / (*area).max(1) as u128)
                .clamp(1, parent.visible_area as u128) as usize;
            let state = RenderElementState {
                visible_area,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
                needs_capture: false,
            };
            if result
                .element_render_state(child.clone())
                .is_some_and(|previous| previous.visible_area >= visible_area)
            {
                continue;
            }
            merge_presented_state(result, child.clone(), state);
            self.collect(child, &state, result);
        }
    }
}

pub fn merge_presented_states(into: &mut RenderElementStates, from: &RenderElementStates) {
    for (id, state) in &from.states {
        merge_presented_state(into, id.clone(), *state);
    }
}

fn merge_presented_state(into: &mut RenderElementStates, id: Id, state: RenderElementState) {
    into.states
        .entry(id)
        .and_modify(|current| {
            if current.presentation_state == RenderElementPresentationState::Skipped {
                *current = state;
            } else if state.presentation_state != RenderElementPresentationState::Skipped {
                current.visible_area = current.visible_area.max(state.visible_area);
            }
        })
        .or_insert(state);
}

/// GPU state that lives across frames for one output's plan.
#[derive(Default)]
pub struct CompositionTargets {
    textures: HashMap<String, TextureTarget>,
    scenes: HashMap<String, super::scene3d::Scene3dTarget>,
    solids: HashMap<String, (Id, [f32; 4], Rectangle<i32, Physical>, CommitCounter)>,
    // Composition sources determine callback eligibility even outside native window geometry.
    // Direct native snapshots do not opt a window into composition output selection.
    presented_sources: RenderElementStates,
    // Output selection must also account for larger native views and their snapshots.
    effective_output_states: RenderElementStates,
}

impl CompositionTargets {
    pub fn presented_sources(&self) -> &RenderElementStates {
        &self.presented_sources
    }

    pub fn effective_output_states(&self) -> &RenderElementStates {
        &self.effective_output_states
    }

    /// Both reports describe the same output frame; replacing only one can retain
    /// callback eligibility from a frame different from the one used for selection.
    pub fn update_presentation(
        &mut self,
        presented_sources: RenderElementStates,
        effective_output_states: RenderElementStates,
    ) {
        debug_assert!(presented_sources.states.keys().all(|id| {
            !presented_sources.element_was_presented(id.clone())
                || effective_output_states.element_was_presented(id.clone())
        }));
        self.presented_sources = presented_sources;
        self.effective_output_states = effective_output_states;
    }

    pub fn clear_presentation(&mut self) {
        self.update_presentation(Default::default(), Default::default());
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty() && self.scenes.is_empty() && self.solids.is_empty()
    }
}

/// One frame's build of a plan.
pub struct Builder<'p> {
    plan: &'p OutputComposition,
    output_name: String,
    /// Textures rendered this frame: index → generation stamp.
    rendered: HashMap<usize, u64>,
    building: HashSet<usize>,
    seen_textures: HashSet<String>,
    seen_scenes: HashSet<String>,
    seen_solids: HashSet<String>,
    presentation: CompositionPresentation,
}

impl<'p> Builder<'p> {
    pub fn new(plan: &'p OutputComposition, output_name: &str) -> Self {
        Self {
            plan,
            output_name: output_name.to_owned(),
            rendered: HashMap::new(),
            building: HashSet::new(),
            seen_textures: HashSet::new(),
            seen_scenes: HashSet::new(),
            seen_solids: HashSet::new(),
            presentation: CompositionPresentation::default(),
        }
    }

    /// Build the output's frame. `targets` keeps GPU state across frames;
    /// anything the plan no longer uses is released afterwards.
    pub fn build<S: CompositionScene>(
        mut self,
        scene: &mut S,
        targets: &mut CompositionTargets,
        ctx: &SceneContext,
    ) -> Result<BuiltScene<S::Element>, Box<dyn std::error::Error>> {
        let plan = self.plan;
        let mut built = self.build_nodes(scene, targets, ctx, &plan.nodes, "root")?;
        targets
            .textures
            .retain(|key, _| self.seen_textures.contains(key));
        targets
            .scenes
            .retain(|key, _| self.seen_scenes.contains(key));
        targets
            .solids
            .retain(|key, _| self.seen_solids.contains(key));
        built.presentation = self.presentation;
        Ok(built)
    }

    fn build_nodes<S: CompositionScene>(
        &mut self,
        scene: &mut S,
        targets: &mut CompositionTargets,
        ctx: &SceneContext,
        nodes: &[CompositionNode],
        path: &str,
    ) -> Result<BuiltScene<S::Element>, Box<dyn std::error::Error>> {
        // Only the output's own stack has a fullscreen fast path. Like the
        // default stacking always did, it hides Top layers and everything
        // below the window stack.
        let fullscreen_stack = (ctx.primary && scene.fullscreen_active())
            .then(|| {
                nodes
                    .iter()
                    .rposition(|node| matches!(node, CompositionNode::Windows { windows: None, .. }))
            })
            .flatten();
        let fullscreen = fullscreen_stack.is_some();
        let first = fullscreen_stack.unwrap_or(0);
        // Back to front first: what the plan draws itself, so the layers and
        // windows above it can sample it as their backdrop.
        let mut made: Vec<Option<(S::Element, Composited)>> =
            (0..nodes.len()).map(|_| None).collect();
        for (index, node) in nodes.iter().enumerate().skip(first) {
            made[index] = self.composite(scene, targets, ctx, node, &format!("{path}/{index}"))?;
        }
        let mut elements = Vec::new();
        let mut below_layers = None;
        let mut covers_fullscreen = false;
        for index in (first..nodes.len()).rev() {
            let before = elements.len();
            match &nodes[index] {
                CompositionNode::Layers { layers } => {
                    let below = below_nodes(&nodes[..index], &made[..index]);
                    // Front to back: the plan lists layers back to front.
                    let mut kinds: Vec<LayerKind> = layers.iter().rev().copied().collect();
                    if fullscreen {
                        kinds.retain(|kind| *kind == LayerKind::Overlay);
                    }
                    // Upper and lower layers come from separate passes; keep
                    // the requested order by building runs of each.
                    let mut runs = Vec::new();
                    let mut start = 0;
                    while start < kinds.len() {
                        let upper = kinds[start].is_upper();
                        let end = kinds[start..]
                            .iter()
                            .position(|kind| kind.is_upper() != upper)
                            .map_or(kinds.len(), |offset| start + offset);
                        runs.push(&kinds[start..end]);
                        start = end;
                    }
                    for (run, kinds) in runs.iter().enumerate() {
                        // A run's backdrop sees the runs behind it first.
                        let mut run_below: Vec<BelowNode<'_>> = runs[run + 1..]
                            .iter()
                            .map(|kinds| BelowNode::Layers(kinds.to_vec()))
                            .collect();
                        run_below.extend(below.iter().cloned());
                        elements.extend(scene.layers(ctx, kinds, &run_below)?);
                    }
                }
                CompositionNode::LayerPopups => {
                    elements.extend(scene.layer_popups(ctx, fullscreen)?);
                }
                CompositionNode::Windows { windows, offset_x, offset_y } => {
                    if below_layers.is_none() {
                        below_layers = Some(elements.len());
                    }
                    let below = below_nodes(&nodes[..index], &made[..index]);
                    let selection = match windows {
                        Some(ids) => WindowSelection::Ids(ids),
                        None => WindowSelection::Output,
                    };
                    let shifted;
                    let window_ctx = if *offset_x != 0 || *offset_y != 0 {
                        shifted = SceneContext {
                            output_geo: Rectangle::new(
                                (ctx.output_geo.loc.x - offset_x, ctx.output_geo.loc.y - offset_y)
                                    .into(),
                                ctx.output_geo.size,
                            ),
                            ..ctx.clone()
                        };
                        &shifted
                    } else {
                        ctx
                    };
                    elements.extend(scene.windows(window_ctx, selection, &below)?);
                }
                CompositionNode::TextureView { .. }
                | CompositionNode::Solid { .. }
                | CompositionNode::Scene3d(_) => {
                    if let Some((element, _)) = made[index].take() {
                        elements.push(element);
                    }
                }
            }
            if fullscreen && below_layers.is_none() && elements.len() > before {
                covers_fullscreen = true;
            }
        }
        Ok(BuiltScene {
            below_layers: below_layers.unwrap_or(0),
            elements,
            covers_fullscreen,
            presentation: CompositionPresentation::default(),
        })
    }

    /// Draw a node the plan composes itself (texture view, solid, 3D scene)
    /// and keep how to draw it again for backdrop captures. `None` for other
    /// nodes and for ones with nothing to show.
    fn composite<S: CompositionScene>(
        &mut self,
        scene: &mut S,
        targets: &mut CompositionTargets,
        ctx: &SceneContext,
        node: &CompositionNode,
        key: &str,
    ) -> Result<Option<(S::Element, Composited)>, Box<dyn std::error::Error>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match node {
            CompositionNode::TextureView {
                texture,
                rect,
                opacity,
            } => {
                if *opacity <= 0.0 || ctx.physical_rect(rect.as_ref()).is_empty() {
                    return Ok(None);
                }
                self.render_texture(scene, targets, ctx, *texture)?;
                let plan = self.plan;
                let spec = &plan.textures[*texture];
                let geometry = ctx.physical_rect(rect.as_ref());
                let Some(target) = targets.textures.get(&spec.key) else {
                    return Ok(None);
                };
                if geometry.is_empty() {
                    return Ok(None);
                }
                let element = target.element(scene.renderer(), geometry, ctx.scale, *opacity);
                (&spec.key, target.generation, rect_key(geometry), opacity.to_bits()).hash(&mut hasher);
                let composited = Composited {
                    texture: Some(target.texture.clone()),
                    color: [0.0; 4],
                    geometry,
                    alpha: *opacity,
                    signature: hasher.finish(),
                };
                Ok(Some((scene.texture(element), composited)))
            }
            CompositionNode::Solid { rect, color } => {
                let key = key.to_owned();
                let geometry = ctx.physical_rect(rect.as_ref());
                let entry = targets
                    .solids
                    .entry(key.clone())
                    .or_insert_with(|| (Id::new(), *color, geometry, CommitCounter::default()));
                if entry.1 != *color || entry.2 != geometry {
                    entry.1 = *color;
                    entry.2 = geometry;
                    entry.3.increment();
                }
                self.seen_solids.insert(key);
                if geometry.is_empty() {
                    return Ok(None);
                }
                let element = SolidColorRenderElement::new(
                    entry.0.clone(),
                    geometry,
                    entry.3,
                    *color,
                    Kind::Unspecified,
                );
                (color.map(f32::to_bits), rect_key(geometry)).hash(&mut hasher);
                let composited = Composited {
                    texture: None,
                    color: *color,
                    geometry,
                    alpha: 1.0,
                    signature: hasher.finish(),
                };
                Ok(Some((scene.solid(element), composited)))
            }
            CompositionNode::Scene3d(spec) => {
                let mut inputs = Vec::new();
                for object in &spec.objects {
                    match object {
                        Object3d::Plane { texture, .. } => {
                            self.render_texture(scene, targets, ctx, *texture)?;
                        }
                    }
                }
                let plan = self.plan;
                for texture in &plan.textures {
                    inputs.push(
                        targets
                            .textures
                            .get(&texture.key)
                            .map(|target| (target.texture.clone(), target.generation)),
                    );
                }
                let key = format!("{}:{key}", self.output_name);
                let geometry = ctx.physical_rect(spec.rect.as_ref());
                self.seen_scenes.insert(key.clone());
                if geometry.is_empty() {
                    return Ok(None);
                }
                let target = match targets.scenes.remove(&key) {
                    Some(target) if target.matches(scene.renderer()) => target,
                    _ => super::scene3d::Scene3dTarget::new(),
                };
                let target = targets.scenes.entry(key.clone()).or_insert(target);
                target.render(scene.renderer(), spec, &inputs, geometry.size)?;
                let (Some(element), Some(texture)) = (
                    target.element(scene.renderer(), geometry, ctx.scale),
                    target.texture().cloned(),
                ) else {
                    return Ok(None);
                };
                let mut states = RenderElementStates::default();
                for object in &spec.objects {
                    let Object3d::Plane { texture, .. } = object;
                    let area = super::scene3d::projected_area(spec, object, geometry.size);
                    if area > 0
                        && let Some(input) = targets.textures.get(&plan.textures[*texture].key)
                    {
                        merge_presented_state(
                            &mut states,
                            input.id.clone(),
                            RenderElementState {
                                visible_area: area,
                                presentation_state: RenderElementPresentationState::Rendering {
                                    reason: None,
                                },
                                needs_capture: false,
                            },
                        );
                    }
                }
                self.presentation.content.insert(
                    element.id().clone(),
                    (geometry.size.w as usize * geometry.size.h as usize, states),
                );
                (&key, target.signature(), rect_key(geometry)).hash(&mut hasher);
                let composited = Composited {
                    texture: Some(texture),
                    color: [0.0; 4],
                    geometry,
                    alpha: 1.0,
                    signature: hasher.finish(),
                };
                Ok(Some((scene.texture(element), composited)))
            }
            CompositionNode::Layers { .. }
            | CompositionNode::LayerPopups
            | CompositionNode::Windows { .. } => Ok(None),
        }
    }

    /// Render texture `index` (and, first, the textures it uses) unless this
    /// frame already did.
    fn render_texture<S: CompositionScene>(
        &mut self,
        scene: &mut S,
        targets: &mut CompositionTargets,
        ctx: &SceneContext,
        index: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.rendered.contains_key(&index) || !self.building.insert(index) {
            return Ok(());
        }
        let plan = self.plan;
        let spec = &plan.textures[index];
        let output_size = ctx.output_geo.size;
        let width = spec.width.map_or(output_size.w, |width| width.round() as i32).max(1);
        let height = spec.height.map_or(output_size.h, |height| height.round() as i32).max(1);
        let scale = spec.scale.map_or(ctx.scale, Scale::from);
        let child = SceneContext {
            output_geo: Rectangle::new(ctx.output_geo.loc, (width, height).into()),
            scale,
            primary: false,
            scope: format!("{}#{}", self.output_name, spec.key),
        };
        let physical: Size<i32, Physical> = (
            ((width as f64 * scale.x).round() as i32).clamp(1, MAX_TEXTURE_EDGE),
            ((height as f64 * scale.y).round() as i32).clamp(1, MAX_TEXTURE_EDGE),
        )
            .into();

        scene.enter_scope(&child.scope);
        let built = self.build_nodes(
            scene,
            targets,
            &child,
            &spec.nodes,
            &format!("texture:{}", spec.key),
        );
        let mut elements = match built {
            Ok(built) => built.elements,
            Err(error) => {
                scene.leave_scope(&child.scope);
                self.building.remove(&index);
                return Err(error);
            }
        };
        elements.splice(0..0, scene.offscreen_damage(&child));
        let existing = targets.textures.remove(&spec.key);
        let result = TextureTarget::render(
            scene.renderer(),
            existing,
            physical,
            scale,
            &elements,
            spec.clear_color,
        );
        drop(elements);
        scene.leave_scope(&child.scope);
        self.building.remove(&index);
        let target = result?;
        self.presentation.content.insert(
            target.id.clone(),
            (
                physical.w as usize * physical.h as usize,
                target.states.clone(),
            ),
        );
        self.rendered.insert(index, target.generation);
        self.seen_textures.insert(spec.key.clone());
        targets.textures.insert(spec.key.clone(), target);
        Ok(())
    }
}

fn rect_key(rect: Rectangle<i32, Physical>) -> (i32, i32, i32, i32) {
    (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h)
}

/// What `nodes` (the nodes below one, back to front) draw, nearest first.
/// `made` holds what [`Builder::composite`] drew for each of them.
fn below_nodes<'a, E>(
    nodes: &'a [CompositionNode],
    made: &[Option<(E, Composited)>],
) -> Vec<BelowNode<'a>> {
    nodes
        .iter()
        .zip(made)
        .rev()
        .filter_map(|(node, made)| match node {
            CompositionNode::Windows { windows, offset_x, offset_y } => Some(BelowNode::Windows {
                selection: match windows {
                    Some(ids) => WindowSelection::Ids(ids),
                    None => WindowSelection::Output,
                },
                offset: (*offset_x, *offset_y).into(),
            }),
            CompositionNode::Layers { layers } => {
                Some(BelowNode::Layers(layers.iter().rev().copied().collect()))
            }
            CompositionNode::TextureView { .. }
            | CompositionNode::Solid { .. }
            | CompositionNode::Scene3d(_) => {
                made.as_ref().map(|(_, composited)| BelowNode::Composited(composited.clone()))
            }
            // Backdrops never sampled popups.
            CompositionNode::LayerPopups => None,
        })
        .collect()
}

/// An offscreen texture a plan renders into, redrawn incrementally.
pub struct TextureTarget {
    context: ContextId<GlesTexture>,
    pub texture: GlesTexture,
    tracker: OutputDamageTracker,
    id: Id,
    damage: Arc<Mutex<DamageBag<i32, Buffer>>>,
    /// Bumped whenever the pixels change.
    pub generation: u64,
    states: RenderElementStates,
}

impl TextureTarget {
    fn render<E: RenderElement<GlesRenderer>>(
        renderer: &mut GlesRenderer,
        existing: Option<TextureTarget>,
        size: Size<i32, Physical>,
        scale: Scale<f64>,
        elements: &[E],
        clear_color: [f32; 4],
    ) -> Result<TextureTarget, GlesError> {
        let reusable = existing.filter(|target| {
            target.context == renderer.context_id()
                && target.texture.size() == (size.w, size.h).into()
                && matches!(
                    target.tracker.mode(),
                    smithay::output::OutputModeSource::Static { scale: tracker_scale, .. }
                        if *tracker_scale == scale
                )
        });
        // The texture keeps what the previous render left in it, so a reused
        // target redraws only the damage since then (age 1).
        let (mut target, age) = match reusable {
            Some(target) => (target, 1),
            None => (
                TextureTarget {
                    context: renderer.context_id(),
                    texture: Offscreen::<GlesTexture>::create_buffer(
                        renderer,
                        Fourcc::Abgr8888,
                        (size.w, size.h).into(),
                    )?,
                    tracker: OutputDamageTracker::new(size, scale, Transform::Normal),
                    id: Id::new(),
                    damage: Arc::new(Mutex::new(DamageBag::new(4))),
                    generation: 0,
                    states: RenderElementStates::default(),
                },
                0,
            ),
        };
        let damage = {
            let mut framebuffer = renderer.bind(&mut target.texture)?;
            let result = target
                .tracker
                .render_output(renderer, &mut framebuffer, age, elements, clear_color)
                .map_err(|_| GlesError::FramebufferBindingError)?;
            target.states = result.states;
            result.damage.cloned()
        };
        let rects: Vec<Rectangle<i32, Buffer>> = damage
            .unwrap_or_default()
            .into_iter()
            .map(|rect| {
                Rectangle::new(
                    (rect.loc.x, rect.loc.y).into(),
                    (rect.size.w, rect.size.h).into(),
                )
            })
            .collect();
        if age == 0 {
            target.damage.lock().unwrap().add([Rectangle::from_size(target.texture.size())]);
            target.generation = target.generation.wrapping_add(1);
        } else if !rects.is_empty() {
            target.damage.lock().unwrap().add(rects);
            target.generation = target.generation.wrapping_add(1);
        }
        Ok(target)
    }

    fn element(
        &self,
        renderer: &GlesRenderer,
        geometry: Rectangle<i32, Physical>,
        scale: Scale<f64>,
        alpha: f32,
    ) -> TextureRenderElement<GlesTexture> {
        texture_element(
            renderer,
            &self.id,
            &self.texture,
            &self.damage,
            geometry,
            scale,
            alpha,
        )
    }
}

/// A texture element that stretches `texture` over `geometry`.
pub(crate) fn texture_element(
    renderer: &GlesRenderer,
    id: &Id,
    texture: &GlesTexture,
    damage: &Arc<Mutex<DamageBag<i32, Buffer>>>,
    geometry: Rectangle<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
) -> TextureRenderElement<GlesTexture> {
    let size = texture.size();
    let logical: Size<i32, Logical> = (
        (geometry.size.w as f64 / scale.x).round().max(1.0) as i32,
        (geometry.size.h as f64 / scale.y).round().max(1.0) as i32,
    )
        .into();
    TextureRenderElement::from_texture_with_damage(
        id.clone(),
        renderer.context_id(),
        Point::<f64, Physical>::from((geometry.loc.x as f64, geometry.loc.y as f64)),
        texture.clone(),
        1,
        Transform::Normal,
        Some(alpha),
        Some(Rectangle::from_size((size.w as f64, size.h as f64).into())),
        Some(logical),
        None,
        damage.lock().unwrap().snapshot(),
        Kind::Unspecified,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_wire_format() {
        let plan: OutputComposition = serde_json::from_str(
            r#"{
                "nodes": [
                    { "kind": "layers", "layers": ["background"] },
                    { "kind": "windows", "windows": ["w1"], "offsetY": -20 },
                    { "kind": "texture-view", "texture": 0, "opacity": 0.5 },
                    { "kind": "scene3d", "projection": [1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1],
                      "view": [1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1],
                      "objects": [{ "kind": "plane", "texture": 0, "width": 10, "height": 5,
                                    "doubleSided": false }] },
                    { "kind": "layer-popups" }
                ],
                "textures": [{ "key": "ws", "nodes": [{ "kind": "windows" }] }]
            }"#,
        )
        .unwrap();
        plan.validate().unwrap();
        assert!(matches!(
            &plan.nodes[1],
            CompositionNode::Windows { windows: Some(ids), offset_y: -20, .. } if ids == &["w1"]
        ));
        match &plan.nodes[3] {
            CompositionNode::Scene3d(scene) => {
                assert!(scene.antialias);
                assert!(matches!(
                    scene.objects[0],
                    Object3d::Plane { double_sided: false, opacity, model, .. }
                        if opacity == 1.0 && model == IDENTITY
                ));
            }
            other => panic!("unexpected node {other:?}"),
        }
        assert_eq!(plan.selected_window_ids(), HashSet::from(["w1".to_owned()]));
    }

    #[test]
    fn rejects_texture_cycles_and_bad_indices() {
        let cyclic = OutputComposition {
            nodes: vec![],
            textures: vec![
                TextureSpec {
                    key: "a".into(),
                    width: None,
                    height: None,
                    scale: None,
                    clear_color: [0.0; 4],
                    nodes: vec![CompositionNode::TextureView { texture: 1, rect: None, opacity: 1.0 }],
                },
                TextureSpec {
                    key: "b".into(),
                    width: None,
                    height: None,
                    scale: None,
                    clear_color: [0.0; 4],
                    nodes: vec![CompositionNode::TextureView { texture: 0, rect: None, opacity: 1.0 }],
                },
            ],
        };
        assert!(cyclic.validate().unwrap_err().contains("cycle"));
        let out_of_range = OutputComposition {
            nodes: vec![CompositionNode::TextureView { texture: 3, rect: None, opacity: 1.0 }],
            textures: vec![],
        };
        assert!(out_of_range.validate().is_err());
    }

    #[test]
    fn default_plan_keeps_the_classic_stacking() {
        let plan = OutputComposition::default_plan();
        plan.validate().unwrap();
        assert!(plan.has_default_windows());
        assert!(plan.selected_window_ids().is_empty());
    }

    /// Records what the walker asks for; each "element" is a solid tagged by
    /// what produced it (its id is unused).
    struct FakeScene {
        fullscreen: bool,
        log: Vec<String>,
    }

    impl FakeScene {
        fn tag(&mut self, tag: String) -> Vec<SolidColorRenderElement> {
            self.log.push(tag);
            vec![SolidColorRenderElement::new(
                Id::new(),
                Rectangle::from_size((1, 1).into()),
                CommitCounter::default(),
                [0.0; 4],
                Kind::Unspecified,
            )]
        }
    }

    impl CompositionScene for FakeScene {
        type Element = SolidColorRenderElement;

        fn renderer(&mut self) -> &mut GlesRenderer {
            unreachable!("plans without textures never render")
        }

        fn layers(
            &mut self,
            _ctx: &SceneContext,
            kinds: &[LayerKind],
            below: &[BelowNode<'_>],
        ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
            Ok(self.tag(format!("layers{kinds:?} over {}", describe(below))))
        }

        fn layer_popups(
            &mut self,
            _ctx: &SceneContext,
            overlay_only: bool,
        ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
            Ok(self.tag(format!("popups overlay_only={overlay_only}")))
        }

        fn windows(
            &mut self,
            _ctx: &SceneContext,
            selection: WindowSelection<'_>,
            below: &[BelowNode<'_>],
        ) -> Result<Vec<Self::Element>, Box<dyn std::error::Error>> {
            Ok(self.tag(format!("windows {selection:?} over {}", describe(below))))
        }

        fn fullscreen_active(&self) -> bool {
            self.fullscreen
        }

        fn texture(&self, _: TextureRenderElement<GlesTexture>) -> Self::Element {
            unreachable!()
        }

        fn solid(&self, element: SolidColorRenderElement) -> Self::Element {
            element
        }
    }

    fn describe(below: &[BelowNode<'_>]) -> String {
        let parts: Vec<String> = below
            .iter()
            .map(|node| match node {
                BelowNode::Windows { selection, .. } => format!("{selection:?}"),
                BelowNode::Layers(kinds) => format!("{kinds:?}"),
                BelowNode::Composited(composited) => match composited.texture {
                    Some(_) => "texture".into(),
                    None => format!("solid{:?}", composited.color),
                },
            })
            .collect();
        format!("[{}]", parts.join(", "))
    }

    fn walk(plan: &OutputComposition, fullscreen: bool) -> (Vec<String>, usize, bool) {
        let mut scene = FakeScene { fullscreen, log: Vec::new() };
        let ctx = SceneContext {
            output_geo: Rectangle::from_size((100, 100).into()),
            scale: Scale::from(1.0),
            primary: true,
            scope: "out".into(),
        };
        let built = Builder::new(plan, "out")
            .build(&mut scene, &mut CompositionTargets::default(), &ctx)
            .unwrap();
        (scene.log, built.below_layers, built.covers_fullscreen)
    }

    #[test]
    fn default_plan_builds_front_to_back_like_the_classic_stack() {
        let (log, below_layers, _) = walk(&OutputComposition::default_plan(), false);
        assert_eq!(
            log,
            [
                "popups overlay_only=false",
                // What their backdrops always sampled.
                "layers[Overlay, Top] over [Output, [Bottom, Background]]",
                "windows Output over [[Bottom, Background]]",
                "layers[Bottom, Background] over []",
            ]
        );
        // Overlays placed below layers go right above the window stack.
        assert_eq!(below_layers, 2);
    }

    #[test]
    fn fullscreen_keeps_overlay_and_drops_everything_below_the_windows() {
        let (log, below_layers, covers) = walk(&OutputComposition::default_plan(), true);
        assert_eq!(
            log,
            [
                "popups overlay_only=true",
                "layers[Overlay] over [Output, [Bottom, Background]]",
                "windows Output over [[Bottom, Background]]",
            ]
        );
        assert_eq!(below_layers, 2);
        assert!(covers, "anything in front of the fullscreen window forces compositing");
    }

    #[test]
    fn custom_plans_keep_their_order_and_split_mixed_layer_runs() {
        let plan = OutputComposition {
            nodes: vec![
                CompositionNode::Windows { windows: Some(vec!["a".into()]), offset_x: 0, offset_y: 0 },
                CompositionNode::Layers {
                    layers: vec![LayerKind::Background, LayerKind::Top, LayerKind::Bottom],
                },
                CompositionNode::Solid { rect: None, color: [0.0, 0.0, 0.0, 1.0] },
            ],
            textures: vec![],
        };
        // No output window stack: the fullscreen fast path never applies.
        let (log, below_layers, covers) = walk(&plan, true);
        assert_eq!(
            log,
            [
                "layers[Bottom] over [[Top], [Background], Ids([\"a\"])]",
                "layers[Top] over [[Background], Ids([\"a\"])]",
                "layers[Background] over [Ids([\"a\"])]",
                "windows Ids([\"a\"]) over []",
            ]
        );
        assert_eq!(below_layers, 4, "the solid and three layer runs are in front");
        assert!(!covers);
    }

    #[test]
    fn backdrops_see_what_the_plan_composes_below_them() {
        let plan = OutputComposition {
            nodes: vec![
                CompositionNode::Solid { rect: None, color: [0.0, 0.0, 0.0, 1.0] },
                CompositionNode::Windows { windows: Some(vec!["a".into()]), offset_x: 0, offset_y: 0 },
                CompositionNode::Solid { rect: None, color: [1.0, 0.0, 0.0, 1.0] },
                CompositionNode::Layers { layers: vec![LayerKind::Top] },
            ],
            textures: vec![],
        };
        let (log, _, _) = walk(&plan, false);
        assert_eq!(
            log,
            [
                "layers[Top] over [solid[1.0, 0.0, 0.0, 1.0], Ids([\"a\"]), solid[0.0, 0.0, 0.0, 1.0]]",
                "windows Ids([\"a\"]) over [solid[0.0, 0.0, 0.0, 1.0]]",
            ]
        );
    }
}

#[cfg(test)]
#[path = "composition_presentation_tests.rs"]
mod presentation_tests;
