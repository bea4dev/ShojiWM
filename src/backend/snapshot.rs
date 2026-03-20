use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::RenderElement,
            element::texture::TextureRenderElement,
            gles::{GlesError, GlesRenderer, GlesTexture},
            Bind, Offscreen, Renderer, Texture,
        },
    },
    utils::{Logical, Physical, Point, Rectangle, Scale, Transform},
};

use crate::{
    backend::visual::window_visual_state,
    ssd::{LogicalRect, WindowDecorationState, WindowTransform},
};

#[derive(Debug, Clone)]
pub struct LiveWindowSnapshot {
    pub id: smithay::backend::renderer::element::Id,
    pub texture: GlesTexture,
    pub rect: LogicalRect,
    pub z_index: usize,
    pub has_client_content: bool,
}

#[derive(Debug, Clone)]
pub struct ClosingWindowSnapshot {
    pub window_id: String,
    pub live: LiveWindowSnapshot,
    pub decoration: WindowDecorationState,
    pub transform: WindowTransform,
}

pub fn capture_snapshot<E: RenderElement<GlesRenderer>>(
    renderer: &mut GlesRenderer,
    existing: Option<LiveWindowSnapshot>,
    rect: LogicalRect,
    z_index: usize,
    has_client_content: bool,
    scale: Scale<f64>,
    elements: &[E],
) -> Result<Option<LiveWindowSnapshot>, GlesError> {
    if rect.width <= 0 || rect.height <= 0 {
        return Ok(None);
    }

    let physical = Rectangle::<i32, Logical>::new(
        Point::from((0, 0)),
        (rect.width, rect.height).into(),
    )
    .to_physical_precise_round(scale);
    if physical.size.w <= 0 || physical.size.h <= 0 {
        return Ok(None);
    }

    let mut snapshot = if let Some(existing) = existing {
        if existing.texture.size().w == physical.size.w
            && existing.texture.size().h == physical.size.h
        {
            existing
        } else {
            LiveWindowSnapshot {
                id: existing.id,
                texture: Offscreen::<GlesTexture>::create_buffer(
                    renderer,
                    Fourcc::Abgr8888,
                    (physical.size.w, physical.size.h).into(),
                )?,
                rect,
                z_index,
                has_client_content,
            }
        }
    } else {
        LiveWindowSnapshot {
            id: smithay::backend::renderer::element::Id::new(),
            texture: Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (physical.size.w, physical.size.h).into(),
            )?,
            rect,
            z_index,
            has_client_content,
        }
    };

    snapshot.rect = rect;
    snapshot.z_index = z_index;
    snapshot.has_client_content = has_client_content;
    let mut framebuffer = renderer.bind(&mut snapshot.texture)?;
    let mut damage_tracker =
        OutputDamageTracker::new((physical.size.w, physical.size.h), 1.0, Transform::Normal);
    let _ = damage_tracker.render_output(
        renderer,
        &mut framebuffer,
        0,
        elements,
        [0.0, 0.0, 0.0, 0.0],
    )
    .map_err(|_| GlesError::FramebufferBindingError)?;
    drop(framebuffer);

    Ok(Some(snapshot))
}

pub fn duplicate_snapshot(
    renderer: &mut GlesRenderer,
    source: &LiveWindowSnapshot,
) -> Result<LiveWindowSnapshot, GlesError> {
    let size = source.texture.size();
    let mut duplicated = LiveWindowSnapshot {
        id: smithay::backend::renderer::element::Id::new(),
        texture: Offscreen::<GlesTexture>::create_buffer(
            renderer,
            Fourcc::Abgr8888,
            size,
        )?,
        rect: source.rect,
        z_index: source.z_index,
        has_client_content: source.has_client_content,
    };

    let element = TextureRenderElement::from_static_texture(
        smithay::backend::renderer::element::Id::new(),
        renderer.context_id(),
        Point::<f64, Physical>::from((0.0, 0.0)),
        source.texture.clone(),
        1,
        Transform::Normal,
        Some(1.0),
        None,
        None,
        None,
        smithay::backend::renderer::element::Kind::Unspecified,
    );

    let mut framebuffer = renderer.bind(&mut duplicated.texture)?;
    let mut damage_tracker = OutputDamageTracker::new((size.w, size.h), 1.0, Transform::Normal);
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

    Ok(duplicated)
}

pub fn closing_snapshot_element(
    renderer: &GlesRenderer,
    snapshot: &ClosingWindowSnapshot,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Option<TextureRenderElement<GlesTexture>> {
    let transformed = crate::backend::visual::transformed_root_rect(snapshot.live.rect, snapshot.transform);
    let transformed_rect = Rectangle::new(
        Point::from((transformed.x, transformed.y)),
        (transformed.width, transformed.height).into(),
    );
    if transformed_rect.intersection(output_geo).is_none() {
        return None;
    }

    let visual = window_visual_state(snapshot.live.rect, snapshot.transform, output_geo, scale);
    let location: Point<i32, smithay::utils::Physical> =
        (Point::from((snapshot.live.rect.x, snapshot.live.rect.y)) - output_geo.loc)
            .to_f64()
            .to_physical_precise_round(scale);
    let location = location.to_f64();

    Some(TextureRenderElement::from_static_texture(
        snapshot.live.id.clone(),
        renderer.context_id(),
        location,
        snapshot.live.texture.clone(),
        1,
        Transform::Normal,
        Some(visual.opacity),
        None,
        None,
        None,
        smithay::backend::renderer::element::Kind::Unspecified,
    ))
}
