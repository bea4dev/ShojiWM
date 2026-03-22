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
        if cached.shader.is_backdrop() {
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
            cached.rect.x - output_geo.loc.x,
            cached.rect.y - output_geo.loc.y,
        )),
        (cached.rect.width, cached.rect.height).into(),
    );

    let clip = cached.clip_rect.map(|clip_rect| RoundedClip {
        rect: Rectangle::new(
            Point::from((
                clip_rect.x - cached.rect.x,
                clip_rect.y - cached.rect.y,
            )),
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
            cached.rect.x - output_geo.loc.x,
            cached.rect.y - output_geo.loc.y,
        )),
        (cached.rect.width, cached.rect.height).into(),
    );

    let state = decoration
        .shader_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let element = state.element(
        renderer,
        ShaderEffectSpec {
            rect: local_rect,
            shader: cached.shader.clone(),
            alpha_bits: alpha.to_bits(),
            render_scale: scale.x as f32,
            clip_rect: cached.clip_rect.map(|clip_rect| {
                Rectangle::new(
                    Point::from((clip_rect.x, clip_rect.y)),
                    (clip_rect.width, clip_rect.height).into(),
                )
            }),
            clip_radius: cached.clip_radius,
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
