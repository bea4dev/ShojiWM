use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Per-popup groups: (id, output-local rect, location, surface, elements).
type PopupRenderGroups<R> = Vec<(
    String,
    crate::ssd::LogicalRect,
    Point<i32, Logical>,
    WlSurface,
    Vec<WaylandSurfaceRenderElement<R>>,
)>;

use smithay::{
    backend::renderer::{
        ImportAll, Renderer,
        element::{
            AsRenderElements, Element, Id, Kind,
            surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
        },
        gles::GlesRenderer,
        utils::with_renderer_surface_state,
    },
    desktop::{
        LayerSurface, PopupManager, Window, WindowSurface, layer_map_for_output,
        utils::bbox_from_surface_tree,
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{
        background_effect::BackgroundEffectSurfaceCachedState,
        compositor::{RectangleKind, RegionAttributes, with_states},
        session_lock::LockSurface,
        shell::wlr_layer::Layer as WlrLayer,
        shell::xdg::XdgToplevelSurfaceData,
    },
};
use tracing::{info, warn};

use crate::{backend::clipped_surface::ClippedSurfaceElement, ssd::ContentClip};

pub enum WindowClipElement {
    Clipped(ClippedSurfaceElement),
    Raw(WaylandSurfaceRenderElement<GlesRenderer>),
}

fn popup_debug_enabled() -> bool {
    std::env::var_os("SHOJI_POPUP_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn gap_debug_enabled() -> bool {
    std::env::var_os("SHOJI_GAP_DEBUG").is_some_and(|value| value != "0" && !value.is_empty())
}

fn clip_selection_debug_enabled() -> bool {
    std::env::var_os("SHOJI_CLIP_SELECTION_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

fn clip_selection_debug_allowed(root_key: &str, app_id: Option<&str>) -> bool {
    if !clip_selection_debug_enabled() {
        return false;
    }
    if !matches!(app_id, Some("firefox") | Some("google-chrome")) {
        return false;
    }

    static LAST_LOGGED: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let state = LAST_LOGGED.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut state) = state.lock() else {
        return false;
    };
    let now = Instant::now();
    let should_log = state
        .get(root_key)
        .is_none_or(|last| now.saturating_duration_since(*last) >= Duration::from_millis(250));
    if should_log {
        state.insert(root_key.to_string(), now);
    }
    should_log
}

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
        out.push(crate::ssd::LogicalRect::new(
            base.x,
            base.y,
            base.width,
            top - base.y,
        ));
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
        out.push(crate::ssd::LogicalRect::new(
            base.x,
            top,
            left - base.x,
            bottom - top,
        ));
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
        .filter(|surface| layer_surface_is_mapped(surface))
        .cloned()
        .partition(|surface| matches!(surface.layer(), WlrLayer::Background | WlrLayer::Bottom));
    (upper, lower)
}

/// Layer surfaces ordered for the dedicated popup pass.
///
/// Render-element vectors are front-to-back in ShojiWM, so an overlay popup
/// precedes top, bottom, and background layer popups. Keeping this separate
/// from the parent-layer passes is important for `zwlr_layer_surface_v1.get_popup`:
/// a tooltip belonging to a Bottom-layer bar must still appear above regular
/// toplevel windows.
pub fn layer_surfaces_for_popup_pass(
    output: &smithay::output::Output,
    overlay_only: bool,
) -> Vec<LayerSurface> {
    let map = layer_map_for_output(output);
    let layer_kinds: &[WlrLayer] = if overlay_only {
        &[WlrLayer::Overlay]
    } else {
        &[
            WlrLayer::Overlay,
            WlrLayer::Top,
            WlrLayer::Bottom,
            WlrLayer::Background,
        ]
    };

    layer_kinds
        .iter()
        .flat_map(|layer| map.layers_on(*layer).rev().cloned())
        .filter(layer_surface_is_mapped)
        .collect()
}

pub fn layer_surface_is_mapped(layer_surface: &LayerSurface) -> bool {
    with_renderer_surface_state(layer_surface.wl_surface(), |state| state.buffer().is_some())
        .unwrap_or(false)
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

pub fn lock_surface_elements<R>(
    renderer: &mut R,
    lock_surface: &LockSurface,
    scale: Scale<f64>,
    alpha: f32,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    render_elements_from_surface_tree(
        renderer,
        lock_surface.wl_surface(),
        Point::<i32, Logical>::from((0, 0)).to_physical_precise_round(scale),
        scale,
        alpha,
        Kind::Unspecified,
    )
}

/// Render elements of a layer surface's own surface tree, *excluding* its
/// xdg_popups. Counterpart of `layer_surface_elements` for callers that
/// handle popups separately (per-popup effects).
pub fn layer_surface_root_elements<R>(
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
            render_elements_from_surface_tree(
                renderer,
                surface.wl_surface(),
                loc.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::Unspecified,
            )
        })
        .collect()
}

/// Per-popup element groups of a layer surface's xdg_popups, front-to-back in
/// the same order `LayerSurface::render_elements` would have produced them.
/// Each entry carries the popup's runtime id and its geometry box as an
/// output-local logical rect (the rect popup effects apply to).
pub fn layer_surface_popup_groups<R>(
    renderer: &mut R,
    output: &smithay::output::Output,
    layer_surface: &LayerSurface,
    scale: Scale<f64>,
    alpha: f32,
) -> PopupRenderGroups<R>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let map = layer_map_for_output(output);
    let Some(geo) = map.layer_geometry(layer_surface) else {
        return Vec::new();
    };
    let loc = geo.loc - layer_surface.geometry().loc;
    PopupManager::popups_for_surface(layer_surface.wl_surface())
        .map(|(popup, popup_offset)| {
            let popup_geometry = popup.geometry();
            let rect = crate::ssd::LogicalRect::new(
                loc.x + popup_offset.x,
                loc.y + popup_offset.y,
                popup_geometry.size.w,
                popup_geometry.size.h,
            );
            // Output-local logical position of the popup's buffer origin —
            // the coordinate the popup's surface-local blur region rects are
            // relative to.
            let buffer_origin = loc + popup_offset - popup_geometry.loc;
            let render_origin = buffer_origin.to_physical_precise_round(scale);
            let elements = render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                render_origin,
                scale,
                alpha,
                Kind::Unspecified,
            );
            (
                crate::ssd::popup_runtime_id(popup.wl_surface()),
                rect,
                buffer_origin,
                popup.wl_surface().clone(),
                elements,
            )
        })
        .collect()
}

/// `ext-background-effect-v1` blur rects a popup surface requested, clamped
/// to the popup's surface-tree extents and translated so they line up with
/// where the popup's buffer origin renders. `buffer_origin` must be in the
/// same coordinate space the returned rects should be in (the popup-group
/// functions above return it alongside each popup).
///
/// This is how fcitx5's candidate window (an input-method popup) asks for
/// blur — the window/layer protocol paths never see popup surfaces, so
/// without this the request was silently ignored.
pub fn protocol_background_effect_rects_for_popup(
    surface: &WlSurface,
    buffer_origin: Point<i32, Logical>,
) -> Vec<crate::ssd::LogicalRect> {
    let blur_region = with_states(surface, |states| {
        let mut cached = states
            .cached_state
            .get::<BackgroundEffectSurfaceCachedState>();
        cached.current().blur_region.clone()
    });
    let Some(region) = blur_region else {
        return Vec::new();
    };
    let bbox = bbox_from_surface_tree(surface, (0, 0));
    if bbox.size.w <= 0 || bbox.size.h <= 0 {
        return Vec::new();
    }
    region_rects_within_bounds(
        &region,
        crate::ssd::LogicalRect::new(bbox.loc.x, bbox.loc.y, bbox.size.w, bbox.size.h),
    )
    .into_iter()
    .map(|rect| {
        crate::ssd::LogicalRect::new(
            buffer_origin.x + rect.x,
            buffer_origin.y + rect.y,
            rect.width,
            rect.height,
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
        WindowSurface::Wayland(surface) => {
            let render_origin = location - window.geometry().loc.to_physical_precise_round(scale);
            let elements = render_elements_from_surface_tree(
                renderer,
                surface.wl_surface(),
                render_origin,
                scale,
                alpha,
                Kind::Unspecified,
            );

            if popup_debug_enabled() {
                let (title, app_id) = with_states(surface.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<XdgToplevelSurfaceData>()
                        .and_then(|role| role.lock().ok())
                        .map(|role| (role.title.clone().unwrap_or_default(), role.app_id.clone()))
                        .unwrap_or_default()
                });
                let geometries = elements
                    .iter()
                    .take(8)
                    .map(|element| Element::geometry(element, scale))
                    .collect::<Vec<_>>();
                let srcs = elements
                    .iter()
                    .take(8)
                    .map(Element::src)
                    .collect::<Vec<_>>();
                info!(
                    root_surface = ?surface.wl_surface().id(),
                    title = %title,
                    app_id = ?app_id,
                    base_location = ?location,
                    render_origin = ?render_origin,
                    element_count = elements.len(),
                    first_geometries = ?geometries,
                    first_srcs = ?srcs,
                    "surface tree placement",
                );
            }

            elements
        }
        WindowSurface::X11(surface) => {
            AsRenderElements::<R>::render_elements(surface, renderer, location, scale, alpha)
        }
    }
}

pub fn root_surface_elements<R>(
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
            let render_origin = location - window.geometry().loc.to_physical_precise_round(scale);
            with_states(surface.wl_surface(), |states| {
                match WaylandSurfaceRenderElement::from_surface(
                    renderer,
                    surface.wl_surface(),
                    states,
                    render_origin.to_f64(),
                    alpha,
                    Kind::Unspecified,
                ) {
                    Ok(Some(element)) => vec![element],
                    Ok(None) => Vec::new(),
                    Err(err) => {
                        warn!("Failed to import root surface: {}", err);
                        Vec::new()
                    }
                }
            })
        }
        WindowSurface::X11(surface) => {
            AsRenderElements::<R>::render_elements(surface, renderer, location, scale, alpha)
        }
    }
}

pub fn non_root_surface_elements<R>(
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
    let root_id = match window.underlying_surface() {
        WindowSurface::Wayland(surface) => Some(Id::from_wayland_resource(surface.wl_surface())),
        WindowSurface::X11(_) => None,
    };

    surface_elements(window, renderer, location, scale, alpha)
        .into_iter()
        .filter(|element| {
            root_id
                .as_ref()
                .is_none_or(|root_id| element.id() != root_id)
        })
        .collect()
}

pub fn snapshot_bounds(
    window: &Window,
    location: Point<i32, Logical>,
    root_rect: crate::ssd::LogicalRect,
    content_clip: Option<ContentClip>,
) -> crate::ssd::LogicalRect {
    if content_clip.is_some_and(|clip| clip.clips_surface) {
        return root_rect;
    }

    let bbox = window.bbox();
    let surface = crate::ssd::LogicalRect::new(
        location.x + bbox.loc.x,
        location.y + bbox.loc.y,
        bbox.size.w,
        bbox.size.h,
    );
    let left = root_rect.x.min(surface.x);
    let top = root_rect.y.min(surface.y);
    let right = root_rect
        .x
        .saturating_add(root_rect.width)
        .max(surface.x.saturating_add(surface.width));
    let bottom = root_rect
        .y
        .saturating_add(root_rect.height)
        .max(surface.y.saturating_add(surface.height));
    crate::ssd::LogicalRect::new(left, top, right - left, bottom - top)
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
                    let popup_geometry_loc = popup.geometry().loc;
                    let popup_offset_logical =
                        window.geometry().loc + popup_offset - popup_geometry_loc;
                    let popup_offset_without_window_geometry =
                        popup_offset - popup_geometry_loc;
                    let render_origin =
                        location - window.geometry().loc.to_physical_precise_round(scale);
                    let offset = popup_offset_logical.to_physical_precise_round(scale);
                    let offset_without_window_geometry: Point<i32, Physical> =
                        popup_offset_without_window_geometry.to_physical_precise_round(scale);
                    let elements = render_elements_from_surface_tree(
                        renderer,
                        popup.wl_surface(),
                        render_origin + offset,
                        scale,
                        alpha,
                        Kind::Unspecified,
                    );

                    if popup_debug_enabled() {
                        let (title, app_id) = match window.underlying_surface() {
                            WindowSurface::Wayland(root) => with_states(root.wl_surface(), |states| {
                                states
                                    .data_map
                                    .get::<XdgToplevelSurfaceData>()
                                    .and_then(|role| role.lock().ok())
                                    .map(|role| {
                                        (
                                            role.title.clone().unwrap_or_default(),
                                            role.app_id.clone(),
                                        )
                                    })
                                    .unwrap_or_default()
                            }),
                            WindowSurface::X11(_) => (String::new(), None),
                        };
                        let first_geometry = elements
                            .first()
                            .map(|element| Element::geometry(element, scale));
                        info!(
                            root_surface = ?surface.id(),
                            popup_surface = ?popup.wl_surface().id(),
                            title = %title,
                            app_id = ?app_id,
                            window_geometry_loc = ?window.geometry().loc,
                            popup_offset = ?popup_offset,
                            popup_geometry_loc = ?popup_geometry_loc,
                            popup_offset_logical = ?popup_offset_logical,
                            popup_offset_without_window_geometry = ?popup_offset_without_window_geometry,
                            base_location = ?location,
                            render_origin = ?render_origin,
                            computed_offset = ?offset,
                            computed_offset_without_window_geometry = ?offset_without_window_geometry,
                            final_location = ?Point::<i32, Physical>::from((
                                render_origin.x + offset.x,
                                render_origin.y + offset.y,
                            )),
                            final_location_without_window_geometry = ?Point::<i32, Physical>::from((
                                render_origin.x + offset_without_window_geometry.x,
                                render_origin.y + offset_without_window_geometry.y,
                            )),
                            first_geometry = ?first_geometry,
                            element_count = elements.len(),
                            "popup render placement",
                        );
                    }

                    elements
                })
                .collect()
        }
        WindowSurface::X11(_) => Vec::new(),
    }
}

/// Per-popup element groups of a toplevel window's xdg_popups, front-to-back
/// in the same order `popup_elements` would have produced them. Each entry
/// carries the popup's runtime id and its geometry box as a *global* logical
/// rect (the rect popup effects apply to).
///
/// `location` is the window's output-local physical render location (the same
/// value `popup_elements` receives); `output_geo` converts back to global
/// logical coordinates.
pub fn window_popup_groups<R>(
    window: &Window,
    renderer: &mut R,
    location: Point<i32, Physical>,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
    alpha: f32,
) -> PopupRenderGroups<R>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let WindowSurface::Wayland(surface) = window.underlying_surface() else {
        return Vec::new();
    };
    let surface = surface.wl_surface();
    let render_origin = location - window.geometry().loc.to_physical_precise_round(scale);
    let location_logical: Point<i32, Logical> = Point::from((
        (location.x as f64 / scale.x).round() as i32,
        (location.y as f64 / scale.y).round() as i32,
    ));
    PopupManager::popups_for_surface(surface)
        .map(|(popup, popup_offset)| {
            let popup_geometry = popup.geometry();
            let offset = (window.geometry().loc + popup_offset - popup_geometry.loc)
                .to_physical_precise_round(scale);
            let elements = render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                render_origin + offset,
                scale,
                alpha,
                Kind::Unspecified,
            );
            // Geometry box in global logical coords: the buffer origin lands
            // at location + (popup_offset - popup_geometry.loc), so the
            // geometry box sits at location + popup_offset.
            let rect = crate::ssd::LogicalRect::new(
                output_geo.loc.x + location_logical.x + popup_offset.x,
                output_geo.loc.y + location_logical.y + popup_offset.y,
                popup_geometry.size.w,
                popup_geometry.size.h,
            );
            // Global logical position of the popup's buffer origin — the
            // coordinate the popup's surface-local blur region rects are
            // relative to (window.geometry().loc cancels against
            // render_origin, so it does not appear here).
            let buffer_origin = Point::<i32, Logical>::from((
                output_geo.loc.x + location_logical.x + popup_offset.x - popup_geometry.loc.x,
                output_geo.loc.y + location_logical.y + popup_offset.y - popup_geometry.loc.y,
            ));
            (
                crate::ssd::popup_runtime_id(popup.wl_surface()),
                rect,
                buffer_origin,
                popup.wl_surface().clone(),
                elements,
            )
        })
        .collect()
}

pub fn clipped_surface_elements(
    window: &Window,
    renderer: &mut GlesRenderer,
    location: Point<i32, Physical>,
    geometry: Option<Rectangle<i32, Physical>>,
    output_origin: Point<i32, Logical>,
    output_scale: Scale<f64>,
    clip_scale: Scale<f64>,
    alpha: f32,
    clip: Option<ContentClip>,
    _clip_all_surfaces: bool,
) -> Result<Vec<WindowClipElement>, smithay::backend::renderer::gles::GlesError> {
    // WindowSlot always describes placement, but only an explicit ancestor
    // SSD clip is allowed to crop the client surface tree.
    let clip = clip.filter(|clip| clip.clips_surface);

    // Color-management tag for the window's root surface, applied to every
    // element in its tree. Approximation: subsurfaces are distinct protocol
    // surfaces and may carry their own descriptions, so a player that puts
    // video on a subsurface while leaving the toplevel untagged is not handled
    // yet. The common case — a client tagging its main surface — is.
    let image_description = match window.underlying_surface() {
        WindowSurface::Wayland(surface) => {
            crate::protocols::color_management::surface_image_description(surface.wl_surface())
        }
        // X11 has no color-management protocol, so XWayland clients are always
        // untagged and take the passthrough path.
        _ => None,
    };

    let elements = surface_elements(window, renderer, location, output_scale, alpha);
    if clip.is_none() || std::env::var_os("SHOJI_GAP_BYPASS_CLIP").is_some() {
        return Ok(elements.into_iter().map(WindowClipElement::Raw).collect());
    }

    let mut debug_app_id: Option<String> = None;
    let debug_label = match window.underlying_surface() {
        WindowSurface::Wayland(surface) => {
            let (title, app_id) = with_states(surface.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|role| role.lock().ok())
                    .map(|role| (role.title.clone().unwrap_or_default(), role.app_id.clone()))
                    .unwrap_or_default()
            });
            debug_app_id = app_id.clone();

            Some(format!(
                "root_surface={:?} title={} app_id={:?}",
                surface.wl_surface().id(),
                title,
                app_id
            ))
        }
        WindowSurface::X11(_) => None,
    };

    let element_geometries = elements
        .iter()
        .map(|element| Element::geometry(element, output_scale))
        .collect::<Vec<_>>();
    let element_srcs = elements
        .iter()
        .map(Element::src)
        .collect::<Vec<_>>();
    let selected_indices = geometry
        .map(|forced_geometry| {
            let best_score = element_geometries
                .iter()
                .map(|element_geometry| {
                    i64::from((element_geometry.size.w - forced_geometry.size.w).abs())
                        + i64::from((element_geometry.size.h - forced_geometry.size.h).abs())
                })
                .min()
                .unwrap_or(0);
            element_geometries
                .iter()
                .enumerate()
                .filter_map(|(index, element_geometry)| {
                    let score = i64::from((element_geometry.size.w - forced_geometry.size.w).abs())
                        + i64::from((element_geometry.size.h - forced_geometry.size.h).abs());
                    (score == best_score).then_some(index)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if gap_debug_enabled() {
        let surface_kind = match window.underlying_surface() {
            WindowSurface::Wayland(_) => "wayland",
            WindowSurface::X11(_) => "x11",
        };
        info!(
            debug_label = ?debug_label,
            surface_kind,
            base_location = ?location,
            forced_geometry = ?geometry,
            output_origin = ?output_origin,
            output_scale = ?output_scale,
            clip_scale = ?clip_scale,
            alpha,
            clip = ?clip,
            element_count = element_geometries.len(),
            chosen_indices = ?selected_indices,
            element_geometries = ?element_geometries,
            element_srcs = ?element_srcs,
            "gap debug clipped surface selection",
        );
    }
    if let (Some(debug_label), true) = (
        debug_label.as_deref(),
        clip_selection_debug_allowed(
            debug_label.as_deref().unwrap_or_default(),
            debug_app_id.as_deref(),
        ),
    ) {
        info!(
            debug_label,
            forced_geometry = ?geometry,
            element_count = element_geometries.len(),
            chosen_indices = ?selected_indices,
            element_geometries = ?element_geometries,
            "clip selection summary",
        );
    }
    match clip {
        Some(clip) if matches!(window.underlying_surface(), WindowSurface::Wayland(_)) => {
            // CSS-like parent clipping belongs to the toplevel/root surface. Subsurfaces are
            // separate protocol surfaces and may intentionally extend outside the root surface
            // (Chrome uses some subsurfaces like popups). Do not infer the root element from the
            // render-element order: Smithay traverses the surface tree in paint order, so a
            // below-sibling subsurface can appear before the root. Match by resource id instead.
            let root_id = match window.underlying_surface() {
                WindowSurface::Wayland(surface) => Id::from_wayland_resource(surface.wl_surface()),
                WindowSurface::X11(_) => unreachable!("Wayland branch already checked"),
            };
            let mut output = Vec::with_capacity(elements.len());
            for element in elements {
                if Element::id(&element) == &root_id {
                    output.push(WindowClipElement::Clipped(ClippedSurfaceElement::new(
                        renderer,
                        element,
                        output_scale,
                        clip_scale,
                        output_origin,
                        clip,
                        geometry,
                        debug_label.clone(),
                        image_description,
                    )?));
                } else {
                    output.push(WindowClipElement::Raw(element));
                }
            }
            Ok(output)
        }
        Some(clip) => elements
            .into_iter()
            .enumerate()
            .map(|(index, element)| {
                let geometry_override = geometry.filter(|_| selected_indices.contains(&index));
                if gap_debug_enabled() {
                    info!(
                        debug_label = ?debug_label,
                        index,
                        element_geometry = ?Element::geometry(&element, output_scale),
                        element_src = ?Element::src(&element),
                        geometry_override = ?geometry_override,
                        clip = ?clip,
                        "gap debug clipped surface candidate",
                    );
                }
                if geometry_override.is_some() {
                    ClippedSurfaceElement::new(
                        renderer,
                        element,
                        output_scale,
                        clip_scale,
                        output_origin,
                        clip,
                        geometry_override,
                        debug_label.clone(),
                        image_description,
                    )
                    .map(WindowClipElement::Clipped)
                } else {
                    Ok(WindowClipElement::Raw(element))
                }
            })
            .collect(),
        None => Ok(elements.into_iter().map(WindowClipElement::Raw).collect()),
    }
}

pub fn clipped_popup_elements(
    window: &Window,
    renderer: &mut GlesRenderer,
    location: Point<i32, Physical>,
    output_origin: Point<i32, Logical>,
    output_scale: Scale<f64>,
    clip_scale: Scale<f64>,
    alpha: f32,
    clip: ContentClip,
) -> Result<Vec<ClippedSurfaceElement>, smithay::backend::renderer::gles::GlesError> {
    popup_elements(window, renderer, location, output_scale, alpha)
        .into_iter()
        .map(|element| {
            ClippedSurfaceElement::new(
                renderer,
                element,
                output_scale,
                clip_scale,
                output_origin,
                clip,
                None,
                Some("popup clipped by ManagedWindow.forceRectSize".to_owned()),
                // Popups are separate protocol surfaces with their own
                // descriptions; inheriting the toplevel's would be wrong. They
                // are effectively never color-tagged, so leave them untagged
                // rather than guess.
                None,
            )
        })
        .collect()
}
