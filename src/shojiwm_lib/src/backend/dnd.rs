//! Drawing drag icons and drag views (see `crate::dnd`).
//!
//! The client's icon surface is drawn into a texture per renderer whenever it
//! changes, and those textures are what the views show: the icon stays
//! drawable after the client destroys the surface at the drop. A view with a
//! decoration tree is drawn into textures of its own (the tree, with the
//! icon texture in its window slot), redrawn only when the tree, the icon or
//! the scale changes. A `<ShaderEffect>` that samples what is behind it
//! cannot be drawn ahead of the frame, so the tree is cut there into layers:
//! the textures before and after it, and the effect itself, which reads the
//! frame drawn so far (windows, then the view's own layers beneath it) the way
//! a window's backdrop does. Every piece is placed at the view's anchor point
//! and scaled around it, with the view's opacity.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ContextId, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::{
                Element, Id, Kind, RenderElement, RenderElementStates,
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                texture::TextureRenderElement,
                utils::{Relocate, RelocateRenderElement, RescaleRenderElement},
            },
            gles::{GlesError, GlesRenderer, GlesTexture},
        },
    },
    desktop::utils::{
        bbox_from_surface_tree, update_surface_primary_scanout_output, with_surfaces_surface_tree,
    },
    output::Output,
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Size, Transform},
    wayland::compositor::send_surface_state,
};
use tracing::warn;

use crate::{
    dnd::{DndState, DragIcon, DragView},
    ssd::{LogicalRect, StandaloneDecoration},
};

/// One piece of a drag view on an output.
pub enum DragElement {
    /// The icon, or a layer of a view's tree.
    Texture(RelocateRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>),
    /// A `<ShaderEffect>` reading what is behind it.
    Backdrop(
        RelocateRenderElement<
            RescaleRenderElement<
                RelocateRenderElement<crate::backend::decoration::DecorationSceneElements>,
            >,
        >,
    ),
}

impl DragElement {
    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        match self {
            Self::Texture(element) => element.geometry(scale),
            Self::Backdrop(element) => element.geometry(scale),
        }
    }
}

smithay::render_elements! {
    DragViewCaptureElements<=GlesRenderer>;
    Paint=crate::backend::paint::StablePaintElement,
    Texture=crate::backend::text::DecorationTextureElements,
    Icon=TextureRenderElement<GlesTexture>,
    Shader=crate::backend::shader_effect::StableShaderEffectElement,
}

/// The icon surface drawn on one renderer.
#[derive(Debug)]
pub struct IconCapture {
    context: ContextId<GlesTexture>,
    texture: GlesTexture,
    /// New with every redraw, so the texture element damages itself.
    id: Id,
    /// The surface tree's bounds relative to its own origin.
    bounds: Rectangle<i32, Logical>,
    scale: f64,
    signature: u64,
    render_states: RenderElementStates,
}

/// A view's decoration tree drawn on one renderer at one scale.
#[derive(Debug)]
pub struct ViewCapture {
    context: ContextId<GlesTexture>,
    scale: f64,
    /// Front to back.
    pieces: Vec<ViewPiece>,
    /// The anchor point in physical pixels from the root's origin.
    anchor_px: Point<f64, Physical>,
    /// The layout revision and icon picture it was drawn from.
    generation: u64,
    icon_id: Option<Id>,
}

#[derive(Debug)]
enum ViewPiece {
    Layer(ViewLayer),
    /// `shader_buffers[index]` of the view's decoration, drawn on the frame.
    Backdrop(usize),
}

/// Consecutive items of a view's tree drawn into one texture.
#[derive(Debug)]
struct ViewLayer {
    texture: GlesTexture,
    id: Id,
    /// The texture's size in logical pixels (it is `round(size · scale)`
    /// physical pixels large).
    size: Size<i32, Logical>,
    /// The texture's top-left corner in physical pixels from the root's
    /// origin.
    origin_px: Point<i32, Physical>,
}

/// The size the window slot of a view gets: the icon's.
pub(crate) fn icon_size(icon: &DragIcon) -> Option<(i32, i32)> {
    icon.live_bounds()
        .or_else(|| icon.captures.first().map(|capture| capture.bounds))
        .map(|bounds| (bounds.size.w, bounds.size.h))
        .filter(|(width, height)| *width > 0 && *height > 0)
}

/// The elements of every drag view on an output, front to back.
pub fn drag_elements(
    renderer: &mut GlesRenderer,
    dnd: &mut DndState,
    output: &Output,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    pointer: Point<f64, Logical>,
) -> Vec<DragElement> {
    if !dnd.has_views() {
        return Vec::new();
    }
    let context = renderer.context_id();
    let active_drag = dnd.active.as_ref().map(|active| active.id);
    let DndState { records, views, .. } = dnd;

    for record in records.values_mut() {
        if let Some(icon) = record.icon.as_mut()
            && let Err(error) = update_icon_capture(renderer, icon, scale)
        {
            warn!(?error, "failed to draw the drag icon");
        }
    }

    let output_rect =
        Rectangle::from_size(output_geo.size.to_f64().to_physical(scale).to_i32_round());
    let mut elements = Vec::new();
    // Later views are on top.
    for view in views.iter_mut().rev() {
        if view.props.opacity <= 0.0 || view.props.scale <= 0.0 {
            continue;
        }
        let icon = records
            .get(&view.drag_id)
            .and_then(|record| record.icon.as_ref());
        let icon_capture = icon.and_then(|icon| {
            icon.captures
                .iter()
                .find(|capture| capture.context == context)
                .map(|capture| (icon, capture))
        });
        let anchor = DndState::anchor_point(&view.props, pointer);
        let anchor_px = (anchor - output_geo.loc.to_f64()).to_physical(scale);
        let opacity = view.props.opacity as f32;
        let view_scale = view.props.scale;
        let place = |element: TextureRenderElement<GlesTexture>| {
            let element =
                RescaleRenderElement::from_element(element, anchor_px.to_i32_round(), view_scale);
            DragElement::Texture(RelocateRenderElement::from_element(
                element,
                (0, 0),
                Relocate::Relative,
            ))
        };
        let pieces = match view.content.as_mut() {
            None => icon_capture
                .map(|(icon, capture)| {
                    // The pointer sits at `-offset` from the surface origin.
                    let anchor_in_texture = Point::<i32, Logical>::from((
                        -icon.offset.x - capture.bounds.loc.x,
                        -icon.offset.y - capture.bounds.loc.y,
                    ))
                    .to_f64()
                    .to_physical(scale);
                    // A whole-pixel location keeps the texture's pixels on the output's.
                    let location = (anchor_px - anchor_in_texture).to_i32_round::<i32>();
                    vec![place(texture_element(
                        capture.id.clone(),
                        &context,
                        location,
                        &capture.texture,
                        capture.bounds.size,
                        opacity,
                    ))]
                })
                .unwrap_or_default(),
            Some(_) => match view_pieces(renderer, view, icon_capture, scale, anchor_px, &place) {
                Ok(pieces) => pieces,
                Err(error) => {
                    warn!(view_id = view.id, ?error, "failed to draw a drag view");
                    Vec::new()
                }
            },
        };
        let mut drawn = false;
        for piece in pieces {
            if piece.geometry(scale).overlaps(output_rect) {
                elements.push(piece);
                drawn = true;
            }
        }
        if !drawn {
            continue;
        }

        // The icon of the running drag is presented here: let the client
        // learn its output (frame callbacks, preferred scale).
        if Some(view.drag_id) == active_drag
            && let Some((icon, capture)) = icon_capture
            && icon.surface.alive()
        {
            // The client learns the output's scale from its enter (and from
            // the preferred scale sent with the frame callbacks), so it draws
            // the icon sharp instead of at scale 1.
            let buffer_scale = output.current_scale().integer_scale();
            let transform = output.current_transform();
            with_surfaces_surface_tree(&icon.surface, |surface, states| {
                output.enter(surface);
                send_surface_state(surface, states, buffer_scale, transform);
                update_surface_primary_scanout_output(
                    surface,
                    output,
                    states,
                    None,
                    &capture.render_states,
                    crate::presentation::area_primary_scanout_compare,
                );
            });
        }
    }
    elements
}

/// Redraw the icon's texture for this renderer when the surface changed.
fn update_icon_capture(
    renderer: &mut GlesRenderer,
    icon: &mut DragIcon,
    scale: Scale<f64>,
) -> Result<(), GlesError> {
    // A destroyed surface keeps the last picture.
    if !icon.surface.alive() {
        return Ok(());
    }
    let bounds = bbox_from_surface_tree(&icon.surface, (0, 0));
    if bounds.size.w <= 0 || bounds.size.h <= 0 {
        return Ok(());
    }
    let context = renderer.context_id();
    let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(
            renderer,
            &icon.surface,
            Point::<i32, Logical>::from((-bounds.loc.x, -bounds.loc.y))
                .to_physical_precise_round(scale.x),
            scale,
            1.0,
            Kind::Unspecified,
        );
    if surface_elements.is_empty() {
        return Ok(());
    }
    let signature =
        crate::backend::snapshot::render_element_scene_signature(&surface_elements, scale);
    let existing = icon
        .captures
        .iter()
        .position(|capture| capture.context == context);
    if let Some(index) = existing {
        let capture = &icon.captures[index];
        if capture.signature == signature && capture.scale == scale.x && capture.bounds == bounds {
            return Ok(());
        }
    }
    let size = bounds.size.to_f64().to_physical(scale).to_i32_round();
    let (texture, render_states) = render_to_texture(renderer, size, scale, &surface_elements)?;
    let capture = IconCapture {
        context,
        texture,
        id: Id::new(),
        bounds,
        scale: scale.x,
        signature,
        render_states,
    };
    match existing {
        Some(index) => icon.captures[index] = capture,
        None => icon.captures.push(capture),
    }
    Ok(())
}

fn texture_element(
    id: Id,
    context: &ContextId<GlesTexture>,
    location: Point<i32, Physical>,
    texture: &GlesTexture,
    size: Size<i32, Logical>,
    alpha: f32,
) -> TextureRenderElement<GlesTexture> {
    TextureRenderElement::from_static_texture(
        id,
        context.clone(),
        location.to_f64(),
        texture.clone(),
        1,
        Transform::Normal,
        Some(alpha),
        Some(whole_texture(texture)),
        Some(size),
        None,
        Kind::Unspecified,
    )
}

/// The pieces of a view with a decoration tree, front to back, its layers
/// redrawn first when the tree, its layout or the icon changed.
fn view_pieces(
    renderer: &mut GlesRenderer,
    view: &mut DragView,
    icon: Option<(&DragIcon, &IconCapture)>,
    scale: Scale<f64>,
    anchor_px: Point<f64, Physical>,
    place: &dyn Fn(TextureRenderElement<GlesTexture>) -> DragElement,
) -> Result<Vec<DragElement>, GlesError> {
    let context = renderer.context_id();
    let opacity = view.props.opacity as f32;
    let view_scale = view.props.scale;
    let Some(content) = view.content.as_mut() else {
        return Ok(Vec::new());
    };
    let Some(layout) = content
        .layouts
        .iter_mut()
        .find(|layout| layout.scale == scale.x && layout.generation == content.generation)
    else {
        // Laid out before the next frame (`refresh_drag_views`).
        return Ok(Vec::new());
    };
    let decoration = &mut layout.decoration;
    let icon_id = icon.map(|(_, capture)| capture.id.clone());
    let existing = view
        .captures
        .iter()
        .position(|capture| capture.context == context && capture.scale == scale.x);
    let index = match existing {
        Some(index)
            if view.captures[index].generation == layout.revision
                && view.captures[index].icon_id == icon_id =>
        {
            index
        }
        _ => {
            let icon = icon.map(|(icon, capture)| (icon.offset, capture));
            let capture = capture_view(renderer, decoration, icon, scale, layout.revision)?;
            match existing {
                Some(index) => {
                    view.captures[index] = capture;
                    index
                }
                None => {
                    view.captures.push(capture);
                    view.captures.len() - 1
                }
            }
        }
    };
    let capture = &view.captures[index];

    // Every piece is placed from the root's origin on the output, on whole
    // pixels.
    let root_px = (anchor_px - capture.anchor_px).to_i32_round::<i32>();
    let root = decoration.layout.root.rect;
    let everywhere = everywhere();
    let mut pieces = Vec::new();
    for piece in &capture.pieces {
        match piece {
            ViewPiece::Layer(layer) => pieces.push(place(texture_element(
                layer.id.clone(),
                &context,
                root_px + layer.origin_px,
                &layer.texture,
                layer.size,
                opacity,
            ))),
            ViewPiece::Backdrop(index) => {
                let Some(cached) = decoration.shader_buffers.get(*index) else {
                    continue;
                };
                let Some(spec) = crate::backend::decoration::shader_effect_spec_in_root(
                    root,
                    Default::default(),
                    cached,
                    everywhere,
                    scale,
                    opacity,
                ) else {
                    continue;
                };
                let state = decoration
                    .shader_cache
                    .entry(cached.stable_key.clone())
                    .or_default();
                let element = match state.backdrop_element(renderer, spec) {
                    Ok(element) => element,
                    Err(error) => {
                        warn!(?error, "drag view shader effect failed");
                        continue;
                    }
                };
                let element = RelocateRenderElement::from_element(
                    crate::backend::decoration::DecorationSceneElements::Backdrop(element),
                    root_px,
                    Relocate::Relative,
                );
                let element = RescaleRenderElement::from_element(
                    element,
                    anchor_px.to_i32_round(),
                    view_scale,
                );
                pieces.push(DragElement::Backdrop(RelocateRenderElement::from_element(
                    element,
                    (0, 0),
                    Relocate::Relative,
                )));
            }
        }
    }
    Ok(pieces)
}

/// Culling area for the element builders: everything.
fn everywhere() -> Rectangle<i32, Logical> {
    Rectangle::new((-1 << 20, -1 << 20).into(), (1 << 21, 1 << 21).into())
}

/// One item of a view's tree, in physical pixels from the root's origin.
enum CaptureItem {
    Element(DragViewCaptureElements),
    /// A `<ShaderEffect>` that samples the frame: `shader_buffers[index]`.
    Backdrop(usize),
}

/// Draw a view's tree into layers. `icon`: where the icon's surface sits
/// relative to the pointer, and its picture.
fn capture_view(
    renderer: &mut GlesRenderer,
    decoration: &mut StandaloneDecoration,
    icon: Option<(Point<i32, Logical>, &IconCapture)>,
    scale: Scale<f64>,
    generation: u64,
) -> Result<ViewCapture, GlesError> {
    let context = renderer.context_id();
    let root = decoration.layout.root.rect;
    let everywhere = everywhere();

    // Every item with whether it is an `overlay` paint (drawn over the icon,
    // as over a window's client) and its paint order.
    let mut items: Vec<(usize, bool, CaptureItem)> = Vec::new();
    let to_output = crate::backend::paint::LayoutToOutput::for_layout(&decoration.layout, scale);
    for cached in &decoration.buffers {
        match crate::backend::paint::paint_element(
            renderer,
            to_output,
            &mut decoration.paint_cache,
            cached,
            everywhere,
            1.0,
        ) {
            Ok(Some(element)) => items.push((
                cached.order,
                cached.stable_key.ends_with(":overlay"),
                CaptureItem::Element(DragViewCaptureElements::Paint(element)),
            )),
            Ok(None) => {}
            Err(error) => warn!(?error, "drag view paint shader failed"),
        }
    }
    for (index, cached) in decoration.shader_buffers.iter().enumerate() {
        if cached.shader.supports_framebuffer_backdrop() {
            items.push((cached.order, false, CaptureItem::Backdrop(index)));
            continue;
        }
        // A window's own content (`windowSource()`): there is none here.
        if cached.shader.is_texture_backed() {
            continue;
        }
        let Some(spec) = crate::backend::decoration::shader_effect_spec_in_root(
            root,
            Default::default(),
            cached,
            everywhere,
            scale,
            1.0,
        ) else {
            continue;
        };
        let state = decoration
            .shader_cache
            .entry(cached.stable_key.clone())
            .or_default();
        match state.element(renderer, spec) {
            Ok(element) => items.push((
                cached.order,
                false,
                CaptureItem::Element(DragViewCaptureElements::Shader(element)),
            )),
            Err(error) => warn!(?error, "drag view shader effect failed"),
        }
    }
    for label in &decoration.text_buffers {
        if let Some(element) = crate::backend::text::memory_text_element(
            renderer,
            label,
            root,
            Default::default(),
            everywhere,
            scale,
            1.0,
        )? {
            items.push((
                label.order,
                false,
                CaptureItem::Element(DragViewCaptureElements::Texture(element)),
            ));
        }
    }
    for image in &decoration.icon_buffers {
        if let Some(element) = crate::backend::icon::memory_icon_element(
            renderer,
            image,
            root,
            Default::default(),
            everywhere,
            scale,
            1.0,
        )? {
            items.push((
                image.order,
                false,
                CaptureItem::Element(DragViewCaptureElements::Texture(element)),
            ));
        }
    }
    // Front to back, like a window's decoration: lower paint order in front.
    items.sort_by_key(|(order, overlay, _)| (!*overlay, *order));
    let overlays = items.iter().filter(|(_, overlay, _)| *overlay).count();
    let mut items: Vec<CaptureItem> = items.into_iter().map(|(_, _, item)| item).collect();

    // The icon goes in the window slot, under the overlays and over
    // everything else; the anchor point is where the pointer sits on it.
    let slot = decoration.layout.window_slot_origin_px();
    let mut anchor_px = slot
        .map(|(x, y)| Point::<f64, Physical>::from((x as f64, y as f64)))
        .unwrap_or_default();
    if let (Some((x, y)), Some((offset, capture))) = (slot, icon) {
        items.insert(
            overlays,
            CaptureItem::Element(DragViewCaptureElements::Icon(texture_element(
                capture.id.clone(),
                &context,
                Point::from((x, y)),
                &capture.texture,
                capture.bounds.size,
                1.0,
            ))),
        );
        anchor_px -= Point::<i32, Logical>::from((
            offset.x + capture.bounds.loc.x,
            offset.y + capture.bounds.loc.y,
        ))
        .to_f64()
        .to_physical(scale);
    }

    // Consecutive elements share a layer; a backdrop effect cuts them.
    let mut pieces = Vec::new();
    let mut run = Vec::new();
    for item in items {
        match item {
            CaptureItem::Element(element) => run.push(element),
            CaptureItem::Backdrop(index) => {
                if let Some(layer) = capture_layer(renderer, std::mem::take(&mut run), scale)? {
                    pieces.push(ViewPiece::Layer(layer));
                }
                pieces.push(ViewPiece::Backdrop(index));
            }
        }
    }
    if let Some(layer) = capture_layer(renderer, run, scale)? {
        pieces.push(ViewPiece::Layer(layer));
    }
    Ok(ViewCapture {
        context,
        scale: scale.x,
        pieces,
        anchor_px,
        generation,
        icon_id: icon.map(|(_, capture)| capture.id.clone()),
    })
}

/// Draw `elements` (front to back, in physical pixels from the root's
/// origin) into a texture that covers them, on whole logical pixels so its
/// element maps onto it exactly.
fn capture_layer(
    renderer: &mut GlesRenderer,
    elements: Vec<DragViewCaptureElements>,
    scale: Scale<f64>,
) -> Result<Option<ViewLayer>, GlesError> {
    let Some(bounds_px) = elements
        .iter()
        .map(|element| element.geometry(scale))
        .reduce(|bounds, geometry| bounds.merge(geometry))
    else {
        return Ok(None);
    };
    let logical = logical_cover(bounds_px, scale.x);
    if logical.width <= 0 || logical.height <= 0 {
        return Ok(None);
    }
    let origin_px = Point::<i32, Logical>::from((logical.x, logical.y))
        .to_f64()
        .to_physical(scale)
        .to_i32_round::<i32>();
    let size = Size::<i32, Logical>::from((logical.width, logical.height));
    let elements: Vec<RelocateRenderElement<DragViewCaptureElements>> = elements
        .into_iter()
        .map(|element| {
            RelocateRenderElement::from_element(
                element,
                Point::from((-origin_px.x, -origin_px.y)),
                Relocate::Relative,
            )
        })
        .collect();
    let (texture, _) = render_to_texture(
        renderer,
        size.to_f64().to_physical(scale).to_i32_round(),
        scale,
        &elements,
    )?;
    Ok(Some(ViewLayer {
        texture,
        id: Id::new(),
        size,
        origin_px,
    }))
}

/// The whole texture as a texture element's source: without it, the source
/// is the element's logical size in texture pixels, which crops (and
/// stretches) a texture drawn at a scale above 1.
fn whole_texture(texture: &GlesTexture) -> Rectangle<f64, Logical> {
    let size = texture.size();
    Rectangle::from_size((size.w as f64, size.h as f64).into())
}

/// The smallest whole-logical-pixel rect that covers `rect`.
fn logical_cover(rect: Rectangle<i32, Physical>, scale: f64) -> LogicalRect {
    let scale = scale.max(0.0001);
    let left = (rect.loc.x as f64 / scale).floor() as i32;
    let top = (rect.loc.y as f64 / scale).floor() as i32;
    let right = ((rect.loc.x + rect.size.w) as f64 / scale).ceil() as i32;
    let bottom = ((rect.loc.y + rect.size.h) as f64 / scale).ceil() as i32;
    LogicalRect::new(left, top, right - left, bottom - top)
}

/// Draw `elements` into a new transparent texture of `size` physical pixels.
fn render_to_texture<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    elements: &[E],
) -> Result<(GlesTexture, RenderElementStates), GlesError> {
    let mut texture = Offscreen::<GlesTexture>::create_buffer(
        renderer,
        Fourcc::Abgr8888,
        (size.w.max(1), size.h.max(1)).into(),
    )?;
    let mut tracker = OutputDamageTracker::new(size, scale.x, Transform::Normal);
    let states = {
        let mut framebuffer = renderer.bind(&mut texture)?;
        tracker
            .render_output(
                renderer,
                &mut framebuffer,
                0,
                elements,
                [0.0, 0.0, 0.0, 0.0],
            )
            .map_err(|_| GlesError::FramebufferBindingError)?
            .states
    };
    Ok((texture, states))
}

#[cfg(test)]
mod tests {
    use smithay::backend::{
        egl::{EGLContext, EGLDisplay},
        renderer::{Color32F, ExportMem, element::solid::SolidColorRenderElement},
    };

    use super::*;
    use crate::ssd::{
        BoxNode, Color, DecorationNode, DecorationNodeKind, DecorationStyle, DecorationTree, Edges,
        LayoutDirection, paint_buffers_for_layout,
    };

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

    fn solid(width: f64, height: f64, color: Color) -> DecorationNode {
        let mut node = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }));
        node.style = DecorationStyle {
            width: Some(width),
            height: Some(height),
            flex_shrink: Some(0.0),
            background: Some(color),
            ..Default::default()
        };
        node
    }

    /// A blue column with 2px padding: an 8x4 green box above the slot.
    fn tree() -> DecorationTree {
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }));
        root.style = DecorationStyle {
            padding: Edges::all(2.0),
            background: Some(Color::rgba(0, 0, 255, 255)),
            ..Default::default()
        };
        root.children = vec![
            solid(8.0, 4.0, Color::rgba(0, 255, 0, 255)),
            DecorationNode::new(DecorationNodeKind::WindowSlot),
        ];
        DecorationTree::new(root)
    }

    #[test]
    fn standalone_layout_takes_its_content_size() {
        let layout = tree().layout_standalone(Some((10, 10)), 1.5).unwrap();
        // 2 + max(8, 10) + 2 wide, 2 + 4 + 10 + 2 high.
        assert_eq!(layout.root.rect, LogicalRect::new(0, 0, 14, 18));
        assert_eq!(layout.window_slot_origin_px(), Some((3, 9)));
        assert_eq!(
            layout.window_slot_rect(),
            Some(LogicalRect::new(2, 6, 10, 10))
        );
        // Without an icon the slot is empty and the content shrinks.
        let layout = tree().layout_standalone(None, 1.0).unwrap();
        assert_eq!(layout.root.rect, LogicalRect::new(0, 0, 12, 8));
    }

    fn pixel(pixels: &[u8], width: i32, x: i32, y: i32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        [
            pixels[offset],
            pixels[offset + 1],
            pixels[offset + 2],
            pixels[offset + 3],
        ]
    }

    fn read(renderer: &mut GlesRenderer, texture: &mut GlesTexture) -> Vec<u8> {
        let size = texture.size();
        let fb = renderer.bind(texture).unwrap();
        let mapping = renderer
            .copy_framebuffer(&fb, Rectangle::from_size(size), Fourcc::Abgr8888)
            .unwrap();
        drop(fb);
        renderer.map_texture(&mapping).unwrap().to_vec()
    }

    #[test]
    fn view_capture_puts_the_icon_in_the_slot_and_anchors_at_the_pointer() {
        let Some(mut renderer) = try_renderer() else {
            eprintln!("no render node; skipping");
            return;
        };
        for (scale, anchor, icon_px) in [(1.0, (7.0, 11.0), (2, 6)), (1.5, (10.5, 16.5), (3, 9))] {
            let scale = Scale::from(scale);
            // A red 10x10 icon whose center is under the pointer.
            let icon_size = Size::<i32, Logical>::from((10, 10))
                .to_f64()
                .to_physical(scale)
                .to_i32_round();
            // Red in its top third, green below: a cropped or stretched icon
            // moves the boundary.
            let third = icon_size.h / 3;
            let red = SolidColorRenderElement::new(
                Id::new(),
                Rectangle::from_size((icon_size.w, third).into()),
                0,
                Color32F::new(1.0, 0.0, 0.0, 1.0),
                Kind::Unspecified,
            );
            let green = SolidColorRenderElement::new(
                Id::new(),
                Rectangle::new((0, third).into(), (icon_size.w, icon_size.h - third).into()),
                0,
                Color32F::new(0.0, 1.0, 0.0, 1.0),
                Kind::Unspecified,
            );
            let (icon_texture, _) =
                render_to_texture(&mut renderer, icon_size, scale, &[red, green]).unwrap();
            let icon = IconCapture {
                context: renderer.context_id(),
                texture: icon_texture,
                id: Id::new(),
                bounds: Rectangle::from_size((10, 10).into()),
                scale: scale.x,
                signature: 0,
                render_states: Default::default(),
            };
            let tree = tree();
            let layout = tree.layout_standalone(Some((10, 10)), scale.x).unwrap();
            let mut decoration = StandaloneDecoration {
                buffers: paint_buffers_for_layout(&layout),
                layout,
                shader_buffers: Vec::new(),
                shader_cache: Default::default(),
                text_buffers: Vec::new(),
                icon_buffers: Vec::new(),
                paint_cache: Default::default(),
            };
            let capture = capture_view(
                &mut renderer,
                &mut decoration,
                Some((Point::from((-5, -5)), &icon)),
                scale,
                1,
            )
            .unwrap();
            let [ViewPiece::Layer(layer)] = capture.pieces.as_slice() else {
                panic!("one layer without backdrop effects: {:?}", capture.pieces);
            };
            assert_eq!(layer.size, Size::from((14, 18)));
            assert_eq!(layer.origin_px, Point::from((0, 0)));
            assert_eq!(capture.anchor_px, Point::from(anchor));
            let mut texture = layer.texture.clone();
            let width = texture.size().w;
            let pixels = read(&mut renderer, &mut texture);
            let (x, y) = (anchor.0 as i32, anchor.1 as i32);
            assert_eq!(
                pixel(&pixels, width, x, y),
                [0, 255, 0, 255],
                "icon under the pointer"
            );
            assert_eq!(
                pixel(&pixels, width, icon_px.0, icon_px.1),
                [255, 0, 0, 255],
                "icon corner"
            );
            assert_eq!(
                pixel(&pixels, width, icon_px.0, icon_px.1 + third + 1),
                [0, 255, 0, 255],
                "the icon is drawn whole, not cropped and stretched"
            );
            assert_eq!(
                pixel(&pixels, width, icon_px.0, icon_px.1 + third - 1),
                [255, 0, 0, 255],
                "red ends a third down"
            );
            assert_eq!(
                pixel(&pixels, width, icon_px.0 - 1, icon_px.1),
                [0, 0, 255, 255],
                "padding"
            );
            assert_eq!(
                pixel(&pixels, width, icon_px.0, icon_px.1 - 1),
                [0, 255, 0, 255],
                "box above"
            );
            assert_eq!(pixel(&pixels, width, 0, 0), [0, 0, 255, 255], "background");
        }
    }

    /// A `<ShaderEffect>` reading the backdrop cuts the view into layers and
    /// sees the frame beneath it; its children stay in front of it.
    #[test]
    fn backdrop_effect_reads_the_frame_beneath_the_view() {
        use crate::ssd::{
            CompiledEffect, EffectAlphaMode, EffectInput, EffectInvalidationPolicy, EffectStage,
            ShaderEffectNode, ShaderModule, ShaderStage,
        };
        let Some(mut renderer) = try_renderer() else {
            eprintln!("no render node; skipping");
            return;
        };
        // Swaps red and green of what is behind.
        let shader_path = std::env::temp_dir().join(format!(
            "shojiwm-drag-backdrop-test-{}.frag",
            std::process::id()
        ));
        std::fs::write(
            &shader_path,
            "vec4 shader_main(EffectContext effect) {\n\
             \x20   vec4 c = texture2D(tex, effect.texture_uv);\n\
             \x20   return vec4(c.g, c.r, c.b, 1.0);\n\
             }\n",
        )
        .unwrap();
        let mut effect = DecorationNode::new(DecorationNodeKind::ShaderEffect(ShaderEffectNode {
            direction: LayoutDirection::Column,
            shader: CompiledEffect {
                input: EffectInput::Backdrop,
                capture_padding: 0,
                invalidate: EffectInvalidationPolicy::Always,
                pipeline: vec![EffectStage::Shader(ShaderStage {
                    shader: ShaderModule {
                        path: shader_path.to_string_lossy().into_owned(),
                    },
                    uniforms: Default::default(),
                    textures: Default::default(),
                })],
                alpha: EffectAlphaMode::Preserve,
            },
        }));
        effect.style = DecorationStyle {
            width: Some(20.0),
            height: Some(20.0),
            flex_shrink: Some(0.0),
            ..Default::default()
        };
        // A blue square in the effect's top-left corner, in front of it.
        effect.children = vec![solid(6.0, 6.0, Color::rgba(0, 0, 255, 255))];
        let mut root = DecorationNode::new(DecorationNodeKind::Box(BoxNode {
            direction: LayoutDirection::Column,
        }));
        root.style = DecorationStyle {
            padding: Edges::all(4.0),
            ..Default::default()
        };
        root.children = vec![effect];
        let scale = Scale::from(1.0);
        let tree = DecorationTree::new(root);
        let mut text = crate::backend::text::TextRasterizer::new(None);
        let mut icons = crate::backend::icon::IconRasterizer::new(None);
        let decoration = StandaloneDecoration::build(
            &tree,
            None,
            scale.x,
            Default::default(),
            &mut text,
            &mut icons,
            None,
        )
        .unwrap();
        let mut view = DragView {
            id: 1,
            drag_id: 1,
            props: Default::default(),
            content: Some(crate::dnd::DragViewContent {
                tree,
                generation: 1,
                layouts: vec![crate::dnd::ScaledDragViewLayout {
                    scale: scale.x,
                    slot_size: None,
                    generation: 1,
                    revision: 1,
                    decoration,
                }],
            }),
            max_linger: std::time::Duration::ZERO,
            deadline: None,
            captures: Vec::new(),
        };
        let place = |element: TextureRenderElement<GlesTexture>| {
            DragElement::Texture(RelocateRenderElement::from_element(
                RescaleRenderElement::from_element(element, (0, 0).into(), 1.0),
                (0, 0),
                Relocate::Relative,
            ))
        };
        // The root's top-left corner lands at (10, 10).
        let pieces = view_pieces(
            &mut renderer,
            &mut view,
            None,
            scale,
            Point::from((10.0, 10.0)),
            &place,
        )
        .unwrap();
        assert_eq!(
            view.captures[0]
                .pieces
                .iter()
                .map(|piece| matches!(piece, ViewPiece::Backdrop(_)))
                .collect::<Vec<_>>(),
            [false, true],
            "the blue square in front, then the effect"
        );

        smithay::render_elements! {
            Scene<=GlesRenderer>;
            Background=SolidColorRenderElement,
            Texture=RelocateRenderElement<RescaleRenderElement<TextureRenderElement<GlesTexture>>>,
            Backdrop=RelocateRenderElement<
                RescaleRenderElement<
                    RelocateRenderElement<crate::backend::decoration::DecorationSceneElements>,
                >,
            >,
        }

        let size = Size::<i32, Physical>::from((48, 48));
        let mut elements: Vec<Scene> = pieces
            .into_iter()
            .map(|piece| match piece {
                DragElement::Texture(element) => Scene::Texture(element),
                DragElement::Backdrop(element) => Scene::Backdrop(element),
            })
            .collect();
        elements.push(Scene::Background(SolidColorRenderElement::new(
            Id::new(),
            Rectangle::from_size(size),
            0,
            Color32F::new(1.0, 0.0, 0.0, 1.0),
            Kind::Unspecified,
        )));
        let mut target: GlesTexture = Offscreen::<GlesTexture>::create_buffer(
            &mut renderer,
            Fourcc::Abgr8888,
            (size.w, size.h).into(),
        )
        .unwrap();
        {
            let mut fb = renderer.bind(&mut target).unwrap();
            let mut tracker = OutputDamageTracker::new(size, 1.0, Transform::Normal);
            tracker
                .render_output(&mut renderer, &mut fb, 0, &elements, [0.0, 0.0, 0.0, 0.0])
                .unwrap();
        }
        // The effect's shader is built while drawing.
        let _ = std::fs::remove_file(&shader_path);
        let pixels = read(&mut renderer, &mut target);
        // Root at (10, 10), effect at (14, 14) 20x20, blue square at (14, 14) 6x6.
        assert_eq!(
            pixel(&pixels, 48, 12, 12),
            [255, 0, 0, 255],
            "padding shows the frame"
        );
        assert_eq!(
            pixel(&pixels, 48, 30, 30),
            [0, 255, 0, 255],
            "the effect swapped the red"
        );
        assert_eq!(
            pixel(&pixels, 48, 16, 16),
            [0, 0, 255, 255],
            "its child in front"
        );
        assert_eq!(
            pixel(&pixels, 48, 40, 40),
            [255, 0, 0, 255],
            "outside the view"
        );
    }

    #[test]
    fn logical_cover_contains_the_physical_rect() {
        let rect = Rectangle::<i32, Physical>::new((-3, 5).into(), (10, 7).into());
        let cover = logical_cover(rect, 1.5);
        assert_eq!(cover, LogicalRect::new(-2, 3, 7, 5));
        assert!((cover.x as f64 * 1.5) <= -3.0);
        assert!(((cover.x + cover.width) as f64 * 1.5) >= 7.0);
    }
}
