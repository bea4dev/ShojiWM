use std::hash::{Hash, Hasher};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Offscreen, Renderer, Texture,
            damage::OutputDamageTracker,
            element::Element,
            element::RenderElement,
            element::texture::TextureRenderElement,
            gles::{GlesError, GlesRenderer, GlesTexture},
            utils::DamageBag,
        },
    },
    output::OutputModeSource,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform},
};
use std::sync::{Arc, Mutex};

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
    pub scene_signature: u64,
    pub damage: Arc<Mutex<DamageBag<i32, Buffer>>>,
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
    tracker: &mut OutputDamageTracker,
    rect: LogicalRect,
    z_index: usize,
    has_client_content: bool,
    scale: Scale<f64>,
    elements: &[E],
) -> Result<Option<LiveWindowSnapshot>, GlesError> {
    if rect.width <= 0 || rect.height <= 0 {
        return Ok(None);
    }

    let physical =
        Rectangle::<i32, Logical>::new(Point::from((0, 0)), (rect.width, rect.height).into())
            .to_physical_precise_round(scale);
    if physical.size.w <= 0 || physical.size.h <= 0 {
        return Ok(None);
    }

    let mut snapshot = if let Some(existing) = existing {
        let LiveWindowSnapshot {
            id,
            texture,
            scene_signature,
            damage,
            ..
        } = existing;
        if texture.size().w == physical.size.w
            && texture.size().h == physical.size.h
        {
            LiveWindowSnapshot {
                id,
                texture,
                rect,
                z_index,
                has_client_content,
                scene_signature,
                damage,
            }
        } else {
            LiveWindowSnapshot {
                id,
                texture: Offscreen::<GlesTexture>::create_buffer(
                    renderer,
                    Fourcc::Abgr8888,
                    (physical.size.w, physical.size.h).into(),
                )?,
                rect,
                z_index,
                has_client_content,
                scene_signature,
                damage, // preserve existing DamageBag so commit counter advances past stored value
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
            scene_signature: 0,
            damage: Arc::new(Mutex::new(DamageBag::new(4))),
        }
    };

    snapshot.rect = rect;
    snapshot.z_index = z_index;
    snapshot.has_client_content = has_client_content;

    // Check if the tracker's physical size still matches; if not, recreate it (age=0 full
    // redraw). If the size matches, use age=1 so only the actually changed regions are
    // re-rendered into the offscreen texture and added to the DamageBag.
    let age = match tracker.mode() {
        OutputModeSource::Static { size, scale: tracker_scale, .. }
            if *size == physical.size && *tracker_scale == Scale::from(scale.x) =>
        {
            1
        }
        _ => {
            *tracker =
                OutputDamageTracker::new(physical.size, scale.x, Transform::Normal);
            0
        }
    };

    let mut framebuffer = renderer.bind(&mut snapshot.texture)?;
    let render_output_result = tracker
        .render_output(
            renderer,
            &mut framebuffer,
            age,
            elements,
            [0.0, 0.0, 0.0, 0.0],
        )
        .map_err(|_| GlesError::FramebufferBindingError)?;

    // Eagerly collect the damage rects so the borrow of `tracker` ends here, before
    // framebuffer is dropped.
    let buffer_damage: Option<Vec<Rectangle<i32, Buffer>>> =
        render_output_result.damage.map(|rects| {
            rects
                .iter()
                .map(|r| {
                    Rectangle::<i32, Buffer>::new(
                        Point::from((r.loc.x, r.loc.y)),
                        (r.size.w, r.size.h).into(),
                    )
                })
                .collect()
        });

    drop(framebuffer);

    if let Some(rects) = buffer_damage {
        if !rects.is_empty() {
            let mut damage = snapshot.damage.lock().unwrap();
            let before = damage.current_commit();
            damage.add(rects.iter().copied());
            let after = damage.current_commit();
            if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
                tracing::info!(
                    commit_before = ?before,
                    commit_after = ?after,
                    damage_rect_count = rects.len(),
                    age,
                    "transform snapshot capture damage added"
                );
            }
        } else if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
            let commit = snapshot.damage.lock().unwrap().current_commit();
            tracing::info!(
                commit = ?commit,
                age,
                "transform snapshot capture: empty damage rects (no visual change)"
            );
        }
    } else if std::env::var_os("SHOJI_TRANSFORM_SNAPSHOT_DEBUG").is_some() {
        let commit = snapshot.damage.lock().unwrap().current_commit();
        tracing::info!(
            commit = ?commit,
            "transform snapshot capture skipped (no render damage)"
        );
    }

    Ok(Some(snapshot))
}

pub fn duplicate_snapshot(
    renderer: &mut GlesRenderer,
    source: &LiveWindowSnapshot,
) -> Result<LiveWindowSnapshot, GlesError> {
    let size = source.texture.size();
    let mut duplicated = LiveWindowSnapshot {
        id: smithay::backend::renderer::element::Id::new(),
        texture: Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, size)?,
        rect: source.rect,
        z_index: source.z_index,
        has_client_content: source.has_client_content,
        scene_signature: source.scene_signature,
        damage: Arc::new(Mutex::new(DamageBag::new(4))),
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
    {
        let mut damage = duplicated.damage.lock().unwrap();
        damage.add([Rectangle::from_size(duplicated.texture.size())]);
    }

    Ok(duplicated)
}

pub fn render_element_scene_signature<E: Element>(
    elements: &[E],
    scale: Scale<f64>,
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    elements.len().hash(&mut hasher);
    for element in elements {
        format!("{:?}", element.id()).hash(&mut hasher);
        format!("{:?}", element.current_commit()).hash(&mut hasher);
        format!("{:?}", element.geometry(scale)).hash(&mut hasher);
        format!("{:?}", element.src()).hash(&mut hasher);
    }
    hasher.finish()
}

pub fn closing_snapshot_element(
    renderer: &GlesRenderer,
    snapshot: &ClosingWindowSnapshot,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Option<TextureRenderElement<GlesTexture>> {
    let transformed =
        crate::backend::visual::transformed_root_rect(snapshot.live.rect, snapshot.transform);
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
    let src = Some(Rectangle::from_size(
        (
            snapshot.live.texture.size().w as f64,
            snapshot.live.texture.size().h as f64,
        )
            .into(),
    ));
    let damage_snapshot = snapshot.live.damage.lock().unwrap().snapshot();

    Some(TextureRenderElement::from_texture_with_damage(
        snapshot.live.id.clone(),
        renderer.context_id(),
        location,
        snapshot.live.texture.clone(),
        1,
        Transform::Normal,
        Some(visual.opacity),
        src,
        Some((snapshot.live.rect.width, snapshot.live.rect.height).into()),
        None,
        damage_snapshot,
        smithay::backend::renderer::element::Kind::Unspecified,
    ))
}

pub fn live_snapshot_element(
    renderer: &GlesRenderer,
    snapshot: &LiveWindowSnapshot,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Option<TextureRenderElement<GlesTexture>> {
    let rect = Rectangle::new(
        Point::from((snapshot.rect.x, snapshot.rect.y)),
        (snapshot.rect.width, snapshot.rect.height).into(),
    );
    if rect.intersection(output_geo).is_none() {
        return None;
    }

    let location: Point<i32, smithay::utils::Physical> =
        (Point::from((snapshot.rect.x, snapshot.rect.y)) - output_geo.loc)
            .to_f64()
            .to_physical_precise_round(scale);
    let location = location.to_f64();
    let src = Some(Rectangle::from_size(
        (
            snapshot.texture.size().w as f64,
            snapshot.texture.size().h as f64,
        )
            .into(),
    ));
    let damage_snapshot = snapshot.damage.lock().unwrap().snapshot();

    Some(TextureRenderElement::from_texture_with_damage(
        snapshot.id.clone(),
        renderer.context_id(),
        location,
        snapshot.texture.clone(),
        1,
        Transform::Normal,
        Some(alpha),
        src,
        Some((snapshot.rect.width, snapshot.rect.height).into()),
        None,
        damage_snapshot,
        smithay::backend::renderer::element::Kind::Unspecified,
    ))
}
