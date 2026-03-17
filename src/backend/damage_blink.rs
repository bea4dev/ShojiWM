use smithay::{
    backend::renderer::{
        element::{Id, Kind, solid::SolidColorRenderElement},
        utils::CommitCounter,
    },
    utils::{Logical, Point, Rectangle, Scale},
};

use crate::ssd::LogicalRect;

const BLINK_FILL: [f32; 4] = [0.18, 1.0, 0.25, 0.18];
const BLINK_BORDER: [f32; 4] = [0.45, 1.0, 0.55, 0.85];
const BLINK_BORDER_WIDTH: i32 = 1;

pub fn elements_for_output(
    rects: &[LogicalRect],
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> Vec<SolidColorRenderElement> {
    let mut elements = Vec::new();

    for rect in rects {
        let left = rect.x.max(output_geo.loc.x);
        let top = rect.y.max(output_geo.loc.y);
        let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
        let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);

        if right <= left || bottom <= top {
            continue;
        }

        let local = Rectangle::new(
            Point::from((left - output_geo.loc.x, top - output_geo.loc.y)),
            (right - left, bottom - top).into(),
        );
        let physical = local.to_physical_precise_round(scale);

        elements.push(SolidColorRenderElement::new(
            Id::new(),
            physical,
            CommitCounter::default(),
            BLINK_FILL,
            Kind::Unspecified,
        ));

        let border_rects = [
            Rectangle::new(physical.loc, (physical.size.w, BLINK_BORDER_WIDTH).into()),
            Rectangle::new(
                Point::from((physical.loc.x, physical.loc.y + physical.size.h - BLINK_BORDER_WIDTH)),
                (physical.size.w, BLINK_BORDER_WIDTH).into(),
            ),
            Rectangle::new(physical.loc, (BLINK_BORDER_WIDTH, physical.size.h).into()),
            Rectangle::new(
                Point::from((physical.loc.x + physical.size.w - BLINK_BORDER_WIDTH, physical.loc.y)),
                (BLINK_BORDER_WIDTH, physical.size.h).into(),
            ),
        ];

        for border in border_rects {
            if border.size.w <= 0 || border.size.h <= 0 {
                continue;
            }

            elements.push(SolidColorRenderElement::new(
                Id::new(),
                border,
                CommitCounter::default(),
                BLINK_BORDER,
                Kind::Unspecified,
            ));
        }
    }

    elements
}
