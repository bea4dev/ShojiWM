//! Renders decoration paint items (`ssd::paint`) as pixel-shader elements.
//!
//! Built-in programs (background/border, box-shadow) and user `paint` /
//! `overlay` shaders share one contract: a `vec4 paint_main(PaintContext)`
//! function wrapped by `paint_prelude.glsl`. The geometry is the layout's
//! root-local physical pixel grid; this module only translates it onto the
//! output, so what the shader sees is exactly what the layout produced.

use std::{collections::HashMap, sync::Mutex};

use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType},
        utils::{CommitCounter, OpaqueRegions},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform, user_data::UserDataMap},
};

use crate::{
    backend::shader_effect::{
        ShaderEffectError, append_shader_uniform_names, append_shader_uniform_values,
        cached_shader_is_outdated, compile_shader_or_fallback,
    },
    ssd::{
        Color, WindowDecorationState,
        paint::{PaintItem, PaintProgram, PxClip, PxRect},
        round_half_up,
    },
};

const PRELUDE: &str = include_str!("paint_prelude.glsl");
const BUILTIN_BOX: &str = include_str!("paint_builtin_box.frag");
const BUILTIN_SHADOW: &str = include_str!("paint_builtin_shadow.frag");
/// Stand-in for a user paint shader that fails to build: the built-in look.
const FALLBACK_PAINT: &str = "vec4 paint_main(PaintContext ctx) {\n    return shoji_default_paint(ctx);\n}\n";

pub(crate) fn wrap_paint_source(source: &str) -> String {
    format!(
        "{PRELUDE}\n{source}\n\nvoid main() {{\n    PaintContext ctx = shoji_make_paint_context();\n    vec4 color = paint_main(ctx);\n    gl_FragColor = color * (shoji_opacity * alpha * shoji_clip_coverage(ctx.frag_phy_px));\n}}\n"
    )
}

/// The uniforms every paint program declares (and every element sets).
fn builtin_uniform_names() -> Vec<UniformName<'static>> {
    let vec4 = [
        "shoji_node_rect",
        "shoji_border",
        "shoji_radius",
        "shoji_padding",
        "shoji_content_rect",
        "shoji_inner_rect",
        "shoji_inner_radius",
        "shoji_background",
        "shoji_border_color_top",
        "shoji_border_color_right",
        "shoji_border_color_bottom",
        "shoji_border_color_left",
        "shoji_clip_rect",
        "shoji_clip_radius",
        "shoji_rounded_clip_rect",
        "shoji_rounded_clip_radius",
        "shoji_shadow_color",
    ];
    let float = [
        "shoji_hole",
        "shoji_scale",
        "shoji_opacity",
        "shoji_clip_enabled",
        "shoji_rounded_clip_enabled",
        "shoji_box_fill",
        "shoji_box_border",
        "shoji_shadow_sigma",
        "shoji_shadow_spread",
        "shoji_shadow_inset",
    ];
    vec4.into_iter()
        .map(|name| UniformName::new(name, UniformType::_4f))
        .chain(float.into_iter().map(|name| UniformName::new(name, UniformType::_1f)))
        .chain(std::iter::once(UniformName::new(
            "shoji_shadow_offset",
            UniformType::_2f,
        )))
        .collect()
}

#[derive(Default)]
struct PaintProgramCache(Mutex<HashMap<String, GlesPixelProgram>>);

fn cached_program(renderer: &mut GlesRenderer, key: &str) -> Option<GlesPixelProgram> {
    renderer
        .egl_context()
        .user_data()
        .get::<PaintProgramCache>()?
        .0
        .lock()
        .unwrap()
        .get(key)
        .cloned()
}

fn store_program(renderer: &mut GlesRenderer, key: String, program: GlesPixelProgram) {
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(PaintProgramCache::default);
    renderer
        .egl_context()
        .user_data()
        .get::<PaintProgramCache>()
        .expect("paint program cache was just inserted")
        .0
        .lock()
        .unwrap()
        .insert(key, program);
}

fn program_for(
    renderer: &mut GlesRenderer,
    item: &PaintItem,
) -> Result<GlesPixelProgram, ShaderEffectError> {
    match &item.program {
        PaintProgram::Box { .. } => builtin_program(renderer, "box", BUILTIN_BOX),
        PaintProgram::Shadow(_) => builtin_program(renderer, "shadow", BUILTIN_SHADOW),
        PaintProgram::Custom(module) => {
            let mut key = format!("paint-v1:custom:{}", module.path);
            for (name, value) in &item.uniforms {
                key.push_str(":uniform:");
                key.push_str(name);
                key.push(':');
                key.push_str(&value.shape_key());
            }
            if cached_shader_is_outdated(&key, &module.path)
                && let Some(cache) = renderer.egl_context().user_data().get::<PaintProgramCache>()
            {
                cache.0.lock().unwrap().remove(&key);
            }
            if let Some(program) = cached_program(renderer, &key) {
                return Ok(program);
            }
            let mut names = builtin_uniform_names();
            for (name, value) in &item.uniforms {
                append_shader_uniform_names(&mut names, name, value);
            }
            let program = compile_shader_or_fallback(
                renderer,
                &key,
                &module.path,
                FALLBACK_PAINT,
                wrap_paint_source,
                |renderer, source| {
                    Ok(renderer.compile_custom_pixel_shader(wrap_paint_source(source), &names)?)
                },
            )?;
            store_program(renderer, key, program.clone());
            Ok(program)
        }
    }
}

fn builtin_program(
    renderer: &mut GlesRenderer,
    name: &str,
    source: &str,
) -> Result<GlesPixelProgram, ShaderEffectError> {
    let key = format!("paint-v1:builtin:{name}");
    if let Some(program) = cached_program(renderer, &key) {
        return Ok(program);
    }
    let program =
        renderer.compile_custom_pixel_shader(wrap_paint_source(source), &builtin_uniform_names())?;
    store_program(renderer, key, program.clone());
    Ok(program)
}

fn premultiplied(color: Color) -> [f32; 4] {
    let alpha = color.a as f32 / 255.0;
    [
        color.r as f32 / 255.0 * alpha,
        color.g as f32 / 255.0 * alpha,
        color.b as f32 / 255.0 * alpha,
        alpha,
    ]
}

fn rect_relative(rect: PxRect, origin: PxRect) -> [f32; 4] {
    [
        (rect.x - origin.x) as f32,
        (rect.y - origin.y) as f32,
        rect.w as f32,
        rect.h as f32,
    ]
}

fn edges(values: [i32; 4]) -> [f32; 4] {
    values.map(|value| value as f32)
}

fn clip_uniforms(
    uniforms: &mut Vec<Uniform<'static>>,
    prefix: &str,
    clip: Option<PxClip>,
    node: PxRect,
) {
    uniforms.push(Uniform::new(
        format!("{prefix}_enabled"),
        if clip.is_some() { 1.0f32 } else { 0.0 },
    ));
    let clip = clip.unwrap_or(PxClip {
        rect: PxRect::default(),
        radius: [0; 4],
    });
    uniforms.push(Uniform::new(format!("{prefix}_rect"), rect_relative(clip.rect, node)));
    uniforms.push(Uniform::new(format!("{prefix}_radius"), edges(clip.radius)));
}

fn uniforms_for_item(item: &PaintItem) -> Vec<Uniform<'static>> {
    let g = &item.geometry;
    let mut uniforms = vec![
        Uniform::new("shoji_node_rect", rect_relative(g.node, g.draw)),
        Uniform::new("shoji_border", edges(g.border)),
        Uniform::new("shoji_radius", edges(g.radius)),
        Uniform::new("shoji_padding", edges(g.padding)),
        Uniform::new("shoji_content_rect", rect_relative(g.content, g.node)),
        Uniform::new("shoji_inner_rect", rect_relative(g.inner, g.node)),
        Uniform::new("shoji_inner_radius", edges(g.inner_radius)),
        Uniform::new("shoji_hole", if g.hole { 1.0f32 } else { 0.0 }),
        Uniform::new("shoji_scale", g.scale as f32),
        Uniform::new("shoji_background", premultiplied(item.background)),
        Uniform::new("shoji_border_color_top", premultiplied(item.border_colors[0])),
        Uniform::new("shoji_border_color_right", premultiplied(item.border_colors[1])),
        Uniform::new("shoji_border_color_bottom", premultiplied(item.border_colors[2])),
        Uniform::new("shoji_border_color_left", premultiplied(item.border_colors[3])),
        Uniform::new("shoji_opacity", item.opacity),
    ];
    clip_uniforms(&mut uniforms, "shoji_clip", g.clip, g.node);
    clip_uniforms(&mut uniforms, "shoji_rounded_clip", g.rounded_clip, g.node);

    let (fill, border) = match item.program {
        PaintProgram::Box { fill, border } => (fill, border),
        _ => (false, false),
    };
    uniforms.push(Uniform::new("shoji_box_fill", if fill { 1.0f32 } else { 0.0 }));
    uniforms.push(Uniform::new("shoji_box_border", if border { 1.0f32 } else { 0.0 }));

    let shadow = match item.program {
        PaintProgram::Shadow(shadow) => Some(shadow),
        _ => None,
    };
    uniforms.push(Uniform::new(
        "shoji_shadow_color",
        shadow.map(|shadow| premultiplied(shadow.color)).unwrap_or([0.0; 4]),
    ));
    uniforms.push(Uniform::new(
        "shoji_shadow_offset",
        shadow
            .map(|shadow| [shadow.offset[0] as f32, shadow.offset[1] as f32])
            .unwrap_or([0.0; 2]),
    ));
    uniforms.push(Uniform::new(
        "shoji_shadow_sigma",
        shadow.map(|shadow| shadow.sigma).unwrap_or(0.0),
    ));
    uniforms.push(Uniform::new(
        "shoji_shadow_spread",
        shadow.map(|shadow| shadow.spread as f32).unwrap_or(0.0),
    ));
    uniforms.push(Uniform::new(
        "shoji_shadow_inset",
        if shadow.is_some_and(|shadow| shadow.inset) { 1.0f32 } else { 0.0 },
    ));

    if item.is_custom() {
        for (name, value) in &item.uniforms {
            append_shader_uniform_values(&mut uniforms, name, value);
        }
    }
    uniforms
}

/// Maps root-local layout pixels to physical pixels relative to the root's
/// logical origin on the output (the frame every decoration element uses).
#[derive(Debug, Clone, Copy)]
pub(crate) struct LayoutToOutput {
    ratio: f64,
    offset: (i32, i32),
}

impl LayoutToOutput {
    pub(crate) fn new(decoration: &WindowDecorationState, output_scale: Scale<f64>) -> Self {
        Self::for_layout(&decoration.layout, output_scale)
    }

    pub(crate) fn for_layout(
        layout: &crate::ssd::ComputedDecorationTree,
        output_scale: Scale<f64>,
    ) -> Self {
        let frame = layout.root.frame;
        let root = layout.root.rect;
        let output_scale = output_scale.x.abs().max(0.0001);
        let ratio = output_scale / frame.scale.max(0.0001);
        Self {
            ratio: if (ratio - 1.0).abs() < 1e-9 { 1.0 } else { ratio },
            offset: (
                round_half_up((frame.origin_x - root.x as f64) * output_scale),
                round_half_up((frame.origin_y - root.y as f64) * output_scale),
            ),
        }
    }

    fn map(self, value: i32) -> i32 {
        if self.ratio == 1.0 {
            value
        } else {
            round_half_up(value as f64 * self.ratio)
        }
    }

    pub(crate) fn rect(self, rect: PxRect) -> Rectangle<i32, Physical> {
        let left = self.map(rect.x);
        let top = self.map(rect.y);
        let right = self.map(rect.right());
        let bottom = self.map(rect.bottom());
        Rectangle::new(
            Point::from((left + self.offset.0, top + self.offset.1)),
            ((right - left).max(0), (bottom - top).max(0)).into(),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PaintSpec {
    item: PaintItem,
    geometry: Rectangle<i32, Physical>,
    alpha_bits: u32,
}

#[derive(Debug, Clone)]
pub struct PaintElementState {
    id: Id,
    commit_counter: CommitCounter,
    last_spec: Option<PaintSpec>,
}

impl Default for PaintElementState {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit_counter: CommitCounter::default(),
            last_spec: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct StablePaintElement {
    program: GlesPixelProgram,
    id: Id,
    commit_counter: CommitCounter,
    area: Size<i32, Logical>,
    geometry: Rectangle<i32, Physical>,
    alpha: f32,
    uniforms: Vec<Uniform<'static>>,
    /// Where the program outputs nothing, in `area` pixels.
    transparent_interior: Option<PxRect>,
}

impl PaintElementState {
    fn element(
        &mut self,
        renderer: &mut GlesRenderer,
        item: &PaintItem,
        geometry: Rectangle<i32, Physical>,
        alpha: f32,
    ) -> Result<StablePaintElement, ShaderEffectError> {
        let spec = PaintSpec {
            item: item.clone(),
            geometry,
            alpha_bits: alpha.to_bits(),
        };
        if self.last_spec.as_ref() != Some(&spec) {
            self.commit_counter.increment();
            self.last_spec = Some(spec);
        }
        Ok(StablePaintElement {
            program: program_for(renderer, item)?,
            id: self.id.clone(),
            commit_counter: self.commit_counter,
            // The shader works in layout pixels, so its area is the drawn
            // rect at layout scale even when the output scale differs.
            area: (item.geometry.draw.w.max(1), item.geometry.draw.h.max(1)).into(),
            geometry,
            alpha: alpha.clamp(0.0, 1.0),
            uniforms: uniforms_for_item(item),
            transparent_interior: item.transparent_interior(),
        })
    }
}

/// The element of one paint item, positioned relative to the root's physical
/// origin (`to_output`). `None` when it is off the output or empty.
pub(crate) fn paint_element(
    renderer: &mut GlesRenderer,
    to_output: LayoutToOutput,
    paint_cache: &mut HashMap<String, PaintElementState>,
    cached: &crate::ssd::CachedDecorationBuffer,
    output_geo: Rectangle<i32, Logical>,
    alpha: f32,
) -> Result<Option<StablePaintElement>, ShaderEffectError> {
    let item = &cached.paint;
    if item.geometry.draw.is_empty() || item.opacity <= 0.0 {
        return Ok(None);
    }
    let rect = cached.rect;
    let visible = Rectangle::<i32, Logical>::new((rect.x, rect.y).into(), (rect.width, rect.height).into())
        .overlaps(output_geo);
    if !visible {
        return Ok(None);
    }
    let geometry = to_output.rect(item.geometry.draw);
    if geometry.size.w <= 0 || geometry.size.h <= 0 {
        return Ok(None);
    }
    let state = match paint_cache.get_mut(&cached.stable_key) {
        Some(state) => state,
        None => paint_cache.entry(cached.stable_key.clone()).or_default(),
    };
    state.element(renderer, item, geometry, alpha).map(Some)
}

impl Element for StablePaintElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit_counter
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.area.to_f64().to_buffer(1.0, Transform::Normal))
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.geometry
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.alpha
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl StablePaintElement {
    /// The transparent interior in `dst`-local pixels, mapped through the
    /// `src` -> `dst` scaling (wrappers may crop or rescale the element) and
    /// shrunk to whole pixels so the skipped area never holds a painted one.
    fn interior_in(
        &self,
        src: Rectangle<f64, Buffer>,
        dst_size: Size<i32, Physical>,
    ) -> Option<Rectangle<i32, Physical>> {
        let interior = self.transparent_interior?;
        if src.size.w <= 0.0 || src.size.h <= 0.0 {
            return None;
        }
        let sx = dst_size.w as f64 / src.size.w;
        let sy = dst_size.h as f64 / src.size.h;
        let left = ((interior.x as f64 - src.loc.x) * sx).ceil() as i32;
        let top = ((interior.y as f64 - src.loc.y) * sy).ceil() as i32;
        let right = ((interior.right() as f64 - src.loc.x) * sx).floor() as i32;
        let bottom = ((interior.bottom() as f64 - src.loc.y) * sy).floor() as i32;
        (right > left && bottom > top)
            .then(|| Rectangle::new((left, top).into(), (right - left, bottom - top).into()))
    }
}

impl RenderElement<GlesRenderer> for StablePaintElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        let rim_damage;
        let damage = match self.interior_in(src, dst.size) {
            Some(interior) => {
                rim_damage = damage
                    .iter()
                    .flat_map(|rect| rect.subtract_rect(interior))
                    .collect::<Vec<_>>();
                &rim_damage[..]
            }
            None => damage,
        };
        if damage.is_empty() {
            return Ok(());
        }
        frame.render_pixel_shader_to(
            &self.program,
            src,
            dst,
            self.area.to_buffer(1, Transform::Normal),
            Some(damage),
            self.alpha,
            &self.uniforms,
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// GPU readback probes: render paint elements offscreen and check the exact
/// pixels. Skipped when no render node is available.
#[cfg(test)]
mod tests {
    use smithay::backend::allocator::Fourcc;
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::backend::renderer::element::utils::CropRenderElement;
    use smithay::backend::renderer::gles::GlesRenderbuffer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};

    use super::*;
    use crate::ssd::{
        BorderStyle, BoxNode, BoxShadow, DecorationNode, DecorationNodeKind, DecorationStyle,
        DecorationTree, LayoutDirection, LogicalRect, PaintShader, ShaderModule,
        ShaderUniformValue, paint_buffers_for_layout,
    };

    const OUT: i32 = 160;

    fn try_renderer() -> Option<GlesRenderer> {
        let gbm = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/dri/renderD128")
            .ok()
            .and_then(|fd| smithay::backend::allocator::gbm::GbmDevice::new(fd).ok())?;
        let egl = unsafe { EGLDisplay::new(gbm).ok()? };
        let ctx = EGLContext::new(&egl).ok()?;
        unsafe { GlesRenderer::new(ctx).ok() }
    }

    struct Pixels(Vec<u8>);

    impl Pixels {
        fn at(&self, x: i32, y: i32) -> [u8; 4] {
            let offset = ((y * OUT + x) * 4) as usize;
            [self.0[offset], self.0[offset + 1], self.0[offset + 2], self.0[offset + 3]]
        }
    }

    /// Lays `root` out and renders every paint item with the root's
    /// top-left at physical (20, 20), on a transparent black target.
    fn render(root: DecorationNode, scale: f64) -> Option<Pixels> {
        render_with(root, scale, 1, true, false)
    }

    /// `stretch` draws every element at that multiple of its layout size;
    /// `skip_interior: false` paints the transparent interiors too.
    fn render_with(
        root: DecorationNode,
        scale: f64,
        stretch: i32,
        skip_interior: bool,
        crop: bool,
    ) -> Option<Pixels> {
        let mut renderer = try_renderer()?;
        let tree = DecorationTree::new(root);
        let layout = tree
            .layout_for_client_with_scale(LogicalRect::new(20, 20, 1, 1), scale)
            .expect("layout");
        let elements = paint_buffers_for_layout(&layout)
            .into_iter()
            .map(|buffer| {
                let draw = buffer.paint.geometry.draw;
                let geometry = Rectangle::new(
                    Point::from(((draw.x + 20) * stretch, (draw.y + 20) * stretch)),
                    (draw.w * stretch, draw.h * stretch).into(),
                );
                let mut element = PaintElementState::default()
                    .element(&mut renderer, &buffer.paint, geometry, 1.0)
                    .expect("paint element");
                if !skip_interior {
                    element.transparent_interior = None;
                }
                element
            })
            .collect::<Vec<_>>();
        if crop {
            // Wrappers hand `draw` a sub-rect of the source: keep the
            // middle of the output only.
            let elements = elements
                .into_iter()
                .filter_map(|element| {
                    CropRenderElement::from_element(
                        element,
                        1.0,
                        Rectangle::new((37, 41).into(), (70, 66).into()),
                    )
                })
                .collect::<Vec<_>>();
            return draw_pixels(&mut renderer, &elements);
        }
        draw_pixels(&mut renderer, &elements)
    }

    fn draw_pixels<E: RenderElement<GlesRenderer>>(
        renderer: &mut GlesRenderer,
        elements: &[E],
    ) -> Option<Pixels> {
        let size = Size::<i32, Physical>::from((OUT, OUT));
        let mut buffer: GlesRenderbuffer = renderer
            .create_buffer(Fourcc::Abgr8888, size.to_logical(1).to_buffer(1, Transform::Normal))
            .ok()?;
        let mut fb = renderer.bind(&mut buffer).ok()?;
        let mut tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
        tracker
            .render_output(renderer, &mut fb, 0, elements, Color32F::new(0.0, 0.0, 0.0, 0.0))
            .ok()?;
        let mapping = renderer
            .copy_framebuffer(
                &fb,
                Rectangle::from_size(size.to_logical(1).to_buffer(1, Transform::Normal)),
                Fourcc::Abgr8888,
            )
            .ok()?;
        Some(Pixels(renderer.map_texture(&mapping).ok()?.to_vec()))
    }

    /// A root holding the styled box at its top-left, followed by the
    /// (empty) client slot.
    fn boxed(style: DecorationStyle) -> DecorationNode {
        let mut slot = DecorationNode::new(DecorationNodeKind::WindowSlot);
        slot.style = DecorationStyle {
            width: Some(0.0),
            height: Some(0.0),
            ..Default::default()
        };
        let mut node = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }));
        node.style = DecorationStyle {
            flex_shrink: Some(0.0),
            ..style
        };
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }));
        root.children = vec![node, slot];
        root
    }

    const RED: Color = Color::rgba(255, 0, 0, 255);
    const BLUE: Color = Color::rgba(0, 0, 255, 255);

    #[test]
    fn builtin_border_lands_on_whole_physical_pixels() {
        // 1 logical px at 1.5x rounds to a 2px border; 30x20 logical -> 45x30 px.
        let Some(pixels) = render(
            boxed(DecorationStyle {
                width: Some(30.0),
                height: Some(20.0),
                background: Some(BLUE),
                border: Some(BorderStyle {
                    width: 1.0,
                    color: RED,
                }),
                ..Default::default()
            }),
            1.5,
        ) else {
            eprintln!("no render node; skipping");
            return;
        };
        let (x0, y0) = (20, 20);
        assert_eq!(pixels.at(x0 - 1, y0 + 10), [0, 0, 0, 0], "outside");
        assert_eq!(pixels.at(x0, y0 + 10), [255, 0, 0, 255], "border column 0");
        assert_eq!(pixels.at(x0 + 1, y0 + 10), [255, 0, 0, 255], "border column 1");
        assert_eq!(pixels.at(x0 + 2, y0 + 10), [0, 0, 255, 255], "background");
        assert_eq!(pixels.at(x0 + 44, y0 + 10), [255, 0, 0, 255], "right border");
        assert_eq!(pixels.at(x0 + 45, y0 + 10), [0, 0, 0, 0], "past the right edge");
        assert_eq!(pixels.at(x0 + 10, y0 + 29), [255, 0, 0, 255], "bottom border");
        assert_eq!(pixels.at(x0 + 10, y0 + 30), [0, 0, 0, 0], "past the bottom edge");
    }

    #[test]
    fn per_side_borders_use_their_own_width_and_color() {
        let green = Color::rgba(0, 255, 0, 255);
        let Some(pixels) = render(
            boxed(DecorationStyle {
                width: Some(40.0),
                height: Some(20.0),
                background: Some(BLUE),
                border: Some(BorderStyle {
                    width: 1.0,
                    color: RED,
                }),
                border_left: Some(BorderStyle {
                    width: 4.0,
                    color: green,
                }),
                ..Default::default()
            }),
            1.0,
        ) else {
            return;
        };
        assert_eq!(pixels.at(23, 30), [0, 255, 0, 255], "left side is 4px green");
        assert_eq!(pixels.at(24, 30), [0, 0, 255, 255]);
        assert_eq!(pixels.at(59, 30), [255, 0, 0, 255], "right side is 1px red");
        assert_eq!(pixels.at(58, 30), [0, 0, 255, 255]);
    }

    #[test]
    fn rounded_corners_are_cut_and_the_outer_shadow_stays_outside() {
        let Some(pixels) = render(
            boxed(DecorationStyle {
                width: Some(40.0),
                height: Some(40.0),
                background: Some(BLUE),
                border_radius: Some(10.0),
                box_shadow: vec![BoxShadow {
                    offset_x: 0.0,
                    offset_y: 0.0,
                    blur: 0.0,
                    spread: 4.0,
                    color: RED,
                    inset: false,
                }],
                ..Default::default()
            }),
            1.0,
        ) else {
            return;
        };
        assert_eq!(pixels.at(40, 40), [0, 0, 255, 255], "inside");
        // The very corner pixel of the box lies outside the 10px rounding,
        // so the (spread) shadow shows through there.
        assert_eq!(pixels.at(20, 20)[2], 0, "box corner is cut");
        assert_eq!(pixels.at(17, 40), [255, 0, 0, 255], "spread shadow left of the box");
        assert_eq!(pixels.at(15, 40), [0, 0, 0, 0], "beyond the spread");
    }

    #[test]
    fn skipping_transparent_interiors_keeps_the_pixels() {
        let node = || {
            boxed(DecorationStyle {
                width: Some(70.0),
                height: Some(50.0),
                border: Some(BorderStyle {
                    width: 2.0,
                    color: RED,
                }),
                border_radius: Some(10.0),
                background: Some(Color::rgba(0, 0, 0, 0)),
                box_shadow: vec![BoxShadow {
                    offset_x: 3.0,
                    offset_y: 5.0,
                    blur: 12.0,
                    spread: -2.0,
                    color: Color::BLACK,
                    inset: false,
                }],
                ..Default::default()
            })
        };
        for (scale, stretch, crop) in [(1.0, 1, false), (1.5, 1, false), (1.25, 2, false), (1.5, 1, true)] {
            let Some(skipped) = render_with(node(), scale, stretch, true, crop) else {
                return;
            };
            let full = render_with(node(), scale, stretch, false, crop).expect("second render");
            let diff = (0..OUT * OUT)
                .map(|i| (i % OUT, i / OUT))
                // Split quads interpolate the coordinates a hair differently.
                .filter(|&(x, y)| {
                    let (a, b) = (skipped.at(x, y), full.at(x, y));
                    (0..4).any(|c| a[c].abs_diff(b[c]) > 1)
                })
                .map(|(x, y)| (x, y, skipped.at(x, y), full.at(x, y)))
                .take(5)
                .collect::<Vec<_>>();
            assert!(
                diff.is_empty(),
                "interior skip changed pixels at scale {scale} stretch {stretch} crop {crop}: {diff:?}"
            );
        }
    }

    #[test]
    fn blurred_and_inset_shadows_fade_where_css_puts_them() {
        let Some(pixels) = render(
            boxed(DecorationStyle {
                width: Some(60.0),
                height: Some(60.0),
                background: Some(Color::rgba(255, 255, 255, 255)),
                box_shadow: vec![
                    BoxShadow {
                        offset_x: 0.0,
                        offset_y: 0.0,
                        blur: 8.0,
                        spread: 0.0,
                        color: Color::BLACK,
                        inset: true,
                    },
                    BoxShadow {
                        offset_x: 6.0,
                        offset_y: 0.0,
                        blur: 8.0,
                        spread: 0.0,
                        color: RED,
                        inset: false,
                    },
                ],
                ..Default::default()
            }),
            1.0,
        ) else {
            return;
        };
        // Outer: about half intensity at the shifted edge, fading out over
        // 3 sigma; on the left only the blur tail reaches past the box.
        let at_edge = pixels.at(80 + 6, 50)[3];
        let farther = pixels.at(80 + 9, 50)[3];
        assert!((100..=156).contains(&at_edge), "edge alpha {at_edge}");
        assert!(farther < at_edge, "fades: {farther} < {at_edge}");
        assert_eq!(pixels.at(80 + 6 + 13, 50)[3], 0, "ends after 3 sigma");
        assert!(pixels.at(19, 50)[3] < 20, "only the tail on the far side");
        assert_eq!(pixels.at(13, 50)[3], 0, "nothing past the tail");
        // Inset: darkest along the inner edge, the white background in the middle.
        let edge = pixels.at(20, 50);
        let middle = pixels.at(50, 50);
        assert!(edge[0] < 160, "inset darkens the edge: {edge:?}");
        assert_eq!(middle, [255, 255, 255, 255], "middle untouched");
    }

    #[test]
    fn custom_paint_sees_the_layout_pixels() {
        let dir = std::env::temp_dir().join(format!("shoji-paint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("content.frag");
        std::fs::write(
            &path,
            "uniform vec4 inside;\n\
             vec4 paint_main(PaintContext ctx) {\n\
                 vec4 c = ctx.content_rect_phy_px;\n\
                 vec2 p = ctx.frag_phy_px;\n\
                 bool in_content = p.x > c.x && p.y > c.y && p.x < c.x + c.z && p.y < c.y + c.w;\n\
                 return in_content ? inside : vec4(1.0, 0.0, 0.0, 1.0);\n\
             }\n",
        )
        .unwrap();
        let mut uniforms = std::collections::BTreeMap::new();
        uniforms.insert(
            "inside".to_string(),
            ShaderUniformValue::Vec4([0.0, 1.0, 0.0, 1.0]),
        );
        let Some(pixels) = render(
            boxed(DecorationStyle {
                width: Some(30.0),
                height: Some(20.0),
                padding: crate::ssd::Edges::all(3.0),
                paint: Some(PaintShader {
                    shader: ShaderModule {
                        path: path.to_string_lossy().into_owned(),
                    },
                    uniforms,
                    outsets: Default::default(),
                }),
                ..Default::default()
            }),
            1.25,
        ) else {
            return;
        };
        // padding 3 logical at 1.25x -> round(3.75) = 4px.
        assert_eq!(pixels.at(23, 30), [255, 0, 0, 255], "padding");
        assert_eq!(pixels.at(24, 30), [0, 255, 0, 255], "content starts at 4px");
        let _ = std::fs::remove_dir_all(dir);
    }
}
