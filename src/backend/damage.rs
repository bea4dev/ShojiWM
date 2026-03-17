use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        gles::{GlesError, GlesFrame, GlesRenderer},
        utils::{CommitCounter, OpaqueRegions},
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale},
};

use crate::ssd::LogicalRect;

#[derive(Debug, Clone)]
pub struct DamageOnlyElement {
    id: Id,
    rect: Rectangle<i32, Logical>,
}

impl DamageOnlyElement {
    pub fn new(rect: Rectangle<i32, Logical>) -> Self {
        Self { id: Id::new(), rect }
    }
}

impl Element for DamageOnlyElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        CommitCounter::default()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.rect.size.to_f64().to_buffer(1.0, smithay::utils::Transform::Normal))
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.rect.to_physical_precise_round(scale)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        0.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for DamageOnlyElement {
    fn draw(
        &self,
        _frame: &mut GlesFrame<'_, '_>,
        _src: Rectangle<f64, Buffer>,
        _dst: Rectangle<i32, Physical>,
        _damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&smithay::utils::user_data::UserDataMap>,
    ) -> Result<(), GlesError> {
        Ok(())
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

pub fn elements_for_output(
    rects: &[LogicalRect],
    output_geo: Rectangle<i32, Logical>,
) -> Vec<DamageOnlyElement> {
    rects.iter()
        .filter_map(|rect| {
            let left = rect.x.max(output_geo.loc.x);
            let top = rect.y.max(output_geo.loc.y);
            let right = (rect.x + rect.width).min(output_geo.loc.x + output_geo.size.w);
            let bottom = (rect.y + rect.height).min(output_geo.loc.y + output_geo.size.h);
            (right > left && bottom > top).then(|| {
                DamageOnlyElement::new(Rectangle::new(
                    Point::from((left - output_geo.loc.x, top - output_geo.loc.y)),
                    (right - left, bottom - top).into(),
                ))
            })
        })
        .collect()
}
