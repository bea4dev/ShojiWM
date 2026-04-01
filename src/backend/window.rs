use smithay::{
    backend::renderer::{
        element::{
            Element,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
        ImportAll, Renderer,
    },
    desktop::{layer_map_for_output, LayerSurface, PopupManager, Window, WindowSurface},
    utils::{Logical, Physical, Point, Scale},
    wayland::{compositor::{RectangleKind, RegionAttributes}, shell::wlr_layer::Layer as WlrLayer},
};

use crate::{backend::clipped_surface::ClippedSurfaceElement, ssd::ContentClip};

fn subtract_logical_rect(
    base: crate::ssd::LogicalRect,
    cut: crate::ssd::LogicalRect,
) -> Vec<crate::ssd::LogicalRect> {
    let left = base.x.max(cut.x);
    let top = base.y.max(cut.y);
    let right = (base.x + base.width).min(cut.x + cut.width);
    let bottom = (base.y + base.height).min(cut.y + cut.height);

    if right <= left || bottom <= top {
        return vec![base];
    }

    let mut out = Vec::new();
    if top > base.y {
        out.push(crate::ssd::LogicalRect::new(base.x, base.y, base.width, top - base.y));
    }
    if bottom < base.y + base.height {
        out.push(crate::ssd::LogicalRect::new(
            base.x,
            bottom,
            base.width,
            base.y + base.height - bottom,
        ));
    }
    if left > base.x {
        out.push(crate::ssd::LogicalRect::new(base.x, top, left - base.x, bottom - top));
    }
    if right < base.x + base.width {
        out.push(crate::ssd::LogicalRect::new(
            right,
            top,
            base.x + base.width - right,
            bottom - top,
        ));
    }
    out.retain(|rect| rect.width > 0 && rect.height > 0);
    out
}

fn intersect_logical_rects(
    a: crate::ssd::LogicalRect,
    b: crate::ssd::LogicalRect,
) -> Option<crate::ssd::LogicalRect> {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    if right <= left || bottom <= top {
        return None;
    }
    Some(crate::ssd::LogicalRect::new(
        left,
        top,
        right - left,
        bottom - top,
    ))
}

pub fn region_rects_within_bounds(
    region: &RegionAttributes,
    bounds: crate::ssd::LogicalRect,
) -> Vec<crate::ssd::LogicalRect> {
    let mut current: Vec<crate::ssd::LogicalRect> = Vec::new();

    for (kind, rect) in &region.rects {
        let Some(clipped) = intersect_logical_rects(
            bounds,
            crate::ssd::LogicalRect::new(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
        ) else {
            continue;
        };

        match kind {
            RectangleKind::Add => {
                let mut pending = vec![clipped];
                for existing in &current {
                    pending = pending
                        .into_iter()
                        .flat_map(|rect| subtract_logical_rect(rect, *existing))
                        .collect();
                    if pending.is_empty() {
                        break;
                    }
                }
                current.extend(pending);
            }
            RectangleKind::Subtract => {
                current = current
                    .into_iter()
                    .flat_map(|rect| subtract_logical_rect(rect, clipped))
                    .collect();
            }
        }
    }

    current
}

pub fn bounding_box_for_rects(
    rects: &[crate::ssd::LogicalRect],
) -> Option<crate::ssd::LogicalRect> {
    let first = rects.first().copied()?;
    let mut left = first.x;
    let mut top = first.y;
    let mut right = first.x + first.width;
    let mut bottom = first.y + first.height;

    for rect in rects.iter().copied().skip(1) {
        left = left.min(rect.x);
        top = top.min(rect.y);
        right = right.max(rect.x + rect.width);
        bottom = bottom.max(rect.y + rect.height);
    }

    Some(crate::ssd::LogicalRect::new(
        left,
        top,
        right - left,
        bottom - top,
    ))
}

pub fn layer_surfaces_for_output(
    output: &smithay::output::Output,
) -> (Vec<LayerSurface>, Vec<LayerSurface>) {
    let map = layer_map_for_output(output);
    let (lower, upper): (Vec<LayerSurface>, Vec<LayerSurface>) = map
        .layers()
        .rev()
        .cloned()
        .partition(|surface| {
            matches!(
                surface.layer(),
                WlrLayer::Background | WlrLayer::Bottom
            )
        });
    (upper, lower)
}

pub fn layer_elements_for_output<R>(
    renderer: &mut R,
    output: &smithay::output::Output,
    scale: Scale<f64>,
    alpha: f32,
) -> (
    Vec<WaylandSurfaceRenderElement<R>>,
    Vec<WaylandSurfaceRenderElement<R>>,
)
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let (upper, lower) = layer_surfaces_for_output(output);

    let upper_elements = upper
        .into_iter()
        .flat_map(|surface| layer_surface_elements(renderer, output, &surface, scale, alpha))
        .collect();

    let lower_elements = lower
        .into_iter()
        .flat_map(|surface| layer_surface_elements(renderer, output, &surface, scale, alpha))
        .collect();

    (upper_elements, lower_elements)
}

pub fn layer_surface_elements<R>(
    renderer: &mut R,
    output: &smithay::output::Output,
    layer_surface: &LayerSurface,
    scale: Scale<f64>,
    alpha: f32,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let map = layer_map_for_output(output);
    map.layer_geometry(layer_surface)
        .map(|geo| (geo.loc - layer_surface.geometry().loc, layer_surface))
        .into_iter()
        .flat_map(|(loc, surface)| {
            AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                surface,
                renderer,
                loc.to_physical_precise_round(scale),
                scale,
                alpha,
            )
        })
        .collect()
}

pub fn surface_elements<R>(
    window: &Window,
    renderer: &mut R,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    match window.underlying_surface() {
        WindowSurface::Wayland(surface) => render_elements_from_surface_tree(
            renderer,
            surface.wl_surface(),
            location,
            scale,
            alpha,
            Kind::Unspecified,
        ),
        WindowSurface::X11(surface) => {
            AsRenderElements::<R>::render_elements(surface, renderer, location, scale, alpha)
        }
    }
}

pub fn debug_surface_elements<R>(
    window: &Window,
    renderer: &mut R,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
) where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    if std::env::var_os("SHOJI_GAP_DEBUG").is_none() {
        return;
    }

    let elements = surface_elements(window, renderer, location, scale, alpha);
    let geometries = elements
        .iter()
        .map(|element| element.geometry(scale))
        .collect::<Vec<_>>();
    let srcs = elements
        .iter()
        .map(|element| element.src())
        .collect::<Vec<_>>();
    let transforms = elements
        .iter()
        .map(|element| element.transform())
        .collect::<Vec<_>>();
    let commits = elements
        .iter()
        .map(|element| element.current_commit())
        .collect::<Vec<_>>();
    let damages = elements
        .iter()
        .map(|element| element.damage_since(scale, None))
        .collect::<Vec<_>>();
    let opaque_regions = elements
        .iter()
        .map(|element| element.opaque_regions(scale))
        .collect::<Vec<_>>();

    let bbox = geometries.iter().copied().reduce(|acc, rect| {
        let left = acc.loc.x.min(rect.loc.x);
        let top = acc.loc.y.min(rect.loc.y);
        let right = (acc.loc.x + acc.size.w).max(rect.loc.x + rect.size.w);
        let bottom = (acc.loc.y + acc.size.h).max(rect.loc.y + rect.size.h);
        smithay::utils::Rectangle::new(
            smithay::utils::Point::from((left, top)),
            ((right - left), (bottom - top)).into(),
        )
    });

    tracing::info!(
        location = ?location,
        scale = ?scale,
        alpha,
        count = elements.len(),
        bbox = ?bbox,
        geometries = ?geometries,
        srcs = ?srcs,
        transforms = ?transforms,
        commits = ?commits,
        damages = ?damages,
        opaque_regions = ?opaque_regions,
        "gap debug raw surface tree elements"
    );
}

pub fn popup_elements<R>(
    window: &Window,
    renderer: &mut R,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
    alpha: f32,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    match window.underlying_surface() {
        WindowSurface::Wayland(surface) => {
            let surface = surface.wl_surface();
            PopupManager::popups_for_surface(surface)
                .flat_map(|(popup, popup_offset)| {
                    let offset = (window.geometry().loc + popup_offset - popup.geometry().loc)
                        .to_physical_precise_round(scale);

                    render_elements_from_surface_tree(
                        renderer,
                        popup.wl_surface(),
                        location + offset,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    )
                })
                .collect()
        }
        WindowSurface::X11(_) => Vec::new(),
    }
}

pub fn clipped_surface_elements(
    window: &Window,
    renderer: &mut GlesRenderer,
    location: Point<i32, Physical>,
    output_origin: Point<i32, Logical>,
    output_scale: Scale<f64>,
    clip_scale: Scale<f64>,
    alpha: f32,
    clip: Option<ContentClip>,
) -> Result<Vec<ClippedSurfaceElement>, smithay::backend::renderer::gles::GlesError> {
    if std::env::var_os("SHOJI_GAP_BYPASS_CLIP").is_some() {
        return Ok(Vec::new());
    }

    let elements = surface_elements(window, renderer, location, output_scale, alpha);
    match clip {
        Some(clip) => elements
            .into_iter()
            .map(|element| {
                ClippedSurfaceElement::new(
                    renderer,
                    element,
                    output_scale,
                    clip_scale,
                    output_origin,
                    clip,
                )
            })
            .collect(),
        None => Ok(Vec::new()),
    }
}
