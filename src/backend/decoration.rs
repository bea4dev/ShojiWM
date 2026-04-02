use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        gles::{GlesError, GlesRenderer},
    },
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Physical, Point, Rectangle, Scale},
};
use tracing::trace;

use crate::{
    backend::visual::{
        RectSnapMode, relative_physical_rect_from_root,
        relative_physical_rect_from_root_precise, snapped_logical_radius,
        snapped_precise_logical_rect_for_element, snapped_precise_logical_rect_in_element_space,
        relative_physical_rect_from_root_snapped_edges,
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

fn gap_disable_decoration_clip_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_DISABLE_DECORATION_CLIP").is_some()
}

fn gap_disable_border_inner_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_DISABLE_BORDER_INNER").is_some()
}

fn gap_disable_titlebar_clip_enabled(height: i32) -> bool {
    std::env::var_os("SHOJI_GAP_DISABLE_TITLEBAR_CLIP").is_some() && height == 30
}

fn gap_show_border_inner_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_SHOW_BORDER_INNER").is_some()
}

fn gap_show_titlebar_clip_enabled(height: i32) -> bool {
    std::env::var_os("SHOJI_GAP_SHOW_TITLEBAR_CLIP").is_some() && height == 30
}

fn gap_show_border_shell_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_SHOW_BORDER_SHELL").is_some()
}

fn gap_show_border_shell_only_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_SHOW_BORDER_SHELL_ONLY").is_some()
}

fn gap_shrink_border_hole_px() -> f32 {
    std::env::var_os("SHOJI_GAP_SHRINK_BORDER_HOLE")
        .and_then(|value| value.to_str().and_then(|value| value.parse::<f32>().ok()))
        .unwrap_or(0.0)
        .max(0.0)
}

fn shrink_rounded_clip_by_pixels(
    clip: RoundedClip,
    geometry: Rectangle<i32, Physical>,
    local_rect: Rectangle<i32, Logical>,
    shrink_px: f32,
) -> RoundedClip {
    if shrink_px <= 0.0 {
        return clip;
    }

    let geom_w = geometry.size.w.max(1) as f32;
    let geom_h = geometry.size.h.max(1) as f32;
    let local_w = local_rect.size.w.max(1) as f32;
    let local_h = local_rect.size.h.max(1) as f32;

    let shrink_x = shrink_px * local_w / geom_w;
    let shrink_y = shrink_px * local_h / geom_h;

    RoundedClip {
        rect: crate::backend::visual::SnappedLogicalRect {
            x: (clip.rect.x + shrink_x).min(local_w),
            y: (clip.rect.y + shrink_y).min(local_h),
            width: (clip.rect.width - shrink_x * 2.0).max(0.0),
            height: (clip.rect.height - shrink_y * 2.0).max(0.0),
        },
        radius: (clip.radius - shrink_x.max(shrink_y)).max(0.0),
    }
}

fn render_inner_clip_from_precise_anchors(
    outer_rect_precise: crate::backend::visual::PreciseLogicalRect,
    outer_geometry: Rectangle<i32, Physical>,
    inner_rect_precise: crate::backend::visual::PreciseLogicalRect,
    inner_radius: f32,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> RoundedClip {
    let inner_geometry = relative_physical_rect_from_root_precise(
        inner_rect_precise,
        root_rect,
        output_geo,
        scale,
    );
    let outer_width_px = outer_geometry.size.w.max(1) as f32;
    let outer_height_px = outer_geometry.size.h.max(1) as f32;
    let outer_width = outer_rect_precise.width.max(0.0001);
    let outer_height = outer_rect_precise.height.max(0.0001);

    RoundedClip {
        rect: crate::backend::visual::SnappedLogicalRect {
            x: (inner_geometry.loc.x - outer_geometry.loc.x).max(0) as f32 * outer_width
                / outer_width_px,
            y: (inner_geometry.loc.y - outer_geometry.loc.y).max(0) as f32 * outer_height
                / outer_height_px,
            width: inner_geometry.size.w.max(0) as f32 * outer_width / outer_width_px,
            height: inner_geometry.size.h.max(0) as f32 * outer_height / outer_height_px,
        },
        radius: inner_radius.max(0.0),
    }
}

fn union_physical_rect(
    current: Option<Rectangle<i32, Physical>>,
    rect: Rectangle<i32, Physical>,
) -> Rectangle<i32, Physical> {
    if let Some(current) = current {
        let left = current.loc.x.min(rect.loc.x);
        let top = current.loc.y.min(rect.loc.y);
        let right = (current.loc.x + current.size.w).max(rect.loc.x + rect.size.w);
        let bottom = (current.loc.y + current.size.h).max(rect.loc.y + rect.size.h);
        Rectangle::new(
            Point::from((left, top)),
            ((right - left).max(0), (bottom - top).max(0)).into(),
        )
    } else {
        rect
    }
}

fn is_descendant_owner(owner: &str, parent: &str) -> bool {
    owner.len() > parent.len()
        && owner.starts_with(parent)
        && owner.as_bytes().get(parent.len()) == Some(&b'.')
}

fn bordered_node_anchor_union_geometry(
    decoration: &crate::ssd::WindowDecorationState,
    border_stable_key: &str,
    border_owner_id: Option<&str>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Option<Rectangle<i32, Physical>> {
    let mut union = None;
    let path_prefix = border_stable_key
        .split_once(':')
        .map(|(path, _)| format!("{path}/"));

    for buffer in &decoration.buffers {
        let is_descendant = path_prefix
            .as_deref()
            .is_some_and(|prefix| buffer.stable_key.starts_with(prefix))
            || border_owner_id.zip(buffer.owner_node_id.as_deref()).is_some_and(|(parent, owner)| {
                is_descendant_owner(owner, parent)
            });
        if !is_descendant {
            continue;
        }
        let rect = buffer
            .rect_precise
            .map(|rect| {
                relative_physical_rect_from_root_precise(
                    rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            })
            .unwrap_or_else(|| {
                relative_physical_rect_from_root_snapped_edges(
                    buffer.rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            });
        union = Some(union_physical_rect(union, rect));
    }

    for buffer in &decoration.shader_buffers {
        let is_descendant = path_prefix
            .as_deref()
            .is_some_and(|prefix| buffer.stable_key.starts_with(prefix))
            || border_owner_id.zip(buffer.owner_node_id.as_deref()).is_some_and(|(parent, owner)| {
                is_descendant_owner(owner, parent)
            });
        if !is_descendant {
            continue;
        }
        let rect = buffer
            .rect_precise
            .map(|rect| {
                relative_physical_rect_from_root_precise(
                    rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            })
            .unwrap_or_else(|| {
                relative_physical_rect_from_root_snapped_edges(
                    buffer.rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            });
        union = Some(union_physical_rect(union, rect));
    }

    for buffer in &decoration.text_buffers {
        let is_descendant = border_owner_id.zip(buffer.owner_node_id.as_deref()).is_some_and(|(parent, owner)| {
            is_descendant_owner(owner, parent)
        });
        if !is_descendant {
            continue;
        }
        let rect = buffer
            .rect_precise
            .map(|rect| {
                relative_physical_rect_from_root_precise(
                    rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            })
            .unwrap_or_else(|| {
                relative_physical_rect_from_root_snapped_edges(
                    buffer.rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            });
        union = Some(union_physical_rect(union, rect));
    }

    for buffer in &decoration.icon_buffers {
        let is_descendant = border_owner_id.zip(buffer.owner_node_id.as_deref()).is_some_and(|(parent, owner)| {
            is_descendant_owner(owner, parent)
        });
        if !is_descendant {
            continue;
        }
        let rect = buffer
            .rect_precise
            .map(|rect| {
                relative_physical_rect_from_root_precise(
                    rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            })
            .unwrap_or_else(|| {
                relative_physical_rect_from_root_snapped_edges(
                    buffer.rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            });
        union = Some(union_physical_rect(union, rect));
    }

    if let Some(content_clip) = decoration.content_clip {
        let rect = relative_physical_rect_from_root_precise(
            content_clip.rect_precise,
            decoration.layout.root.rect,
            output_geo,
            scale,
        );
        union = Some(union_physical_rect(union, rect));
    }

    union
}

fn align_shared_clip_edges_to_outer(
    mut clip: RoundedClip,
    clip_rect_precise: Option<crate::backend::visual::PreciseLogicalRect>,
    clip_rect_logical: Option<LogicalRect>,
    outer_rect_precise: crate::backend::visual::PreciseLogicalRect,
    outer_rect_logical: LogicalRect,
    scale: Scale<f64>,
) -> RoundedClip {
    let epsilon_x = (1.0 / scale.x.abs().max(0.0001)) as f32;
    let epsilon_y = (1.0 / scale.y.abs().max(0.0001)) as f32;
    let local_right = outer_rect_precise.width.max(0.0);
    let local_bottom = outer_rect_precise.height.max(0.0);

    let shared_left = clip_rect_precise
        .map(|rect| (rect.x - outer_rect_precise.x).abs() <= epsilon_x)
        .or_else(|| clip_rect_logical.map(|rect| rect.x == outer_rect_logical.x))
        .unwrap_or(false);
    let shared_top = clip_rect_precise
        .map(|rect| (rect.y - outer_rect_precise.y).abs() <= epsilon_y)
        .or_else(|| clip_rect_logical.map(|rect| rect.y == outer_rect_logical.y))
        .unwrap_or(false);
    let shared_right = clip_rect_precise
        .map(|rect| {
            ((rect.x + rect.width) - (outer_rect_precise.x + outer_rect_precise.width)).abs()
                <= epsilon_x
        })
        .or_else(|| {
            clip_rect_logical.map(|rect| {
                rect.x + rect.width == outer_rect_logical.x + outer_rect_logical.width
            })
        })
        .unwrap_or(false);
    let shared_bottom = clip_rect_precise
        .map(|rect| {
            ((rect.y + rect.height) - (outer_rect_precise.y + outer_rect_precise.height)).abs()
                <= epsilon_y
        })
        .or_else(|| {
            clip_rect_logical.map(|rect| {
                rect.y + rect.height == outer_rect_logical.y + outer_rect_logical.height
            })
        })
        .unwrap_or(false);

    if shared_left {
        clip.rect.x = 0.0;
    }
    if shared_top {
        clip.rect.y = 0.0;
    }
    if shared_right {
        clip.rect.width = (local_right - clip.rect.x).max(0.0);
    }
    if shared_bottom {
        clip.rect.height = (local_bottom - clip.rect.y).max(0.0);
    }

    clip
}

fn clip_contains_local_rect(
    clip: RoundedClip,
    local_rect: Rectangle<i32, Logical>,
) -> bool {
    let epsilon = 1.0;
    let local_right = local_rect.size.w.max(0) as f32;
    let local_bottom = local_rect.size.h.max(0) as f32;
    clip.rect.x <= epsilon
        && clip.rect.y <= epsilon
        && clip.rect.x + clip.rect.width >= local_right - epsilon
        && clip.rect.y + clip.rect.height >= local_bottom - epsilon
}

fn corner_radii_from_clip(
    fallback_radius: f32,
    clip: RoundedClip,
    local_rect: Rectangle<i32, Logical>,
) -> [f32; 4] {
    let mut radii = [fallback_radius; 4];
    let epsilon = 1.0;
    let local_right = local_rect.size.w.max(0) as f32;
    let local_bottom = local_rect.size.h.max(0) as f32;

    let shares_left = clip.rect.x.abs() <= epsilon;
    let shares_top = clip.rect.y.abs() <= epsilon;
    let shares_right = ((clip.rect.x + clip.rect.width) - local_right).abs() <= epsilon;
    let shares_bottom = ((clip.rect.y + clip.rect.height) - local_bottom).abs() <= epsilon;

    if shares_top && shares_left {
        radii[0] = clip.radius.max(radii[0]);
    }
    if shares_top && shares_right {
        radii[1] = clip.radius.max(radii[1]);
    }
    if shares_bottom && shares_right {
        radii[2] = clip.radius.max(radii[2]);
    }
    if shares_bottom && shares_left {
        radii[3] = clip.radius.max(radii[3]);
    }

    radii
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
    if gap_show_border_shell_only_enabled() && cached.source_kind != "window-border" {
        return Ok(None);
    }
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
    let snapped_radius_f32 = |radius: f32| {
        let scale_x = scale.x.abs().max(0.0001) as f32;
        ((radius.max(0.0) * scale_x).round() / scale_x).max(0.0)
    };
    let outer_radius = snapped_radius_f32(cached.radius_precise.unwrap_or(cached.radius as f32));
    let border_fit = cached
        .owner_node_id
        .as_deref()
        .and_then(|owner_id| {
            decoration
                .layout
                .root
                .stable_id
                .as_deref()
                .filter(|root_id| *root_id == owner_id)
                .map(|_| decoration.layout.root.style.effective_border_fit(&decoration.layout.root.kind))
        })
        .unwrap_or_else(|| {
            if cached.source_kind == "window-border" {
                crate::ssd::BorderFit::FitChildren
            } else {
                crate::ssd::BorderFit::Normal
            }
        });
    let geometry = cached
        .rect_precise
        .map(|rect| {
            relative_physical_rect_from_root_precise(
                rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            )
        })
        .unwrap_or_else(|| {
            relative_physical_rect_from_root_snapped_edges(
                cached.rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            )
        });
    let outer_rect_precise = cached.rect_precise.unwrap_or(crate::backend::visual::PreciseLogicalRect {
        x: cached.rect.x as f32,
        y: cached.rect.y as f32,
        width: cached.rect.width as f32,
        height: cached.rect.height as f32,
    });
    let hole_geometry = cached.hole_rect_precise.map(|hole_rect| {
        relative_physical_rect_from_root_precise(
            hole_rect,
            decoration.layout.root.rect,
            output_geo,
            scale,
        )
    }).or_else(|| cached.hole_rect.map(|hole_rect| {
        relative_physical_rect_from_root(
            hole_rect,
            decoration.layout.root.rect,
            output_geo,
            scale,
            Some(hole_rect),
        )
    }));
    let border_anchor_hole_geometry =
        (matches!(border_fit, crate::ssd::BorderFit::FitChildren) && cached.shared_inner_hole && cached.border_width > 0.0)
            .then(|| {
                bordered_node_anchor_union_geometry(
                    decoration,
                    &cached.stable_key,
                    cached.owner_node_id.as_deref(),
                    output_geo,
                    scale,
                )
            })
            .flatten();
    let border_anchor_hole_precise = border_anchor_hole_geometry.map(|hole_geometry| {
        let outer_width_px = geometry.size.w.max(1) as f32;
        let outer_height_px = geometry.size.h.max(1) as f32;
        crate::backend::visual::PreciseLogicalRect {
            x: outer_rect_precise.x
                + (hole_geometry.loc.x - geometry.loc.x).max(0) as f32
                    * outer_rect_precise.width.max(0.0001)
                    / outer_width_px,
            y: outer_rect_precise.y
                + (hole_geometry.loc.y - geometry.loc.y).max(0) as f32
                    * outer_rect_precise.height.max(0.0001)
                    / outer_height_px,
            width: hole_geometry.size.w.max(0) as f32 * outer_rect_precise.width.max(0.0001)
                / outer_width_px,
            height: hole_geometry.size.h.max(0) as f32 * outer_rect_precise.height.max(0.0001)
                / outer_height_px,
        }
    });
    let anchored_border_inner = (matches!(border_fit, crate::ssd::BorderFit::FitChildren) && cached.shared_inner_hole && cached.border_width > 0.0)
        .then(|| {
            border_anchor_hole_precise.map(|hole_rect| {
                render_inner_clip_from_precise_anchors(
                    outer_rect_precise,
                    geometry,
                    hole_rect,
                    snapped_radius_f32(
                        cached.hole_radius_precise.unwrap_or(cached.hole_radius as f32),
                    ),
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                )
            })
        })
        .flatten();
    let quantized_border_inner = anchored_border_inner.or_else(|| (cached.border_width > 0.0 && !cached.shared_inner_hole)
        .then(|| {
        if let Some(hole_rect) = cached.hole_rect {
            RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: (hole_rect.x - cached.rect.x).max(0) as f32,
                    y: (hole_rect.y - cached.rect.y).max(0) as f32,
                    width: hole_rect.width.max(0) as f32,
                    height: hole_rect.height.max(0) as f32,
                },
                radius: snapped_radius_f32(
                    cached.hole_radius_precise.unwrap_or(cached.hole_radius as f32),
                ),
            }
        } else if let Some(hole_rect) = cached.hole_rect_precise {
            let outer_geometry = geometry;
            let hole_geometry = relative_physical_rect_from_root_precise(
                hole_rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            );
            let left_px = (hole_geometry.loc.x - outer_geometry.loc.x).max(0);
            let top_px = (hole_geometry.loc.y - outer_geometry.loc.y).max(0);
            let outer_width_px = outer_geometry.size.w.max(1) as f32;
            let outer_height_px = outer_geometry.size.h.max(1) as f32;
            RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: left_px as f32 * cached.rect.width.max(1) as f32 / outer_width_px,
                    y: top_px as f32 * cached.rect.height.max(1) as f32 / outer_height_px,
                    width: hole_geometry.size.w.max(0) as f32
                        * cached.rect.width.max(1) as f32
                        / outer_width_px,
                    height: hole_geometry.size.h.max(0) as f32
                        * cached.rect.height.max(1) as f32
                        / outer_height_px,
                },
                radius: snapped_radius_f32(
                    cached.hole_radius_precise.unwrap_or(cached.hole_radius as f32),
                ),
            }
        } else {
            let logical_border_width_x =
                (((cached.border_width.max(0.0) as f64) * scale.x.abs().max(0.0001)).round()
                    / scale.x.abs().max(0.0001)) as f32;
            let logical_border_width_y =
                (((cached.border_width.max(0.0) as f64) * scale.y.abs().max(0.0001)).round()
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
    }));

    let clip = if (gap_disable_decoration_clip_enabled() && cached.source_kind != "window-border")
        || gap_disable_titlebar_clip_enabled(cached.rect.height)
    {
        None
    } else {
        cached.clip_rect_precise
            .map(|clip_rect| {
                align_shared_clip_edges_to_outer(
                    RoundedClip {
                        rect: snapped_precise_logical_rect_in_element_space(
                            clip_rect,
                            outer_rect_precise,
                            scale,
                        ),
                        radius: snapped_radius_f32(
                            cached.clip_radius_precise.unwrap_or(cached.clip_radius as f32),
                        ),
                    },
                    Some(clip_rect),
                    None,
                    outer_rect_precise,
                    cached.rect,
                    scale,
                )
            })
            .or_else(|| cached.clip_rect.map(|clip_rect| {
                align_shared_clip_edges_to_outer(
                    RoundedClip {
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
                    },
                    None,
                    Some(clip_rect),
                    outer_rect_precise,
                    cached.rect,
                    scale,
                )
            }))
    };
    let inner = quantized_border_inner.or_else(|| {
        cached.hole_rect_precise.map(|hole_rect| RoundedClip {
            rect: snapped_precise_logical_rect_in_element_space(
                hole_rect,
                outer_rect_precise,
                scale,
            ),
            radius: snapped_radius_f32(
                cached.hole_radius_precise.unwrap_or(cached.hole_radius as f32),
            ),
        }).or_else(|| cached.hole_rect.map(|hole_rect| RoundedClip {
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
            radius: snapped_radius_f32(
                cached.hole_radius_precise.unwrap_or(cached.hole_radius as f32),
            ),
        }))
    });
    let inner = if cached.source_kind == "window-border" {
        inner.map(|clip| {
            shrink_rounded_clip_by_pixels(
                clip,
                geometry,
                local_rect,
                gap_shrink_border_hole_px(),
            )
        })
    } else {
        inner
    };
    let expected_inner = inner;
    let derived_inner = (cached.border_width > 0.0).then(|| RoundedClip {
        rect: crate::backend::visual::SnappedLogicalRect {
            x: cached.border_width.max(0.0),
            y: cached.border_width.max(0.0),
            width: (local_rect.size.w as f32 - cached.border_width.max(0.0) * 2.0).max(0.0),
            height: (local_rect.size.h as f32 - cached.border_width.max(0.0) * 2.0).max(0.0),
        },
        radius: (outer_radius - cached.border_width.max(0.0)).max(0.0),
    });
    let inner = if gap_disable_border_inner_enabled() && cached.source_kind == "window-border" {
        None
    } else {
        inner
    };
    let use_physical_anchor_space = cached.border_width > 0.0;
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;
    let shader_rect = if use_physical_anchor_space {
        Rectangle::new(
            Point::from((0, 0)),
            (geometry.size.w.max(1), geometry.size.h.max(1)).into(),
        )
    } else {
        local_rect
    };
    let render_outer_radius = if use_physical_anchor_space {
        (outer_radius * scale_x).round().max(0.0)
    } else {
        outer_radius
    };
    let render_border_width = if use_physical_anchor_space {
        let border_width_x = (cached.border_width.max(0.0) * scale_x).round().max(0.0);
        let border_width_y = (cached.border_width.max(0.0) * scale_y).round().max(0.0);
        border_width_x.max(border_width_y)
    } else {
        cached.border_width
    };
    let render_inner = if use_physical_anchor_space {
        let border_width_x = (cached.border_width.max(0.0) * scale_x).round().max(0.0);
        let border_width_y = (cached.border_width.max(0.0) * scale_y).round().max(0.0);
        let derived_render_inner = || RoundedClip {
            rect: crate::backend::visual::SnappedLogicalRect {
                x: border_width_x,
                y: border_width_y,
                width: (geometry.size.w as f32 - border_width_x * 2.0).max(0.0),
                height: (geometry.size.h as f32 - border_width_y * 2.0).max(0.0),
            },
            radius: (render_outer_radius - border_width_x.max(border_width_y)).max(0.0),
        };

        if matches!(border_fit, crate::ssd::BorderFit::Normal) {
            Some(derived_render_inner())
        } else {
            border_anchor_hole_geometry.or(hole_geometry).map(|hole_geometry| RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: (hole_geometry.loc.x - geometry.loc.x).max(0) as f32,
                    y: (hole_geometry.loc.y - geometry.loc.y).max(0) as f32,
                    width: hole_geometry.size.w.max(0) as f32,
                    height: hole_geometry.size.h.max(0) as f32,
                },
                radius: ((cached
                    .hole_radius_precise
                    .unwrap_or(cached.hole_radius as f32)
                    .max(0.0)
                    * scale_x)
                    .round()
                    .max(0.0)),
            })
            .or_else(|| Some(derived_render_inner()))
        }
    } else {
        inner
    };
    let render_clip = if use_physical_anchor_space {
        cached.clip_rect_precise.map(|clip_rect| {
            let clip_geometry = relative_physical_rect_from_root_precise(
                clip_rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            );
            RoundedClip {
                rect: crate::backend::visual::SnappedLogicalRect {
                    x: (clip_geometry.loc.x - geometry.loc.x).max(0) as f32,
                    y: (clip_geometry.loc.y - geometry.loc.y).max(0) as f32,
                    width: clip_geometry.size.w.max(0) as f32,
                    height: clip_geometry.size.h.max(0) as f32,
                },
                radius: (cached.clip_radius_precise.unwrap_or(cached.clip_radius as f32)
                    * scale_x)
                    .round()
                    .max(0.0),
            }
        }).or_else(|| {
            cached.clip_rect.map(|clip_rect| {
                let clip_geometry = relative_physical_rect_from_root(
                    clip_rect,
                    decoration.layout.root.rect,
                    output_geo,
                    scale,
                    Some(clip_rect),
                );
                RoundedClip {
                    rect: crate::backend::visual::SnappedLogicalRect {
                        x: (clip_geometry.loc.x - geometry.loc.x).max(0) as f32,
                        y: (clip_geometry.loc.y - geometry.loc.y).max(0) as f32,
                        width: clip_geometry.size.w.max(0) as f32,
                        height: clip_geometry.size.h.max(0) as f32,
                    },
                    radius: (cached.clip_radius.max(0) as f32 * scale_x).round().max(0.0),
                }
            })
        })
    } else {
        clip
    };
    let shader_outer_rect = if cached.border_width > 0.0 {
        if let Some(inner) = render_inner {
            let thickness = render_border_width.max(0.0);
            let mut left = (inner.rect.x - thickness).max(0.0);
            let mut top = (inner.rect.y - thickness).max(0.0);
            let mut right =
                (inner.rect.x + inner.rect.width + thickness).min(shader_rect.size.w as f32);
            let mut bottom =
                (inner.rect.y + inner.rect.height + thickness).min(shader_rect.size.h as f32);
            if left <= 1.0 {
                left = 0.0;
            }
            if top <= 1.0 {
                top = 0.0;
            }
            if (shader_rect.size.w as f32 - right).abs() <= 1.0 {
                right = shader_rect.size.w as f32;
            }
            if (shader_rect.size.h as f32 - bottom).abs() <= 1.0 {
                bottom = shader_rect.size.h as f32;
            }
            [
                left,
                top,
                (right - left).max(0.0),
                (bottom - top).max(0.0),
            ]
        } else {
            [
                0.0,
                0.0,
                shader_rect.size.w as f32,
                shader_rect.size.h as f32,
            ]
        }
    } else {
        [
            0.0,
            0.0,
            shader_rect.size.w as f32,
            shader_rect.size.h as f32,
        ]
    };
    let prefer_derived_inner =
        matches!(border_fit, crate::ssd::BorderFit::FitChildren) && cached.shared_inner_hole && render_inner.is_none();
    let inner_mode = if cached.border_width > 0.0 {
        if prefer_derived_inner {
            crate::backend::rounded::RoundedInnerMode::DerivedInset
        } else {
            render_inner
                .map(crate::backend::rounded::RoundedInnerMode::Explicit)
                .unwrap_or(crate::backend::rounded::RoundedInnerMode::DerivedInset)
        }
    } else {
        crate::backend::rounded::RoundedInnerMode::None
    };

    let state = decoration
        .rounded_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let outer_render_scale = if use_physical_anchor_space {
        1.0
    } else {
        geometry.size.w.max(1) as f32 / local_rect.size.w.max(1) as f32
    };
    let inner_render_scale = match (render_inner, hole_geometry) {
        (Some(inner), Some(hole_geometry)) => {
            if use_physical_anchor_space {
                1.0
            } else {
                hole_geometry.size.w.max(1) as f32 / inner.rect.width.max(0.0001)
            }
        }
        (Some(inner), None) => {
            if use_physical_anchor_space {
                1.0
            } else {
                (inner.rect.width * scale_x).round().max(1.0) / inner.rect.width.max(0.0001)
            }
        }
        _ => outer_render_scale,
    };
    let clip_geometry = cached.clip_rect_precise.map(|clip_rect| {
        relative_physical_rect_from_root_precise(
            clip_rect,
            decoration.layout.root.rect,
            output_geo,
            scale,
        )
    }).or_else(|| cached.clip_rect.map(|clip_rect| {
        relative_physical_rect_from_root(
            clip_rect,
            decoration.layout.root.rect,
            output_geo,
            scale,
            Some(clip_rect),
        )
    }));
    let clip_render_scale = match (render_clip, clip_geometry) {
        (Some(clip), Some(clip_geometry)) => {
            if use_physical_anchor_space {
                1.0
            } else {
                clip_geometry.size.w.max(1) as f32 / clip.rect.width.max(0.0001)
            }
        }
        (Some(clip), None) => {
            if use_physical_anchor_space {
                1.0
            } else {
                let scale_x = scale.x.abs().max(0.0001) as f32;
                (clip.rect.width * scale_x).round().max(1.0) / clip.rect.width.max(0.0001)
            }
        }
        _ => outer_render_scale,
    };
    let corner_radii = if use_physical_anchor_space {
        [render_outer_radius, render_outer_radius, render_outer_radius, render_outer_radius]
    } else if cached.border_width <= 0.0 {
        render_clip.map(|clip| corner_radii_from_clip(outer_radius, clip, local_rect))
            .unwrap_or([outer_radius, outer_radius, outer_radius, outer_radius])
    } else {
        [outer_radius, outer_radius, outer_radius, outer_radius]
    };
    let clip = if cached.border_width <= 0.0 {
        render_clip.filter(|clip| !clip_contains_local_rect(*clip, local_rect))
    } else {
        render_clip
    };
    let spec = RoundedRectSpec {
        rect: local_rect,
        shader_rect,
        shader_outer_rect,
        geometry,
        color: [
            cached.color.r as f32 / 255.0,
            cached.color.g as f32 / 255.0,
            cached.color.b as f32 / 255.0,
            cached.color.a as f32 / 255.0,
        ],
        alpha,
        radius: render_outer_radius,
        corner_radii,
        shape: if cached.border_width > 0.0 {
            RoundedShapeKind::Border {
                width: render_border_width,
            }
        } else {
            RoundedShapeKind::Fill
        },
        inner_mode,
        clip,
        outer_render_scale,
        inner_render_scale,
        clip_render_scale,
        debug_inner_only: if cached.source_kind == "window-border" && gap_show_border_inner_enabled() {
            1.0
        } else {
            0.0
        },
        debug_clip_only: if cached.source_kind == "fill" && gap_show_titlebar_clip_enabled(cached.rect.height) {
            1.0
        } else {
            0.0
        },
        debug_shell_only: if cached.source_kind == "window-border" && gap_show_border_shell_enabled() {
            1.0
        } else {
            0.0
        },
    };
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        tracing::info!(
            stable_key = %cached.stable_key,
            source_kind = %cached.source_kind,
            spec = ?spec,
            "gap debug rounded decoration spec"
        );
    }
    let element = state.element(renderer, spec)?;
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        let root_local_rect_precise = cached.rect_precise.map(|rect| crate::backend::visual::PreciseLogicalRect {
            x: rect.x - decoration.layout.root.rect.x as f32,
            y: rect.y - decoration.layout.root.rect.y as f32,
            width: rect.width,
            height: rect.height,
        });
        let root_local_clip_precise = cached.clip_rect_precise.map(|rect| crate::backend::visual::PreciseLogicalRect {
            x: rect.x - decoration.layout.root.rect.x as f32,
            y: rect.y - decoration.layout.root.rect.y as f32,
            width: rect.width,
            height: rect.height,
        });
        let to_physical = |inner: RoundedClip| {
            let scale_x = scale.x.abs().max(0.0001) as f32;
            let scale_y = scale.y.abs().max(0.0001) as f32;
            let left = (inner.rect.x * scale_x).round() as i32;
            let top = (inner.rect.y * scale_y).round() as i32;
            let right = ((inner.rect.x + inner.rect.width) * scale_x).round() as i32;
            let bottom = ((inner.rect.y + inner.rect.height) * scale_y).round() as i32;
            smithay::utils::Rectangle::<i32, smithay::utils::Physical>::new(
                smithay::utils::Point::<i32, smithay::utils::Physical>::from((left, top)),
                ((right - left).max(0), (bottom - top).max(0)).into(),
            )
        };
        let to_physical_precise = |inner: RoundedClip| {
            let scale_x = scale.x.abs().max(0.0001) as f32;
            let scale_y = scale.y.abs().max(0.0001) as f32;
            (
                inner.rect.x * scale_x,
                inner.rect.y * scale_y,
                inner.rect.width * scale_x,
                inner.rect.height * scale_y,
            )
        };
        let offset_physical =
            |rect: smithay::utils::Rectangle<i32, smithay::utils::Physical>| {
                smithay::utils::Rectangle::<i32, smithay::utils::Physical>::new(
                    smithay::utils::Point::<i32, smithay::utils::Physical>::from((
                        geometry.loc.x + rect.loc.x,
                        geometry.loc.y + rect.loc.y,
                    )),
                    rect.size,
                )
            };
        let inner_physical = inner.map(to_physical);
        let expected_inner_physical = expected_inner.map(to_physical);
        let derived_inner_physical = derived_inner.map(to_physical);
        let expected_inner_physical_precise = expected_inner.map(to_physical_precise);
        let derived_inner_physical_precise = derived_inner.map(to_physical_precise);
        let expected_inner_physical_global_precise = expected_inner_physical_precise.map(|rect| {
            (
                geometry.loc.x as f32 + rect.0,
                geometry.loc.y as f32 + rect.1,
                rect.2,
                rect.3,
            )
        });
        let derived_inner_physical_global_precise = derived_inner_physical_precise.map(|rect| {
            (
                geometry.loc.x as f32 + rect.0,
                geometry.loc.y as f32 + rect.1,
                rect.2,
                rect.3,
            )
        });
        let expected_inner_physical_global = expected_inner_physical.map(offset_physical);
        let derived_inner_physical_global = derived_inner_physical.map(offset_physical);
        let clip_physical = clip.map(to_physical);
        let clip_physical_precise = clip.map(to_physical_precise);
        let clip_physical_global_precise = clip_physical_precise.map(|rect| {
            (
                geometry.loc.x as f32 + rect.0,
                geometry.loc.y as f32 + rect.1,
                rect.2,
                rect.3,
            )
        });
        let clip_physical_global = clip_physical.map(offset_physical);
        let derived_vs_expected_delta = match (derived_inner_physical, expected_inner_physical) {
            (Some(derived), Some(expected)) => Some((
                derived.loc.x - expected.loc.x,
                derived.loc.y - expected.loc.y,
                derived.size.w - expected.size.w,
                derived.size.h - expected.size.h,
            )),
            _ => None,
        };
        let border_physical = inner_physical.map(|inner| {
            (
                inner.loc.x,
                inner.loc.y,
                geometry.size.w - (inner.loc.x + inner.size.w),
                geometry.size.h - (inner.loc.y + inner.size.h),
            )
        });
        let geometry_right = geometry.loc.x + geometry.size.w;
        let geometry_bottom = geometry.loc.y + geometry.size.h;
        let expected_inner_right_global = expected_inner_physical_global
            .map(|rect| rect.loc.x + rect.size.w);
        let expected_inner_bottom_global = expected_inner_physical_global
            .map(|rect| rect.loc.y + rect.size.h);
        let derived_inner_right_global = derived_inner_physical_global
            .map(|rect| rect.loc.x + rect.size.w);
        let derived_inner_bottom_global = derived_inner_physical_global
            .map(|rect| rect.loc.y + rect.size.h);
        let clip_right_global = clip_physical_global.map(|rect| rect.loc.x + rect.size.w);
        let clip_bottom_global = clip_physical_global.map(|rect| rect.loc.y + rect.size.h);
        tracing::info!(
            stable_key = %cached.stable_key,
            source_kind = %cached.source_kind,
            rect = ?cached.rect,
            rect_precise = ?cached.rect_precise,
            root_local_rect_precise = ?root_local_rect_precise,
            local_rect = ?local_rect,
            border_width = cached.border_width,
            hole_rect = ?cached.hole_rect,
            hole_rect_precise = ?cached.hole_rect_precise,
            radius = cached.radius,
            radius_precise = ?cached.radius_precise,
            hole_radius = cached.hole_radius,
            hole_radius_precise = ?cached.hole_radius_precise,
            clip_rect = ?cached.clip_rect,
            expected_inner = ?expected_inner,
            expected_inner_physical = ?expected_inner_physical,
            expected_inner_physical_precise = ?expected_inner_physical_precise,
            expected_inner_physical_global_precise = ?expected_inner_physical_global_precise,
            expected_inner_physical_global = ?expected_inner_physical_global,
            derived_inner = ?derived_inner,
            derived_inner_physical = ?derived_inner_physical,
            derived_inner_physical_precise = ?derived_inner_physical_precise,
            derived_inner_physical_global_precise = ?derived_inner_physical_global_precise,
            derived_inner_physical_global = ?derived_inner_physical_global,
            geometry_right,
            geometry_bottom,
            expected_inner_right_global,
            expected_inner_bottom_global,
            derived_inner_right_global,
            derived_inner_bottom_global,
            expected_inner_right_gap_px = expected_inner_right_global.map(|right| geometry_right - right),
            expected_inner_bottom_gap_px = expected_inner_bottom_global.map(|bottom| geometry_bottom - bottom),
            derived_inner_right_gap_px = derived_inner_right_global.map(|right| geometry_right - right),
            derived_inner_bottom_gap_px = derived_inner_bottom_global.map(|bottom| geometry_bottom - bottom),
            derived_vs_expected_delta = ?derived_vs_expected_delta,
            snapped_inner = ?inner,
            inner_physical = ?inner_physical,
            border_physical = ?border_physical,
            snapped_clip = ?clip,
            root_local_clip_precise = ?root_local_clip_precise,
            clip_physical = ?clip_physical,
            clip_physical_precise = ?clip_physical_precise,
            clip_physical_global_precise = ?clip_physical_global_precise,
            clip_physical_global = ?clip_physical_global,
            clip_right_global,
            clip_bottom_global,
            clip_right_gap_px = clip_right_global.map(|right| geometry_right - right),
            clip_bottom_gap_px = clip_bottom_global.map(|bottom| geometry_bottom - bottom),
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
    if gap_show_border_shell_only_enabled() {
        return Ok(None);
    }
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
    let geometry = cached
        .rect_precise
        .map(|rect| {
            relative_physical_rect_from_root_precise(
                rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            )
        })
        .unwrap_or_else(|| {
            relative_physical_rect_from_root_snapped_edges(
                cached.rect,
                decoration.layout.root.rect,
                output_geo,
                scale,
            )
        });

    let state = decoration
        .shader_cache
        .entry(cached.stable_key.clone())
        .or_default();
    let render_scale = geometry.size.w.max(1) as f32 / local_rect.size.w.max(1) as f32;
    let spec = ShaderEffectSpec {
        rect: local_rect,
        geometry,
        shader: cached.shader.clone(),
        alpha_bits: alpha.to_bits(),
        render_scale,
            clip_rect: if gap_disable_decoration_clip_enabled()
                || gap_disable_titlebar_clip_enabled(cached.rect.height)
            {
                None
            } else {
            cached.clip_rect.map(|clip_rect| {
                snapped_logical_rect_in_element_space(
                    clip_rect,
                    cached.rect,
                    window_snap_origin,
                    scale,
                    RectSnapMode::OriginAndSize,
                )
            }).or_else(|| {
            cached.rect_precise.zip(cached.clip_rect_precise).map(|(rect_precise, clip_rect)| {
                    snapped_precise_logical_rect_for_element(
                        clip_rect,
                        rect_precise,
                        window_snap_origin,
                        scale,
                    )
                })
            })
        },
            clip_radius: if gap_disable_decoration_clip_enabled()
                || gap_disable_titlebar_clip_enabled(cached.rect.height)
            {
                0
            } else {
            cached
                .clip_radius_precise
                .map(|radius| radius.round() as i32)
                .unwrap_or(cached.clip_radius)
        },
    };
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        tracing::info!(
            stable_key = %cached.stable_key,
            spec = ?spec,
            "gap debug shader decoration spec"
        );
    }
    let debug_clip_rect = spec.clip_rect;
    let element = state.element(renderer, spec)?;
    if std::env::var_os("SHOJI_GAP_DEBUG").is_some() {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        let root_local_rect_precise = cached.rect_precise.map(|rect| crate::backend::visual::PreciseLogicalRect {
            x: rect.x - decoration.layout.root.rect.x as f32,
            y: rect.y - decoration.layout.root.rect.y as f32,
            width: rect.width,
            height: rect.height,
        });
        let root_local_clip_precise = cached.clip_rect_precise.map(|rect| crate::backend::visual::PreciseLogicalRect {
            x: rect.x - decoration.layout.root.rect.x as f32,
            y: rect.y - decoration.layout.root.rect.y as f32,
            width: rect.width,
            height: rect.height,
        });
        let clip_physical = debug_clip_rect.map(|clip_rect| {
            let scale_x = scale.x.abs().max(0.0001) as f32;
            let scale_y = scale.y.abs().max(0.0001) as f32;
            let left = (clip_rect.x * scale_x).round() as i32;
            let top = (clip_rect.y * scale_y).round() as i32;
            let right = ((clip_rect.x + clip_rect.width) * scale_x).round() as i32;
            let bottom = ((clip_rect.y + clip_rect.height) * scale_y).round() as i32;
            smithay::utils::Rectangle::<i32, smithay::utils::Physical>::new(
                smithay::utils::Point::<i32, smithay::utils::Physical>::from((left, top)),
                ((right - left).max(0), (bottom - top).max(0)).into(),
            )
        });
        let clip_physical_precise = debug_clip_rect.map(|clip_rect| {
            let scale_x = scale.x.abs().max(0.0001) as f32;
            let scale_y = scale.y.abs().max(0.0001) as f32;
            (
                clip_rect.x * scale_x,
                clip_rect.y * scale_y,
                clip_rect.width * scale_x,
                clip_rect.height * scale_y,
            )
        });
        let clip_physical_global_precise = clip_physical_precise.map(|rect| {
            (
                geometry.loc.x as f32 + rect.0,
                geometry.loc.y as f32 + rect.1,
                rect.2,
                rect.3,
            )
        });
        let clip_physical_global = clip_physical.map(|rect| {
            smithay::utils::Rectangle::<i32, smithay::utils::Physical>::new(
                smithay::utils::Point::<i32, smithay::utils::Physical>::from((
                    geometry.loc.x + rect.loc.x,
                    geometry.loc.y + rect.loc.y,
                )),
                rect.size,
            )
        });
        tracing::info!(
            stable_key = %cached.stable_key,
            rect = ?cached.rect,
            rect_precise = ?cached.rect_precise,
            root_local_rect_precise = ?root_local_rect_precise,
            local_rect = ?local_rect,
            clip_rect = ?cached.clip_rect,
            clip_rect_precise = ?cached.clip_rect_precise,
            root_local_clip_precise = ?root_local_clip_precise,
            snapped_clip = ?cached.clip_rect.map(|clip_rect| {
                snapped_logical_rect_for_element(
                    clip_rect,
                    Point::from((cached.rect.x, cached.rect.y)),
                    window_snap_origin,
                    scale,
                    RectSnapMode::SharedEdges,
                )
            }),
            clip_physical = ?clip_physical,
            clip_physical_precise = ?clip_physical_precise,
            clip_physical_global_precise = ?clip_physical_global_precise,
            clip_physical_global = ?clip_physical_global,
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
