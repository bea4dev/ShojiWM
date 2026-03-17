use std::collections::HashMap;

use smithay::{
    backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement},
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Rectangle, Scale},
};
use tracing::trace;

use crate::ssd::{LogicalRect, WindowDecorationState};

pub fn solid_elements_for_output(
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
) -> Vec<SolidColorRenderElement> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());

    let mut elements = Vec::new();
    for window in space.elements() {
        let Some(decoration) = decorations.get(window) else {
            continue;
        };

        for cached in &decoration.buffers {
            if let Some(element) = solid_rect_element(&cached.buffer, cached.rect, output_geo, scale) {
                elements.push(element);
            }
        }
    }

    trace!(
        output = %output.name(),
        output_geometry = ?output_geo,
        element_count = elements.len(),
        "prepared solid decoration elements for output"
    );

    elements
}

pub fn solid_elements_for_window(
    space: &Space<Window>,
    decorations: &HashMap<Window, WindowDecorationState>,
    output: &Output,
    window: &Window,
) -> Vec<SolidColorRenderElement> {
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let Some(decoration) = decorations.get(window) else {
        return Vec::new();
    };

    decoration
        .buffers
        .iter()
        .filter_map(|cached| solid_rect_element(&cached.buffer, cached.rect, output_geo, scale))
        .collect()
}

fn solid_rect_element(
    buffer: &SolidColorBuffer,
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Option<SolidColorRenderElement> {
    intersect_logical_rect(rect, output_geo)?;
    let local =
        Point::from((rect.x - output_geo.loc.x, rect.y - output_geo.loc.y)).to_physical_precise_round(scale);

    Some(SolidColorRenderElement::from_buffer(
        buffer,
        local,
        scale,
        1.0,
        smithay::backend::renderer::element::Kind::Unspecified,
    ))
}

fn intersect_logical_rect(
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
) -> Option<LogicalRect> {
    let left = rect.x.max(output_geo.loc.x);
    let top = rect.y.max(output_geo.loc.y);
    let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
    let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);

    (right > left && bottom > top)
        .then(|| LogicalRect::new(left, top, right - left, bottom - top))
}
