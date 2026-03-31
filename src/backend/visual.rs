use smithay::{
    backend::renderer::{
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Renderer,
    },
    utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Transform, user_data::UserDataMap},
};

use crate::ssd::{LogicalRect, WindowTransform};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnappedLogicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreciseLogicalRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectSnapMode {
    SharedEdges,
    OriginAndSize,
}

pub fn snapped_logical_rect_relative(
    rect: LogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    snapped_logical_rect_relative_with_mode(rect, origin, scale, RectSnapMode::SharedEdges)
}

pub fn snapped_logical_rect_relative_with_mode(
    rect: LogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
    mode: RectSnapMode,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let left = (((rect.x - origin.x) as f64) * scale_x).round() / scale_x;
    let top = (((rect.y - origin.y) as f64) * scale_y).round() / scale_y;
    let (right, bottom) = match mode {
        RectSnapMode::SharedEdges => (
            ((((rect.x + rect.width) - origin.x) as f64) * scale_x).round() / scale_x,
            ((((rect.y + rect.height) - origin.y) as f64) * scale_y).round() / scale_y,
        ),
        RectSnapMode::OriginAndSize => (
            left + ((rect.width as f64) * scale_x).round() / scale_x,
            top + ((rect.height as f64) * scale_y).round() / scale_y,
        ),
    };
    SnappedLogicalRect {
        x: left as f32,
        y: top as f32,
        width: (right - left).max(0.0) as f32,
        height: (bottom - top).max(0.0) as f32,
    }
}

pub fn snapped_logical_radius(radius: i32, scale: Scale<f64>) -> f32 {
    let scale_x = scale.x.abs().max(0.0001);
    (((radius.max(0)) as f64) * scale_x).round().max(0.0) as f32 / scale_x as f32
}

pub fn snapped_logical_rect_for_element(
    rect: LogicalRect,
    element_origin: Point<i32, Logical>,
    snap_origin: Point<i32, Logical>,
    scale: Scale<f64>,
    mode: RectSnapMode,
) -> SnappedLogicalRect {
    let snapped_global =
        snapped_logical_rect_relative_with_mode(rect, snap_origin, scale, mode);
    SnappedLogicalRect {
        x: snapped_global.x - (element_origin.x - snap_origin.x) as f32,
        y: snapped_global.y - (element_origin.y - snap_origin.y) as f32,
        width: snapped_global.width,
        height: snapped_global.height,
    }
}

pub fn snapped_logical_rect_in_element_space(
    rect: LogicalRect,
    element_rect: LogicalRect,
    snap_origin: Point<i32, Logical>,
    scale: Scale<f64>,
    mode: RectSnapMode,
) -> SnappedLogicalRect {
    let snapped_global =
        snapped_logical_rect_relative_with_mode(rect, snap_origin, scale, mode);
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);

    let element_left_px = (((element_rect.x - snap_origin.x) as f64) * scale_x).round() as f32;
    let element_top_px = (((element_rect.y - snap_origin.y) as f64) * scale_y).round() as f32;
    let element_width_px = ((element_rect.width as f64) * scale_x).round().max(1.0) as f32;
    let element_height_px = ((element_rect.height as f64) * scale_y).round().max(1.0) as f32;

    let snapped_left_px = ((snapped_global.x as f64) * scale_x).round() as f32;
    let snapped_top_px = ((snapped_global.y as f64) * scale_y).round() as f32;
    let snapped_right_px =
        (((snapped_global.x + snapped_global.width) as f64) * scale_x).round() as f32;
    let snapped_bottom_px =
        (((snapped_global.y + snapped_global.height) as f64) * scale_y).round() as f32;

    let local_left_px = snapped_left_px - element_left_px;
    let local_top_px = snapped_top_px - element_top_px;
    let local_width_px = (snapped_right_px - snapped_left_px).max(0.0);
    let local_height_px = (snapped_bottom_px - snapped_top_px).max(0.0);

    SnappedLogicalRect {
        x: local_left_px * element_rect.width.max(1) as f32 / element_width_px,
        y: local_top_px * element_rect.height.max(1) as f32 / element_height_px,
        width: local_width_px * element_rect.width.max(1) as f32 / element_width_px,
        height: local_height_px * element_rect.height.max(1) as f32 / element_height_px,
    }
}

pub fn snapped_precise_logical_rect_in_element_space(
    rect: PreciseLogicalRect,
    element_rect: PreciseLogicalRect,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;

    let element_left_px = (element_rect.x * scale_x).round();
    let element_top_px = (element_rect.y * scale_y).round();
    let snapped_left_px = (rect.x * scale_x).round();
    let snapped_top_px = (rect.y * scale_y).round();
    let snapped_right_px = ((rect.x + rect.width) * scale_x).round();
    let snapped_bottom_px = ((rect.y + rect.height) * scale_y).round();

    let local_left_px = snapped_left_px - element_left_px;
    let local_top_px = snapped_top_px - element_top_px;
    let local_width_px = (snapped_right_px - snapped_left_px).max(0.0);
    let local_height_px = (snapped_bottom_px - snapped_top_px).max(0.0);

    let element_width_px = ((element_rect.width * scale_x).round()).max(1.0);
    let element_height_px = ((element_rect.height * scale_y).round()).max(1.0);

    SnappedLogicalRect {
        x: local_left_px * element_rect.width.max(0.0001) / element_width_px,
        y: local_top_px * element_rect.height.max(0.0001) / element_height_px,
        width: local_width_px * element_rect.width.max(0.0001) / element_width_px,
        height: local_height_px * element_rect.height.max(0.0001) / element_height_px,
    }
}

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

pub fn root_physical_origin(
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Point<i32, Physical> {
    Point::<f64, Logical>::from((
        (rect.x - output_geo.loc.x) as f64,
        (rect.y - output_geo.loc.y) as f64,
    ))
    .to_physical_precise_round(output_scale)
}

pub fn relative_physical_rect_from_root(
    rect: LogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
    shared_rect: Option<LogicalRect>,
) -> Rectangle<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001);
    let scale_y = output_scale.y.abs().max(0.0001);
    let root_left_px = (((root_rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let root_top_px = (((root_rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;

    let anchored_left = shared_rect.is_some_and(|shared| rect.x == shared.x);
    let anchored_top = shared_rect.is_some_and(|shared| rect.y == shared.y);

    let left_px = if anchored_left {
        ((((rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32) - root_left_px
    } else {
        (((rect.x - root_rect.x) as f64) * scale_x).round() as i32
    };
    let top_px = if anchored_top {
        ((((rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32) - root_top_px
    } else {
        (((rect.y - root_rect.y) as f64) * scale_y).round() as i32
    };

    let width_px = ((rect.width as f64) * scale_x).round().max(0.0) as i32;
    let height_px = ((rect.height as f64) * scale_y).round().max(0.0) as i32;

    Rectangle::new(Point::from((left_px, top_px)), (width_px, height_px).into())
}

pub fn relative_physical_rect_from_root_snapped_edges(
    rect: LogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    relative_physical_rect_from_root_precise(
        PreciseLogicalRect {
            x: rect.x as f32,
            y: rect.y as f32,
            width: rect.width as f32,
            height: rect.height as f32,
        },
        root_rect,
        output_geo,
        output_scale,
    )
}

pub fn snapped_logical_rect_from_relative_physical(
    rect: Rectangle<i32, Physical>,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;
    SnappedLogicalRect {
        x: rect.loc.x as f32 / scale_x,
        y: rect.loc.y as f32 / scale_y,
        width: rect.size.w as f32 / scale_x,
        height: rect.size.h as f32 / scale_y,
    }
}

pub fn relative_physical_rect_from_root_precise(
    rect: PreciseLogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001) as f32;
    let scale_y = output_scale.y.abs().max(0.0001) as f32;
    let root_left_px =
        (((root_rect.x - output_geo.loc.x) as f64) * output_scale.x.abs().max(0.0001)).round() as i32;
    let root_top_px =
        (((root_rect.y - output_geo.loc.y) as f64) * output_scale.y.abs().max(0.0001)).round() as i32;
    let left_px = (((rect.x - output_geo.loc.x as f32) * scale_x).round()) as i32;
    let top_px = (((rect.y - output_geo.loc.y as f32) * scale_y).round()) as i32;
    let right_px = ((((rect.x + rect.width) - output_geo.loc.x as f32) * scale_x).round()) as i32;
    let bottom_px = ((((rect.y + rect.height) - output_geo.loc.y as f32) * scale_y).round()) as i32;
    Rectangle::new(
        Point::from((left_px - root_left_px, top_px - root_top_px)),
        ((right_px - left_px).max(0), (bottom_px - top_px).max(0)).into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PreciseLogicalRect, relative_physical_rect_from_root,
        relative_physical_rect_from_root_snapped_edges, snapped_precise_logical_rect_in_element_space,
    };
    use crate::ssd::LogicalRect;
    use smithay::utils::{Logical, Rectangle, Scale};

    #[test]
    fn snapped_edge_relative_rect_is_translation_stable_against_output() {
        let output_geo = Rectangle::<i32, Logical>::new((0, 0).into(), (4000, 2000).into());
        let scale = Scale::from((1.6, 1.6));

        let root_a = LogicalRect::new(100, 40, 200, 80);
        let child_a = LogicalRect::new(111, 40, 10, 10);
        let root_b = LogicalRect::new(101, 40, 200, 80);
        let child_b = LogicalRect::new(112, 40, 10, 10);

        let snapped_a =
            relative_physical_rect_from_root_snapped_edges(child_a, root_a, output_geo, scale);
        let snapped_b =
            relative_physical_rect_from_root_snapped_edges(child_b, root_b, output_geo, scale);
        let local_a = relative_physical_rect_from_root(child_a, root_a, output_geo, scale, None);
        let local_b = relative_physical_rect_from_root(child_b, root_b, output_geo, scale, None);

        assert_eq!(snapped_a, snapped_b);
        assert_ne!(local_a, local_b);
    }

    #[test]
    fn precise_clip_in_element_space_is_translation_stable() {
        let scale = Scale::from((1.6, 1.6));
        let element_a = PreciseLogicalRect {
            x: 100.0,
            y: 40.0,
            width: 18.75,
            height: 18.75,
        };
        let clip_a = PreciseLogicalRect {
            x: 101.875,
            y: 41.875,
            width: 15.0,
            height: 15.0,
        };
        let element_b = PreciseLogicalRect {
            x: 101.0,
            y: 40.0,
            width: 18.75,
            height: 18.75,
        };
        let clip_b = PreciseLogicalRect {
            x: 102.875,
            y: 41.875,
            width: 15.0,
            height: 15.0,
        };

        assert_eq!(
            snapped_precise_logical_rect_in_element_space(clip_a, element_a, scale),
            snapped_precise_logical_rect_in_element_space(clip_b, element_b, scale)
        );
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
