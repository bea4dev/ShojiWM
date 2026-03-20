use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::memory::MemoryRenderBufferRenderElement,
        gles::{GlesError, GlesRenderer},
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
};
use tracing::trace;

use crate::{
    backend::rounded::{RoundedClip, RoundedRectSpec, RoundedShapeKind, StableRoundedElement},
    backend::text,
    ssd::{LogicalRect, WindowDecorationState},
};

pub fn rounded_elements_for_output(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &mut HashMap<Window, WindowDecorationState>,
    output: &Output,
) -> Result<Vec<StableRoundedElement>, GlesError> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Ok(Vec::new());
    };
    let scale = Scale::from(output.current_scale().fractional_scale());

    let mut elements = Vec::new();
    for window in space.elements() {
        let Some(decoration) = decorations.get_mut(window) else {
            continue;
        };

        let buffers = decoration.buffers.clone();
        for cached in &buffers {
            if let Some(element) =
                rounded_rect_element(renderer, decoration, cached, output_geo, scale, 1.0)?
            {
                elements.push(element);
            }
        }
    }

    trace!(
        output = %output.name(),
        output_geometry = ?output_geo,
        element_count = elements.len(),
        "prepared rounded decoration elements for output"
    );

    Ok(elements)
}

pub fn rounded_elements_for_window(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<StableRoundedElement>, GlesError> {
    let buffers = decoration.buffers.clone();
    buffers
        .iter()
        .filter_map(|cached| {
            rounded_rect_element(renderer, decoration, cached, output_geo, scale, alpha).transpose()
        })
        .collect()
}

pub fn text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    text::text_elements_for_window(renderer, space, decorations, output, window, alpha)
}

pub fn icon_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<MemoryRenderBufferRenderElement<GlesRenderer>>, GlesError> {
    crate::backend::icon::icon_elements_for_window(renderer, space, decorations, output, window, alpha)
}

fn rounded_rect_element(
    renderer: &mut GlesRenderer,
    decoration: &mut crate::ssd::WindowDecorationState,
    cached: &crate::ssd::CachedDecorationBuffer,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableRoundedElement>, GlesError> {
    if intersect_logical_rect(cached.rect, output_geo).is_none() {
        return Ok(None);
    }
    let local_rect = Rectangle::new(
        Point::from((
            cached.rect.x - output_geo.loc.x,
            cached.rect.y - output_geo.loc.y,
        )),
        (cached.rect.width, cached.rect.height).into(),
    );

    let clip = cached.clip_rect.map(|clip_rect| RoundedClip {
        rect: Rectangle::new(
            Point::from((clip_rect.x - cached.rect.x, clip_rect.y - cached.rect.y)),
            (clip_rect.width, clip_rect.height).into(),
        ),
        radius: cached.clip_radius,
    });

    let state = decoration
        .rounded_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let element = state.element(
        renderer,
        RoundedRectSpec {
            rect: local_rect,
            color: [
                cached.color.r as f32 / 255.0,
                cached.color.g as f32 / 255.0,
                cached.color.b as f32 / 255.0,
                cached.color.a as f32 / 255.0,
            ],
            alpha,
            radius: cached.radius,
            shape: if cached.border_width > 0 {
                RoundedShapeKind::Border {
                    width: cached.border_width,
                }
            } else {
                RoundedShapeKind::Fill
            },
            clip,
            render_scale: scale.x as f32,
        },
    )?;
    Ok(Some(element))
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
