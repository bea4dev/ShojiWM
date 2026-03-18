use smithay::{
    backend::renderer::{
        element::{
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind,
        },
        gles::GlesRenderer,
        ImportAll, Renderer,
    },
    desktop::{layer_map_for_output, LayerSurface, PopupManager, Window, WindowSurface},
    utils::{Physical, Point, Scale},
};

use crate::{backend::clipped_surface::ClippedSurfaceElement, ssd::ContentClip};

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
    let map = layer_map_for_output(output);
    let (lower, upper): (Vec<&LayerSurface>, Vec<&LayerSurface>) =
        map.layers().rev().partition(|surface| {
            matches!(
                surface.layer(),
                smithay::wayland::shell::wlr_layer::Layer::Background
                    | smithay::wayland::shell::wlr_layer::Layer::Bottom
            )
        });

    let upper_elements = upper
        .into_iter()
        .filter_map(|surface| {
            map.layer_geometry(surface)
                .map(|geo| (geo.loc - surface.geometry().loc, surface))
        })
        .flat_map(|(loc, surface)| {
            AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                surface,
                renderer,
                loc.to_physical_precise_round(scale),
                scale,
                alpha,
            )
        })
        .collect();

    let lower_elements = lower
        .into_iter()
        .filter_map(|surface| {
            map.layer_geometry(surface)
                .map(|geo| (geo.loc - surface.geometry().loc, surface))
        })
        .flat_map(|(loc, surface)| {
            AsRenderElements::<R>::render_elements::<WaylandSurfaceRenderElement<R>>(
                surface,
                renderer,
                loc.to_physical_precise_round(scale),
                scale,
                alpha,
            )
        })
        .collect();

    (upper_elements, lower_elements)
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
    scale: Scale<f64>,
    alpha: f32,
    clip: Option<ContentClip>,
) -> Result<Vec<ClippedSurfaceElement>, smithay::backend::renderer::gles::GlesError> {
    let elements = surface_elements(window, renderer, location, scale, alpha);
    match clip {
        Some(clip) => elements
            .into_iter()
            .map(|element| ClippedSurfaceElement::new(renderer, element, scale, clip))
            .collect(),
        None => Ok(Vec::new()),
    }
}
