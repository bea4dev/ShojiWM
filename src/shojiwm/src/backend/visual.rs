use smithay::{
    backend::renderer::{
        Renderer,
        element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
    },
    utils::{
        Buffer, Logical, Physical, Point, Rectangle, Scale, Transform, user_data::UserDataMap,
    },
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

pub fn precise_rect_from_logical(rect: LogicalRect) -> PreciseLogicalRect {
    PreciseLogicalRect {
        x: rect.x as f32,
        y: rect.y as f32,
        width: rect.width as f32,
        height: rect.height as f32,
    }
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

pub fn snapped_precise_logical_rect_relative_with_mode(
    rect: PreciseLogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
    mode: RectSnapMode,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let left = (((rect.x - origin.x as f32) as f64) * scale_x).round() / scale_x;
    let top = (((rect.y - origin.y as f32) as f64) * scale_y).round() / scale_y;
    let (right, bottom) = match mode {
        RectSnapMode::SharedEdges => (
            ((((rect.x + rect.width) - origin.x as f32) as f64) * scale_x).round() / scale_x,
            ((((rect.y + rect.height) - origin.y as f32) as f64) * scale_y).round() / scale_y,
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
    let snapped_global = snapped_logical_rect_relative_with_mode(rect, snap_origin, scale, mode);
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
    let snapped_global = snapped_logical_rect_relative_with_mode(rect, snap_origin, scale, mode);
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

pub fn snapped_precise_logical_rect_for_element(
    rect: PreciseLogicalRect,
    element_rect: PreciseLogicalRect,
    snap_origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;

    let element_left_px = ((element_rect.x - snap_origin.x as f32) * scale_x).round();
    let element_top_px = ((element_rect.y - snap_origin.y as f32) * scale_y).round();
    let element_right_px =
        (((element_rect.x + element_rect.width) - snap_origin.x as f32) * scale_x).round();
    let element_bottom_px =
        (((element_rect.y + element_rect.height) - snap_origin.y as f32) * scale_y).round();

    let snapped_left_px = ((rect.x - snap_origin.x as f32) * scale_x).round();
    let snapped_top_px = ((rect.y - snap_origin.y as f32) * scale_y).round();
    let snapped_right_px = (((rect.x + rect.width) - snap_origin.x as f32) * scale_x).round();
    let snapped_bottom_px = (((rect.y + rect.height) - snap_origin.y as f32) * scale_y).round();

    let local_left_px = snapped_left_px - element_left_px;
    let local_top_px = snapped_top_px - element_top_px;
    let local_width_px = (snapped_right_px - snapped_left_px).max(0.0);
    let local_height_px = (snapped_bottom_px - snapped_top_px).max(0.0);

    let element_width_px = (element_right_px - element_left_px).max(1.0);
    let element_height_px = (element_bottom_px - element_top_px).max(1.0);

    SnappedLogicalRect {
        x: local_left_px * element_rect.width.max(0.0001) / element_width_px,
        y: local_top_px * element_rect.height.max(0.0001) / element_height_px,
        width: local_width_px * element_rect.width.max(0.0001) / element_width_px,
        height: local_height_px * element_rect.height.max(0.0001) / element_height_px,
    }
}

pub fn precise_logical_rect_in_element_space(
    rect: PreciseLogicalRect,
    element_rect: PreciseLogicalRect,
) -> SnappedLogicalRect {
    SnappedLogicalRect {
        x: rect.x - element_rect.x,
        y: rect.y - element_rect.y,
        width: rect.width.max(0.0),
        height: rect.height.max(0.0),
    }
}

pub fn snapped_precise_logical_rect_in_area_space(
    rect: PreciseLogicalRect,
    element_rect: PreciseLogicalRect,
    area_width: i32,
    area_height: i32,
    snap_origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;
    let origin_x = snap_origin.x as f32;
    let origin_y = snap_origin.y as f32;

    let element_left_px = ((element_rect.x - origin_x) * scale_x).round();
    let element_top_px = ((element_rect.y - origin_y) * scale_y).round();
    let element_right_px = ((element_rect.x + element_rect.width - origin_x) * scale_x).round();
    let element_bottom_px = ((element_rect.y + element_rect.height - origin_y) * scale_y).round();

    let clip_left_px = ((rect.x - origin_x) * scale_x).round();
    let clip_top_px = ((rect.y - origin_y) * scale_y).round();
    let clip_right_px = ((rect.x + rect.width - origin_x) * scale_x).round();
    let clip_bottom_px = ((rect.y + rect.height - origin_y) * scale_y).round();

    let element_width_px = (element_right_px - element_left_px).max(1.0);
    let element_height_px = (element_bottom_px - element_top_px).max(1.0);
    let area_width = area_width.max(1) as f32;
    let area_height = area_height.max(1) as f32;

    SnappedLogicalRect {
        x: (clip_left_px - element_left_px) * area_width / element_width_px,
        y: (clip_top_px - element_top_px) * area_height / element_height_px,
        width: (clip_right_px - clip_left_px).max(0.0) * area_width / element_width_px,
        height: (clip_bottom_px - clip_top_px).max(0.0) * area_height / element_height_px,
    }
}

pub fn snapped_precise_logical_rect_in_root_frame_area_space(
    rect: PreciseLogicalRect,
    element_rect: PreciseLogicalRect,
    area_width: i32,
    area_height: i32,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    scale: Scale<f64>,
) -> SnappedLogicalRect {
    let element_physical =
        relative_physical_rect_from_root_precise(element_rect, root_rect, output_geo, scale);
    let clip_physical =
        relative_physical_rect_from_root_precise(rect, root_rect, output_geo, scale);
    let element_width_px = element_physical.size.w.max(1) as f32;
    let element_height_px = element_physical.size.h.max(1) as f32;
    let area_width = area_width.max(1) as f32;
    let area_height = area_height.max(1) as f32;

    SnappedLogicalRect {
        x: (clip_physical.loc.x - element_physical.loc.x) as f32 * area_width / element_width_px,
        y: (clip_physical.loc.y - element_physical.loc.y) as f32 * area_height / element_height_px,
        width: clip_physical.size.w.max(0) as f32 * area_width / element_width_px,
        height: clip_physical.size.h.max(0) as f32 * area_height / element_height_px,
    }
}

pub fn logical_rect_to_physical_buffer_rect(
    rect: LogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Buffer> {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let left = (((rect.x - origin.x) as f64) * scale_x).round() as i32;
    let top = (((rect.y - origin.y) as f64) * scale_y).round() as i32;
    let right = ((((rect.x + rect.width) - origin.x) as f64) * scale_x).round() as i32;
    let bottom = ((((rect.y + rect.height) - origin.y) as f64) * scale_y).round() as i32;
    Rectangle::new(
        Point::from((left, top)),
        ((right - left).max(0), (bottom - top).max(0)).into(),
    )
}

pub fn logical_rect_to_physical_buffer_rect_f64(
    rect: LogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<f64, Buffer> {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let left = ((rect.x - origin.x) as f64) * scale_x;
    let top = ((rect.y - origin.y) as f64) * scale_y;
    let right = (((rect.x + rect.width) - origin.x) as f64) * scale_x;
    let bottom = (((rect.y + rect.height) - origin.y) as f64) * scale_y;
    Rectangle::new(
        Point::from((left, top)),
        ((right - left).max(0.0), (bottom - top).max(0.0)).into(),
    )
}

pub fn precise_logical_rect_to_physical_buffer_rect(
    rect: PreciseLogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<f64, Buffer> {
    let scale_x = scale.x.abs().max(0.0001) as f32;
    let scale_y = scale.y.abs().max(0.0001) as f32;
    let origin_x = origin.x as f32;
    let origin_y = origin.y as f32;
    let left = (rect.x - origin_x) * scale_x;
    let top = (rect.y - origin_y) * scale_y;
    let right = (rect.x + rect.width - origin_x) * scale_x;
    let bottom = (rect.y + rect.height - origin_y) * scale_y;
    Rectangle::new(
        Point::from((left as f64, top as f64)),
        (
            (right - left).max(0.0) as f64,
            (bottom - top).max(0.0) as f64,
        )
            .into(),
    )
}

pub fn logical_rect_to_physical_rect(
    rect: LogicalRect,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let left = (((rect.x - origin.x) as f64) * scale_x).round() as i32;
    let top = (((rect.y - origin.y) as f64) * scale_y).round() as i32;
    let right = ((((rect.x + rect.width) - origin.x) as f64) * scale_x).round() as i32;
    let bottom = ((((rect.y + rect.height) - origin.y) as f64) * scale_y).round() as i32;
    Rectangle::new(
        Point::from((left, top)),
        ((right - left).max(0), (bottom - top).max(0)).into(),
    )
}

pub fn logical_point_to_physical_point_global_edges(
    point: Point<i32, Logical>,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let point_x = ((point.x as f64) * scale_x).round() as i32;
    let point_y = ((point.y as f64) * scale_y).round() as i32;
    let origin_x = ((origin.x as f64) * scale_x).round() as i32;
    let origin_y = ((origin.y as f64) * scale_y).round() as i32;
    Point::from((point_x - origin_x, point_y - origin_y))
}

pub fn precise_logical_point_to_physical_point_global_edges(
    point: Point<f64, Logical>,
    origin: Point<i32, Logical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    let point_x = (point.x * scale_x).round() as i32;
    let point_y = (point.y * scale_y).round() as i32;
    let origin_x = ((origin.x as f64) * scale_x).round() as i32;
    let origin_y = ((origin.y as f64) * scale_y).round() as i32;
    Point::from((point_x - origin_x, point_y - origin_y))
}

pub fn logical_point_to_relative_physical_point_from_output(
    point: Point<i32, Logical>,
    output_geo: Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    logical_point_to_relative_physical_point_from_origin(
        point,
        output_geo.loc,
        capture_origin_physical,
        scale,
    )
}

pub fn logical_point_to_relative_physical_point_from_origin(
    point: Point<i32, Logical>,
    output_origin: Point<i32, Logical>,
    capture_origin_physical: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    let global = logical_point_to_physical_point_global_edges(point, output_origin, scale);
    Point::from((
        global.x - capture_origin_physical.x,
        global.y - capture_origin_physical.y,
    ))
}

pub fn precise_logical_point_to_relative_physical_point_from_output(
    point: Point<f64, Logical>,
    output_geo: Rectangle<i32, Logical>,
    capture_origin_physical: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    precise_logical_point_to_relative_physical_point_from_origin(
        point,
        output_geo.loc,
        capture_origin_physical,
        scale,
    )
}

pub fn precise_logical_point_to_relative_physical_point_from_origin(
    point: Point<f64, Logical>,
    output_origin: Point<i32, Logical>,
    capture_origin_physical: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Point<i32, Physical> {
    let global = precise_logical_point_to_physical_point_global_edges(point, output_origin, scale);
    Point::from((
        global.x - capture_origin_physical.x,
        global.y - capture_origin_physical.y,
    ))
}

pub fn logical_size_to_physical_buffer_size(
    width: i32,
    height: i32,
    scale: Scale<f64>,
) -> (i32, i32) {
    let scale_x = scale.x.abs().max(0.0001);
    let scale_y = scale.y.abs().max(0.0001);
    (
        ((width as f64) * scale_x).round().max(0.0) as i32,
        ((height as f64) * scale_y).round().max(0.0) as i32,
    )
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

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
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

    fn framebuffer_capture_region(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.framebuffer_capture_region(scale)
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
        self.element
            .draw(frame, src, dst, damage, opaque_regions, cache)
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

/// Wraps a render element and discards its claimed opaque regions.
///
/// Applied when `COMPOSITOR.rendering.surfacePolicy` resolves
/// `opaqueRegion: "ignore"` for a surface: the damage tracker then neither
/// culls elements composited behind this one nor renders any of its pixels
/// with blending disabled. Used for clients that over-declare their opaque
/// region (e.g. GTK3 tooltips claim the full rect despite transparent
/// rounded corners).
#[derive(Debug)]
pub struct IgnoredOpaqueRegionElement<E> {
    element: E,
}

impl<E> IgnoredOpaqueRegionElement<E> {
    pub fn from_element(element: E) -> Self {
        Self { element }
    }
}

impl<E: Element> Element for IgnoredOpaqueRegionElement<E> {
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

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.element.damage_since(scale, commit)
    }

    fn opaque_regions(&self, _scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        OpaqueRegions::default()
    }

    fn alpha(&self) -> f32 {
        self.element.alpha()
    }

    fn kind(&self) -> Kind {
        self.element.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.element.is_framebuffer_effect()
    }

    fn framebuffer_capture_region(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.framebuffer_capture_region(scale)
    }
}

impl<R: Renderer, E: RenderElement<R>> RenderElement<R> for IgnoredOpaqueRegionElement<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.element
            .draw(frame, src, dst, damage, opaque_regions, cache)
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

pub fn requires_full_window_snapshot(visual: WindowVisualState) -> bool {
    // Translation and opacity can be applied directly to each render element. Scaling a complete
    // window element-by-element can expose subpixel rounding differences between its surfaces and
    // decoration nodes, so keep the single-snapshot path for scale animations.
    const VISUAL_IDENTITY_EPSILON: f64 = 1e-3;
    (visual.scale.x - 1.0).abs() >= VISUAL_IDENTITY_EPSILON
        || (visual.scale.y - 1.0).abs() >= VISUAL_IDENTITY_EPSILON
}

pub fn is_identity_visual_geometry(visual: WindowVisualState) -> bool {
    // Animation values pass through runtime serialization before reaching renderer space. Use a
    // small tolerance so a visually finished scale animation cannot leave a window on the more
    // expensive transform path indefinitely. Opacity is deliberately excluded: normal rendering
    // applies it directly to each element and does not require a full-window transform snapshot.
    visual.translation.x == 0 && visual.translation.y == 0 && !requires_full_window_snapshot(visual)
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

/// `root_physical_origin` with the sub-logical-pixel remainder of the managed
/// rect applied before the physical rounding. The integer layout pipeline is
/// unaware of the fraction, so shifting only this anchor moves the whole
/// rendered window rigidly at physical-pixel granularity.
pub fn root_physical_origin_precise(
    rect: LogicalRect,
    subpixel_offset: Point<f64, Logical>,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Point<i32, Physical> {
    Point::<f64, Logical>::from((
        (rect.x - output_geo.loc.x) as f64 + subpixel_offset.x,
        (rect.y - output_geo.loc.y) as f64 + subpixel_offset.y,
    ))
    .to_physical_precise_round(output_scale)
}

fn root_physical_size_from_edges(
    rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> smithay::utils::Size<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001);
    let scale_y = output_scale.y.abs().max(0.0001);
    let left = (((rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let top = (((rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;
    let right = ((((rect.x + rect.width) - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let bottom = ((((rect.y + rect.height) - output_geo.loc.y) as f64) * scale_y).round() as i32;
    ((right - left).max(0), (bottom - top).max(0)).into()
}

fn relative_physical_rect_in_root_frame(
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let root_width = (root_rect.width.max(1)) as f32;
    let root_height = (root_rect.height.max(1)) as f32;
    let root_size = root_physical_size_from_edges(root_rect, output_geo, output_scale);
    let root_x = root_rect.x as f32;
    let root_y = root_rect.y as f32;

    // The whole SSD tree is measured in one physical frame owned by the root
    // decoration rect. The root frame itself is snapped from output-global
    // left/right/top/bottom edges so its far edges stay fixed during fractional
    // scale resizes. Descendants are then rounded in this root-local frame,
    // preventing each element from choosing a different output-global phase.
    let to_px_x = |x: f32| (((x - root_x) / root_width) * root_size.w.max(0) as f32).round() as i32;
    let to_px_y =
        |y: f32| (((y - root_y) / root_height) * root_size.h.max(0) as f32).round() as i32;

    let left_px = to_px_x(left);
    let top_px = to_px_y(top);
    let right_px = to_px_x(right);
    let bottom_px = to_px_y(bottom);
    Rectangle::new(
        Point::from((left_px, top_px)),
        ((right_px - left_px).max(0), (bottom_px - top_px).max(0)).into(),
    )
}

pub fn relative_physical_rect_from_root(
    rect: LogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
    shared_rect: Option<LogicalRect>,
) -> Rectangle<i32, Physical> {
    let _ = shared_rect;
    relative_physical_rect_in_root_frame(
        rect.x as f32,
        rect.y as f32,
        (rect.x + rect.width) as f32,
        (rect.y + rect.height) as f32,
        root_rect,
        output_geo,
        output_scale,
    )
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
    relative_physical_rect_in_root_frame(
        rect.x,
        rect.y,
        rect.x + rect.width,
        rect.y + rect.height,
        root_rect,
        output_geo,
        output_scale,
    )
}

pub fn relative_physical_rect_from_root_global_edges(
    rect: LogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001);
    let scale_y = output_scale.y.abs().max(0.0001);
    let root_left_px = (((root_rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let root_top_px = (((root_rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;
    let left_px = (((rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let top_px = (((rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;
    let right_px = ((((rect.x + rect.width) - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let bottom_px = ((((rect.y + rect.height) - output_geo.loc.y) as f64) * scale_y).round() as i32;
    Rectangle::new(
        Point::from((left_px - root_left_px, top_px - root_top_px)),
        ((right_px - left_px).max(0), (bottom_px - top_px).max(0)).into(),
    )
}

pub fn relative_physical_rect_from_root_global_origin_size(
    rect: LogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001);
    let scale_y = output_scale.y.abs().max(0.0001);
    let root_left_px = (((root_rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let root_top_px = (((root_rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;
    let left_px = (((rect.x - output_geo.loc.x) as f64) * scale_x).round() as i32;
    let top_px = (((rect.y - output_geo.loc.y) as f64) * scale_y).round() as i32;
    let width_px = ((rect.width as f64) * scale_x).round().max(0.0) as i32;
    let height_px = ((rect.height as f64) * scale_y).round().max(0.0) as i32;
    Rectangle::new(
        Point::from((left_px - root_left_px, top_px - root_top_px)),
        (width_px, height_px).into(),
    )
}

pub fn relative_physical_rect_from_root_global_edges_precise(
    rect: PreciseLogicalRect,
    root_rect: LogicalRect,
    output_geo: Rectangle<i32, Logical>,
    output_scale: Scale<f64>,
) -> Rectangle<i32, Physical> {
    let scale_x = output_scale.x.abs().max(0.0001) as f32;
    let scale_y = output_scale.y.abs().max(0.0001) as f32;
    let output_x = output_geo.loc.x as f32;
    let output_y = output_geo.loc.y as f32;
    let root_left_px = (((root_rect.x as f32 - output_x) * scale_x).round()) as i32;
    let root_top_px = (((root_rect.y as f32 - output_y) * scale_y).round()) as i32;
    let left_px = (((rect.x - output_x) * scale_x).round()) as i32;
    let top_px = (((rect.y - output_y) * scale_y).round()) as i32;
    let right_px = ((((rect.x + rect.width) - output_x) * scale_x).round()) as i32;
    let bottom_px = ((((rect.y + rect.height) - output_y) * scale_y).round()) as i32;
    Rectangle::new(
        Point::from((left_px - root_left_px, top_px - root_top_px)),
        ((right_px - left_px).max(0), (bottom_px - top_px).max(0)).into(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PreciseLogicalRect, WindowVisualState, is_identity_visual_geometry,
        relative_physical_rect_from_root, relative_physical_rect_from_root_snapped_edges,
        requires_full_window_snapshot, snapped_precise_logical_rect_in_area_space,
        snapped_precise_logical_rect_in_element_space,
        snapped_precise_logical_rect_in_root_frame_area_space,
    };
    use crate::ssd::LogicalRect;
    use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

    #[test]
    fn opacity_only_visual_state_has_identity_geometry() {
        let visual = WindowVisualState {
            origin: Point::<i32, Physical>::from((10, 20)),
            scale: Scale::from((1.0, 1.0)),
            translation: Point::from((0, 0)),
            opacity: 0.5,
        };

        assert!(is_identity_visual_geometry(visual));
    }

    #[test]
    fn translated_visual_state_does_not_have_identity_geometry() {
        let visual = WindowVisualState {
            origin: Point::<i32, Physical>::from((10, 20)),
            scale: Scale::from((1.0, 1.0)),
            translation: Point::from((1, 0)),
            opacity: 1.0,
        };

        assert!(!is_identity_visual_geometry(visual));
    }

    #[test]
    fn translated_visual_state_does_not_require_full_window_snapshot() {
        let visual = WindowVisualState {
            origin: Point::<i32, Physical>::from((10, 20)),
            scale: Scale::from((1.0, 1.0)),
            translation: Point::from((1, 0)),
            opacity: 0.5,
        };

        assert!(!requires_full_window_snapshot(visual));
    }

    #[test]
    fn scaled_visual_state_requires_full_window_snapshot() {
        let visual = WindowVisualState {
            origin: Point::<i32, Physical>::from((10, 20)),
            scale: Scale::from((0.95, 1.0)),
            translation: Point::from((0, 0)),
            opacity: 1.0,
        };

        assert!(requires_full_window_snapshot(visual));
    }

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
        assert_eq!(local_a, local_b);
    }

    #[test]
    fn root_frame_uses_output_snapped_far_edges() {
        let output_geo = Rectangle::<i32, Logical>::new((0, 0).into(), (4000, 2000).into());
        let scale = Scale::from((1.25, 1.25));
        let root = LogicalRect::new(1, 0, 10, 10);

        let full = relative_physical_rect_from_root_snapped_edges(root, root, output_geo, scale);

        assert_eq!(full.loc.x, 0);
        assert_eq!(full.loc.y, 0);
        assert_eq!(full.size.w, 13);
        assert_eq!(full.size.h, 13);
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

    #[test]
    fn precise_clip_in_area_space_matches_shader_area_at_fractional_scale() {
        let scale = Scale::from((1.6, 1.6));
        let element = PreciseLogicalRect {
            x: 3.75,
            y: 3.75,
            width: 900.0,
            height: 30.0,
        };

        let clip = snapped_precise_logical_rect_in_area_space(
            element,
            element,
            900,
            30,
            Point::from((0, 0)),
            scale,
        );

        assert_eq!(clip.x, 0.0);
        assert_eq!(clip.y, 0.0);
        assert_eq!(clip.width, 900.0);
        assert_eq!(clip.height, 30.0);
    }

    #[test]
    fn root_frame_area_clip_matches_root_frame_geometry() {
        let output_geo = Rectangle::<i32, Logical>::new((0, 0).into(), (4000, 2000).into());
        let scale = Scale::from((1.25, 1.25));
        let root = LogicalRect::new(1, 0, 10, 10);
        let element = PreciseLogicalRect {
            x: 1.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };

        let clip = snapped_precise_logical_rect_in_root_frame_area_space(
            element, element, 10, 10, root, output_geo, scale,
        );

        assert_eq!(clip.x, 0.0);
        assert_eq!(clip.y, 0.0);
        assert_eq!(clip.width, 10.0);
        assert_eq!(clip.height, 10.0);
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
    let origin_x = reference_rect.x as f64 + reference_rect.width as f64 * transform.origin.x;
    let origin_y = reference_rect.y as f64 + reference_rect.height as f64 * transform.origin.y;

    let left = origin_x + (rect.x as f64 - origin_x) * transform.scale_x + transform.translate_x;
    let top = origin_y + (rect.y as f64 - origin_y) * transform.scale_y + transform.translate_y;
    let rect_right = rect.x.saturating_add(rect.width);
    let rect_bottom = rect.y.saturating_add(rect.height);
    let right =
        origin_x + (rect_right as f64 - origin_x) * transform.scale_x + transform.translate_x;
    let bottom =
        origin_y + (rect_bottom as f64 - origin_y) * transform.scale_y + transform.translate_y;

    let x = left.min(right).floor() as i32;
    let y = top.min(bottom).floor() as i32;
    let width = (left.max(right) - left.min(right)).ceil() as i32;
    let height = (top.max(bottom) - top.min(bottom)).ceil() as i32;

    LogicalRect::new(x, y, width.max(0), height.max(0))
}

pub fn transformed_precise_rect(
    rect: PreciseLogicalRect,
    reference_rect: LogicalRect,
    transform: WindowTransform,
) -> PreciseLogicalRect {
    let origin_x = reference_rect.x as f64 + reference_rect.width as f64 * transform.origin.x;
    let origin_y = reference_rect.y as f64 + reference_rect.height as f64 * transform.origin.y;

    let left = origin_x + (rect.x as f64 - origin_x) * transform.scale_x + transform.translate_x;
    let top = origin_y + (rect.y as f64 - origin_y) * transform.scale_y + transform.translate_y;
    let rect_right = rect.x as f64 + rect.width as f64;
    let rect_bottom = rect.y as f64 + rect.height as f64;
    let right = origin_x + (rect_right - origin_x) * transform.scale_x + transform.translate_x;
    let bottom = origin_y + (rect_bottom - origin_y) * transform.scale_y + transform.translate_y;

    PreciseLogicalRect {
        x: left.min(right) as f32,
        y: top.min(bottom) as f32,
        width: (left.max(right) - left.min(right)).max(0.0) as f32,
        height: (top.max(bottom) - top.min(bottom)).max(0.0) as f32,
    }
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
