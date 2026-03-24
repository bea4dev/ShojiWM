use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Renderer,
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform, user_data::UserDataMap},
};

use crate::ssd::{LogicalRect, WindowTransform};

#[derive(Debug)]
pub struct AlphaRenderElement<E> {
    element: E,
    alpha: f32,
}

impl<E> AlphaRenderElement<E> {
    pub fn from_element(element: E, alpha: f32) -> Self {
        Self {
            element,
            alpha: alpha.clamp(0.0, 1.0),
        }
    }
}

impl<E: Element> Element for AlphaRenderElement<E> {
    fn id(&self) -> &Id {
        self.element.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.geometry(scale)
    }

    fn location(&self, scale: Scale<f64>) -> Point<i32, Physical> {
        self.element.location(scale)
    }

    fn transform(&self) -> Transform {
        self.element.transform()
    }

    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.element.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.element.alpha() * self.alpha
    }

    fn kind(&self) -> Kind {
        self.element.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.element.is_framebuffer_effect()
    }
}

impl<R: Renderer, E: RenderElement<R>> RenderElement<R> for AlphaRenderElement<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.element.draw(frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.element.underlying_storage(renderer)
    }

    fn capture_framebuffer(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), R::Error> {
        self.element.capture_framebuffer(frame, src, dst, cache)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WindowVisualState {
    pub origin: Point<i32, Physical>,
    pub scale: Scale<f64>,
    pub translation: Point<i32, Physical>,
    pub opacity: f32,
}

pub fn window_visual_state(
    rect: LogicalRect,
    transform: WindowTransform,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> WindowVisualState {
    let logical_origin = Point::<f64, Logical>::from((
        rect.x as f64 + rect.width as f64 * transform.origin.x,
        rect.y as f64 + rect.height as f64 * transform.origin.y,
    ));
    let origin = (logical_origin - output_geo.loc.to_f64()).to_physical_precise_round(output_scale);
    let translation = Point::<f64, Logical>::from((transform.translate_x, transform.translate_y))
        .to_physical_precise_round(output_scale);

    WindowVisualState {
        origin,
        scale: Scale::from((transform.scale_x.max(0.0), transform.scale_y.max(0.0))),
        translation,
        opacity: transform.opacity,
    }
}

pub fn transformed_root_rect(rect: LogicalRect, transform: WindowTransform) -> LogicalRect {
    transformed_rect(rect, rect, transform)
}

pub fn transformed_rect(
    rect: LogicalRect,
    reference_rect: LogicalRect,
    transform: WindowTransform,
) -> LogicalRect {
    let origin_x =
        reference_rect.x as f64 + reference_rect.width as f64 * transform.origin.x;
    let origin_y =
        reference_rect.y as f64 + reference_rect.height as f64 * transform.origin.y;

    let left = origin_x + (rect.x as f64 - origin_x) * transform.scale_x + transform.translate_x;
    let top = origin_y + (rect.y as f64 - origin_y) * transform.scale_y + transform.translate_y;
    let rect_right = rect.x.saturating_add(rect.width);
    let rect_bottom = rect.y.saturating_add(rect.height);
    let right = origin_x
        + (rect_right as f64 - origin_x) * transform.scale_x
        + transform.translate_x;
    let bottom = origin_y
        + (rect_bottom as f64 - origin_y) * transform.scale_y
        + transform.translate_y;

    let x = left.min(right).floor() as i32;
    let y = top.min(bottom).floor() as i32;
    let width = (left.max(right) - left.min(right)).ceil() as i32;
    let height = (top.max(bottom) - top.min(bottom)).ceil() as i32;

    LogicalRect::new(x, y, width.max(0), height.max(0))
}

pub fn inverse_transform_point(
    point: Point<f64, Logical>,
    rect: LogicalRect,
    transform: WindowTransform,
) -> Point<f64, Logical> {
    let origin_x = rect.x as f64 + rect.width as f64 * transform.origin.x;
    let origin_y = rect.y as f64 + rect.height as f64 * transform.origin.y;
    let scale_x = if transform.scale_x.abs() < f64::EPSILON {
        1.0
    } else {
        transform.scale_x
    };
    let scale_y = if transform.scale_y.abs() < f64::EPSILON {
        1.0
    } else {
        transform.scale_y
    };

    Point::from((
        origin_x + (point.x - transform.translate_x - origin_x) / scale_x,
        origin_y + (point.y - transform.translate_y - origin_y) / scale_y,
    ))
}

pub fn transform_point(
    point: Point<f64, Logical>,
    rect: LogicalRect,
    transform: WindowTransform,
) -> Point<f64, Logical> {
    let origin_x = rect.x as f64 + rect.width as f64 * transform.origin.x;
    let origin_y = rect.y as f64 + rect.height as f64 * transform.origin.y;

    Point::from((
        origin_x + (point.x - origin_x) * transform.scale_x + transform.translate_x,
        origin_y + (point.y - origin_y) * transform.scale_y + transform.translate_y,
    ))
}
