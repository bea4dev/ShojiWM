use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        gles::{GlesError, GlesRenderer},
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
};
use tracing::trace;

use crate::{
    backend::visual::{
        RectSnapMode, relative_physical_rect_from_root, snapped_logical_radius,
        snapped_logical_rect_for_element, snapped_logical_rect_from_relative_physical,
        snapped_logical_rect_in_element_space,
    },
    backend::rounded::{RoundedClip, RoundedRectSpec, RoundedShapeKind, StableRoundedElement},
    backend::shader_effect::{
        ShaderEffectError, ShaderEffectSpec, StableShaderEffectElement,
    },
    backend::text,
    ssd::{LogicalRect, WindowDecorationState},
};

smithay::render_elements! {
    pub DecorationSceneElements<=GlesRenderer>;
    Rounded=crate::backend::rounded::StableRoundedElement,
    Shader=crate::backend::shader_effect::StableShaderEffectElement,
    Backdrop=crate::backend::shader_effect::StableBackdropFramebufferElement,
}

#[derive(Debug, thiserror::Error)]
pub enum DecorationSceneError {
    #[error(transparent)]
    Gles(#[from] GlesError),
    #[error(transparent)]
    Shader(#[from] ShaderEffectError),
}

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

pub fn shader_elements_for_window(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<StableShaderEffectElement>, ShaderEffectError> {
    let buffers = decoration.shader_buffers.clone();
    buffers
        .iter()
        .filter_map(|cached| {
            shader_effect_element(renderer, decoration, cached, output_geo, scale, alpha).transpose()
        })
        .collect()
}

pub fn background_elements_for_window(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<DecorationSceneElements>, DecorationSceneError> {
    Ok(ordered_background_elements_for_window(renderer, decoration, output_geo, scale, alpha)?
        .into_iter()
        .map(|(_, element)| element)
        .collect())
}

pub fn ordered_background_elements_for_window(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, DecorationSceneElements)>, DecorationSceneError> {
    let mut items = Vec::new();

    for cached in decoration.buffers.clone() {
        if let Some(element) =
            rounded_rect_element(renderer, decoration, &cached, output_geo, scale, alpha)?
        {
            items.push((cached.order, DecorationSceneElements::Rounded(element)));
        }
    }

    for cached in decoration.shader_buffers.clone() {
        if cached.shader.is_texture_backed() {
            continue;
        }
        if let Some(element) =
            shader_effect_element(renderer, decoration, &cached, output_geo, scale, alpha)?
        {
            items.push((cached.order, DecorationSceneElements::Shader(element)));
        }
    }

    items.sort_by_key(|(order, _)| *order);
    Ok(items)
}

pub fn text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<crate::backend::text::DecorationTextureElements>, GlesError> {
    text::text_elements_for_window(renderer, space, decorations, output, window, alpha)
}

pub fn icon_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<crate::backend::text::DecorationTextureElements>, GlesError> {
    crate::backend::icon::icon_elements_for_window(renderer, space, decorations, output, window, alpha)
}

pub fn ordered_icon_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::icon::ordered_icon_elements_for_window(renderer, space, decorations, output, window, alpha)
}

pub fn ordered_icon_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::icon::ordered_icon_elements_for_decoration(renderer, decoration, output_geo, scale, alpha)
}

pub fn ordered_text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::text::ordered_text_elements_for_window(renderer, space, decorations, output, window, alpha)
}

pub fn ordered_text_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::text::ordered_text_elements_for_decoration(renderer, decoration, output_geo, scale, alpha)
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
            cached.rect.x - decoration.layout.root.rect.x,
            cached.rect.y - decoration.layout.root.rect.y,
        )),
        (cached.rect.width, cached.rect.height).into(),
    );
    let outer_radius = snapped_logical_radius(cached.radius, scale);
    let geometry = relative_physical_rect_from_root(
        cached.rect,
        decoration.layout.root.rect,
        output_geo,
        scale,
        cached.clip_rect,
    );
    let quantized_border_inner = (cached.border_width > 0 && !cached.shared_inner_hole)
        .then(|| {
        if let Some(hole_rect) = cached.hole_rect {
            let left = ((((hole_rect.x - cached.rect.x).max(0)) as f64) * scale.x.abs().max(0.0001))
                .round()
                / scale.x.abs().max(0.0001);
            let top = ((((hole_rect.y - cached.rect.y).max(0)) as f64) * scale.y.abs().max(0.0001))
                .round()
                / scale.y.abs().max(0.0001);
            let right = ((((cached.rect.x + cached.rect.width) - (hole_rect.x + hole_rect.width)).max(0) as f64)
                * scale.x.abs().max(0.0001))
                .round()
                / scale.x.abs().max(0.0001);
            let bottom = ((((cached.rect.y + cached.rect.height) - (hole_rect.y + hole_rect.height)).max(0) as f64)
                * scale.y.abs().max(0.0001))
                .round()
                / scale.y.abs().max(0.0001);
            RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: left as f32,
                    y: top as f32,
                    width: (cached.rect.width as f32 - left as f32 - right as f32).max(0.0),
                    height: (cached.rect.height as f32 - top as f32 - bottom as f32).max(0.0),
                },
                radius: snapped_logical_radius(cached.hole_radius, scale),
            }
        } else {
            let logical_border_width_x =
                ((((cached.border_width.max(0)) as f64) * scale.x.abs().max(0.0001)).round()
                    / scale.x.abs().max(0.0001)) as f32;
            let logical_border_width_y =
                ((((cached.border_width.max(0)) as f64) * scale.y.abs().max(0.0001)).round()
                    / scale.y.abs().max(0.0001)) as f32;
            let logical_border_radius = logical_border_width_x.max(logical_border_width_y);
            RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: logical_border_width_x,
                    y: logical_border_width_y,
                    width: (cached.rect.width as f32 - logical_border_width_x * 2.0).max(0.0),
                    height: (cached.rect.height as f32 - logical_border_width_y * 2.0).max(0.0),
                },
                radius: (outer_radius - logical_border_radius).max(0.0),
            }
        }
    });

    let clip = cached.clip_rect.map(|clip_rect| RoundedClip {
        rect: snapped_logical_rect_from_relative_physical(
            relative_physical_rect_from_root(
                clip_rect,
                cached.rect,
                output_geo,
                scale,
                Some(clip_rect),
            ),
            scale,
        ),
        radius: snapped_logical_radius(cached.clip_radius, scale),
    });
    let inner = quantized_border_inner.or_else(|| {
        cached.hole_rect.map(|hole_rect| RoundedClip {
            rect: snapped_logical_rect_from_relative_physical(
                relative_physical_rect_from_root(
                    hole_rect,
                    cached.rect,
                    output_geo,
                    scale,
                    Some(hole_rect),
                ),
                scale,
            ),
            radius: snapped_logical_radius(cached.hole_radius, scale),
        })
    });

    let state = decoration
        .rounded_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let element = state.element(
        renderer,
        RoundedRectSpec {
            rect: local_rect,
            geometry,
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
            inner,
            clip,
            render_scale: scale.x as f32,
        },
    )?;
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        tracing::info!(
            stable_key = %cached.stable_key,
            source_kind = %cached.source_kind,
            rect = ?cached.rect,
            local_rect = ?local_rect,
            border_width = cached.border_width,
            hole_rect = ?cached.hole_rect,
            clip_rect = ?cached.clip_rect,
            snapped_inner = ?inner,
            snapped_clip = ?clip,
            geometry = ?geometry,
            "gap debug rounded decoration element"
        );
    }
    Ok(Some(element))
}

fn shader_effect_element(
    renderer: &mut GlesRenderer,
    decoration: &mut crate::ssd::WindowDecorationState,
    cached: &crate::backend::shader_effect::CachedShaderEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableShaderEffectElement>, ShaderEffectError> {
    if intersect_logical_rect(cached.rect, output_geo).is_none() {
        return Ok(None);
    }

    let local_rect = Rectangle::new(
        Point::from((
            cached.rect.x - decoration.layout.root.rect.x,
            cached.rect.y - decoration.layout.root.rect.y,
        )),
        (cached.rect.width, cached.rect.height).into(),
    );
    let window_snap_origin = output_geo.loc;
    let geometry = relative_physical_rect_from_root(
        cached.rect,
        decoration.layout.root.rect,
        output_geo,
        scale,
        cached.clip_rect,
    );

    let state = decoration
        .shader_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let element = state.element(
        renderer,
        ShaderEffectSpec {
            rect: local_rect,
            geometry,
            shader: cached.shader.clone(),
            alpha_bits: alpha.to_bits(),
            render_scale: scale.x as f32,
            clip_rect: cached.clip_rect.map(|clip_rect| {
                snapped_logical_rect_in_element_space(
                    clip_rect,
                    cached.rect,
                    window_snap_origin,
                    scale,
                    RectSnapMode::OriginAndSize,
                )
            }),
            clip_radius: cached.clip_radius,
        },
    )?;
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        tracing::info!(
            stable_key = %cached.stable_key,
            rect = ?cached.rect,
            local_rect = ?local_rect,
            clip_rect = ?cached.clip_rect,
            snapped_clip = ?cached.clip_rect.map(|clip_rect| {
                snapped_logical_rect_for_element(
                    clip_rect,
                    Point::from((cached.rect.x, cached.rect.y)),
                    window_snap_origin,
                    scale,
                    RectSnapMode::SharedEdges,
                )
            }),
            geometry = ?geometry,
            "gap debug shader decoration element"
        );
    }
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
