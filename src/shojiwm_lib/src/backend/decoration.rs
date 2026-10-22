use std::collections::HashMap;

use smithay::{
    backend::renderer::gles::{GlesError, GlesRenderer},
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
};

use crate::{
    backend::shader_effect::{
        ShaderEffectError, ShaderEffectSpec, StableBackdropFramebufferElement,
        StableShaderEffectElement,
    },
    backend::text,
    backend::visual::{
        RectSnapMode, relative_physical_rect_from_root_precise,
        relative_physical_rect_from_root_snapped_edges, snapped_logical_rect_for_element,
        snapped_precise_logical_rect_in_root_frame_area_space,
    },
    ssd::{DecorationPart, LogicalRect, PopupLayer, WindowDecorationState},
};

smithay::render_elements! {
    pub DecorationSceneElements<=GlesRenderer>;
    Paint=crate::backend::paint::StablePaintElement,
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
    crate::env_flag!("SHOJI_GAP_DISABLE_DECORATION_CLIP")
}

fn gap_disable_titlebar_clip_enabled(height: i32) -> bool {
    crate::env_flag!("SHOJI_GAP_DISABLE_TITLEBAR_CLIP") && height == 30
}

fn gap_show_border_shell_only_enabled() -> bool {
    crate::env_flag!("SHOJI_GAP_SHOW_BORDER_SHELL_ONLY")
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
            shader_effect_element(renderer, decoration, cached, output_geo, scale, alpha)
                .transpose()
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
    Ok(
        ordered_background_elements_for_window(renderer, decoration, output_geo, scale, alpha)?
            .into_iter()
            .map(|(_, element)| element)
            .collect(),
    )
}

pub fn ordered_background_elements_for_window(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, DecorationSceneElements)>, DecorationSceneError> {
    ordered_background_elements_for_window_with_framebuffer_backdrops(
        renderer, decoration, output_geo, scale, alpha, false,
    )
}

pub fn ordered_background_elements_for_window_with_framebuffer_backdrops(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
    include_framebuffer_backdrops: bool,
) -> Result<Vec<(usize, DecorationSceneElements)>, DecorationSceneError> {
    let mut items = ordered_paint_elements(
        renderer,
        decoration,
        DecorationPart::Window,
        output_geo,
        scale,
        alpha,
    )?;

    for cached in decoration.shader_buffers.clone() {
        if cached.shader.supports_framebuffer_backdrop() {
            if include_framebuffer_backdrops {
                if let Some(element) = backdrop_shader_effect_element(
                    renderer, decoration, &cached, output_geo, scale, alpha,
                )? {
                    items.push((cached.order, DecorationSceneElements::Backdrop(element)));
                }
            }
            // When framebuffer backdrops are excluded (full-window snapshot
            // and offscreen source passes) there is no framebuffer to sample.
            // Falling through to the generic pixel element used to draw the
            // effect anyway — with undefined input and, crucially, WITHOUT
            // the rounded ancestor clip (StableShaderEffectElement has no
            // clip support), so its square corners painted outside the
            // rounded window border. Those frames then persisted on screen:
            // the corner region gets no damage after the animation ends.
            continue;
        }
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

fn ordered_paint_elements(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    part: DecorationPart,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, DecorationSceneElements)>, DecorationSceneError> {
    let scopes = decoration.popup_scopes();
    let to_output = crate::backend::paint::LayoutToOutput::new(decoration, scale);
    let WindowDecorationState {
        buffers,
        paint_cache,
        ..
    } = decoration;
    let mut items = Vec::new();
    for cached in buffers.iter() {
        if !scopes.includes(part, &cached.stable_key) {
            continue;
        }
        if let Some(element) = crate::backend::paint::paint_element(
            renderer,
            to_output,
            paint_cache,
            cached,
            output_geo,
            alpha,
        )? {
            items.push((cached.order, DecorationSceneElements::Paint(element)));
        }
    }
    Ok(items)
}

/// The elements of the open `<Popup>`s of one layer, positioned relative to
/// the root's physical origin like every decoration element, front to back.
pub fn popup_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    layer: PopupLayer,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<PopupSceneElement>, DecorationSceneError> {
    let part = DecorationPart::Popups(layer);
    let mut items = ordered_paint_elements(renderer, decoration, part, output_geo, scale, alpha)?
        .into_iter()
        .map(|(order, element)| (order, PopupSceneElement::Decoration(element)))
        .collect::<Vec<_>>();
    let textures =
        text::ordered_text_elements_for_part(renderer, decoration, part, output_geo, scale, alpha)?
            .into_iter()
            .chain(crate::backend::icon::ordered_icon_elements_for_part(
                renderer, decoration, part, output_geo, scale, alpha,
            )?);
    items.extend(textures.map(|(order, element)| (order, PopupSceneElement::Texture(element))));
    items.sort_by_key(|(order, _)| *order);
    Ok(items.into_iter().map(|(_, element)| element).collect())
}

/// One element of a popup pass; the backend relocates it onto the output.
pub enum PopupSceneElement {
    Decoration(DecorationSceneElements),
    Texture(text::DecorationTextureElements),
}

pub fn framebuffer_backdrop_element_for_window_rect(
    renderer: &mut GlesRenderer,
    decoration: &mut WindowDecorationState,
    stable_key: String,
    rect: LogicalRect,
    effect: crate::ssd::CompiledEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableBackdropFramebufferElement>, ShaderEffectError> {
    let cached = crate::backend::shader_effect::CachedShaderEffect {
        owner_node_id: None,
        stable_key,
        order: 0,
        rect,
        rect_precise: None,
        shader: effect,
        clip_rect: None,
        clip_radius: 0,
        clip_rect_precise: None,
        clip_radius_precise: None,
        node_shape: Default::default(),
    };
    backdrop_shader_effect_element(renderer, decoration, &cached, output_geo, scale, alpha)
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
    crate::backend::icon::icon_elements_for_window(
        renderer,
        space,
        decorations,
        output,
        window,
        alpha,
    )
}

pub fn ordered_icon_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::icon::ordered_icon_elements_for_window(
        renderer,
        space,
        decorations,
        output,
        window,
        alpha,
    )
}

pub fn ordered_icon_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::icon::ordered_icon_elements_for_decoration(
        renderer, decoration, output_geo, scale, alpha,
    )
}

pub fn ordered_text_elements_for_window(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::text::ordered_text_elements_for_window(
        renderer,
        space,
        decorations,
        output,
        window,
        alpha,
    )
}

pub fn ordered_text_elements_for_decoration(
    renderer: &mut GlesRenderer,
    decoration: &WindowDecorationState,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Vec<(usize, crate::backend::text::DecorationTextureElements)>, GlesError> {
    crate::backend::text::ordered_text_elements_for_decoration(
        renderer, decoration, output_geo, scale, alpha,
    )
}

fn local_clip_from_logical_rects(
    clip_rect: LogicalRect,
    element_rect: LogicalRect,
) -> crate::backend::visual::SnappedLogicalRect {
    crate::backend::visual::SnappedLogicalRect {
        x: (clip_rect.x - element_rect.x) as f32,
        y: (clip_rect.y - element_rect.y) as f32,
        width: clip_rect.width.max(0) as f32,
        height: clip_rect.height.max(0) as f32,
    }
}

fn backdrop_shader_effect_element(
    renderer: &mut GlesRenderer,
    decoration: &mut crate::ssd::WindowDecorationState,
    cached: &crate::backend::shader_effect::CachedShaderEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableBackdropFramebufferElement>, ShaderEffectError> {
    let Some(spec) = shader_effect_spec(decoration, cached, output_geo, scale, alpha) else {
        return Ok(None);
    };
    let state = decoration
        .shader_cache
        .entry(cached.stable_key.clone())
        .or_default();
    Ok(Some(state.backdrop_element(renderer, spec)?))
}

fn shader_effect_spec(
    decoration: &crate::ssd::WindowDecorationState,
    cached: &crate::backend::shader_effect::CachedShaderEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Option<ShaderEffectSpec> {
    shader_effect_spec_in_root(
        decoration.layout.root.rect,
        decoration.root_subpixel_offset,
        cached,
        output_geo,
        scale,
        alpha,
    )
}

/// The spec of a decoration's `<ShaderEffect>` with its geometry relative to
/// the root's physical origin, like every decoration element.
pub(crate) fn shader_effect_spec_in_root(
    root_rect: LogicalRect,
    root_subpixel_offset: crate::backend::visual::RootSubpixelEdges,
    cached: &crate::backend::shader_effect::CachedShaderEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Option<ShaderEffectSpec> {
    if gap_show_border_shell_only_enabled()
        || intersect_logical_rect(cached.rect, output_geo).is_none()
    {
        return None;
    }

    let local_rect = Rectangle::new(
        Point::from((cached.rect.x - root_rect.x, cached.rect.y - root_rect.y)),
        (cached.rect.width, cached.rect.height).into(),
    );
    let geometry = cached
        .rect_precise
        .map(|rect| {
            relative_physical_rect_from_root_precise(
                rect,
                root_rect,
                root_subpixel_offset,
                output_geo,
                scale,
            )
        })
        .unwrap_or_else(|| {
            relative_physical_rect_from_root_snapped_edges(
                cached.rect,
                root_rect,
                root_subpixel_offset,
                output_geo,
                scale,
            )
        });
    let render_scale = geometry.size.w.max(1) as f32 / local_rect.size.w.max(1) as f32;
    Some(ShaderEffectSpec {
        rect: local_rect,
        geometry,
        framebuffer_regions: Vec::new(),
        // Backdrop blur must sample beyond the visible rect: with a zero
        // padding the dual-kawase chain clamps at the capture edge and the
        // outermost column/row degenerates into an edge-duplicate that no
        // longer matches the true blurred neighborhood — visible as a 1px
        // seam between the effect and the window border. The layer-surface
        // path already pads (see shader_effect.rs), windows must match.
        framebuffer_capture_padding: crate::backend::shader_effect::framebuffer_capture_padding(
            &cached.shader,
            render_scale,
        ),
        shader: cached.shader.clone(),
        // The working texture starts at the node (plus capture padding), so
        // the frame is the content rect.
        frame: cached.node_shape.effect_frame(None, scale.x),
        alpha_bits: alpha.to_bits(),
        render_scale,
        clip_rect: if gap_disable_decoration_clip_enabled()
            || gap_disable_titlebar_clip_enabled(cached.rect.height)
        {
            None
        } else {
            cached
                .rect_precise
                .zip(cached.clip_rect_precise.or_else(|| {
                    cached
                        .clip_rect
                        .map(crate::backend::visual::precise_rect_from_logical)
                }))
                .map(|(rect_precise, clip_rect)| {
                    snapped_precise_logical_rect_in_root_frame_area_space(
                        clip_rect,
                        rect_precise,
                        local_rect.size.w,
                        local_rect.size.h,
                        root_rect,
                        root_subpixel_offset,
                        output_geo,
                        scale,
                    )
                })
                .or_else(|| {
                    cached
                        .clip_rect
                        .map(|clip_rect| local_clip_from_logical_rects(clip_rect, cached.rect))
                })
        },
        clip_radius: if gap_disable_decoration_clip_enabled()
            || gap_disable_titlebar_clip_enabled(cached.rect.height)
        {
            0.0
        } else {
            cached
                .clip_radius_precise
                .unwrap_or(cached.clip_radius as f32)
        },
    })
}

fn shader_effect_element(
    renderer: &mut GlesRenderer,
    decoration: &mut crate::ssd::WindowDecorationState,
    cached: &crate::backend::shader_effect::CachedShaderEffect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Result<Option<StableShaderEffectElement>, ShaderEffectError> {
    let Some(spec) = shader_effect_spec(decoration, cached, output_geo, scale, alpha) else {
        return Ok(None);
    };
    let local_rect = spec.rect;
    let window_snap_origin = output_geo.loc;

    let state = decoration
        .shader_cache
        .entry(cached.stable_key.clone())
        .or_default();
    if crate::env_flag!("SHOJI_GAP_DEBUG") {
        tracing::info!(
            stable_key = %cached.stable_key,
            spec = ?spec,
            "gap debug shader decoration spec"
        );
    }
    let debug_clip_rect = spec.clip_rect;
    let element = state.element(renderer, spec)?;
    if crate::env_flag!("SHOJI_GAP_DEBUG") {
        let geometry = smithay::backend::renderer::element::Element::geometry(&element, scale);
        let root_local_rect_precise =
            cached
                .rect_precise
                .map(|rect| crate::backend::visual::PreciseLogicalRect {
                    x: rect.x - decoration.layout.root.rect.x as f32,
                    y: rect.y - decoration.layout.root.rect.y as f32,
                    width: rect.width,
                    height: rect.height,
                });
        let root_local_clip_precise =
            cached
                .clip_rect_precise
                .map(|rect| crate::backend::visual::PreciseLogicalRect {
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
        let geometry_right = geometry.loc.x + geometry.size.w;
        let geometry_bottom = geometry.loc.y + geometry.size.h;
        let clip_right_global = clip_physical_global.map(|rect| rect.loc.x + rect.size.w);
        let clip_bottom_global = clip_physical_global.map(|rect| rect.loc.y + rect.size.h);
        tracing::info!(
            stable_key = %cached.stable_key,
            owner_node_id = ?cached.owner_node_id,
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
            geometry_right,
            geometry_bottom,
            clip_right_global,
            clip_bottom_global,
            clip_right_gap_px = clip_right_global.map(|right| geometry_right - right),
            clip_bottom_gap_px = clip_bottom_global.map(|bottom| geometry_bottom - bottom),
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
