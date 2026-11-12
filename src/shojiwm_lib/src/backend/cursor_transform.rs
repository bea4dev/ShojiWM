//! `COMPOSITOR.cursor.setTransform`: the cursor drawn rotated and scaled
//! around its hotspot.
//!
//! The cursor's own elements (a theme image or a client surface tree) are drawn
//! untransformed into a texture, which is then drawn over the transformed
//! bounding box with a shader that maps each pixel back into that texture. The
//! result is an ordinary GL element, so a transformed cursor never goes on the
//! cursor plane and the fast cursor path stays off while it is shown.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, Color32F, Frame, Offscreen, Renderer, Texture,
            element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
            gles::{
                GlesError, GlesFrame, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
                UniformName, UniformType, UniformValue,
            },
            utils::CommitCounter,
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Scale, Transform, user_data::UserDataMap},
};

use crate::{cursor::CursorTransform, drawing::PointerRenderElement};

/// Where the transformed cursor lands and how its pixels map back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub dst: Rectangle<i32, Physical>,
    /// Column-major 3x3 from a position in `dst` (0..1) to the source
    /// texture (0..1).
    pub uv_matrix: [f32; 9],
}

/// `src` is the cursor's untransformed box and `pivot` its hotspot, both in
/// output pixels.
pub fn placement(
    src: Rectangle<i32, Physical>,
    pivot: Point<f64, Physical>,
    transform: &CursorTransform,
) -> Placement {
    let m = transform.matrix();
    let forward = |x: f64, y: f64| {
        let (dx, dy) = (x - pivot.x, y - pivot.y);
        (
            pivot.x + m[0][0] * dx + m[0][1] * dy,
            pivot.y + m[1][0] * dx + m[1][1] * dy,
        )
    };
    let (left, top) = (src.loc.x as f64, src.loc.y as f64);
    let (right, bottom) = (left + src.size.w as f64, top + src.size.h as f64);
    let corners = [
        forward(left, top),
        forward(right, top),
        forward(left, bottom),
        forward(right, bottom),
    ];
    let min_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::INFINITY, f64::min)
        .floor();
    let min_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::INFINITY, f64::min)
        .floor();
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil();
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil();
    let dst = Rectangle::new(
        (min_x as i32, min_y as i32).into(),
        (
            ((max_x - min_x) as i32).max(1),
            ((max_y - min_y) as i32).max(1),
        )
            .into(),
    );

    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    let inverse = [
        [m[1][1] / det, -m[0][1] / det],
        [-m[1][0] / det, m[0][0] / det],
    ];
    let dst_size = [dst.size.w as f64, dst.size.h as f64];
    let src_size = [src.size.w as f64, src.size.h as f64];
    let dst_loc = [dst.loc.x as f64, dst.loc.y as f64];
    let src_loc = [left, top];
    let pivot = [pivot.x, pivot.y];
    let linear = |row: usize, col: usize| inverse[row][col] * dst_size[col] / src_size[row];
    let offset = |row: usize| {
        let back = (0..2)
            .map(|col| inverse[row][col] * (dst_loc[col] - pivot[col]))
            .sum::<f64>();
        (back + pivot[row] - src_loc[row]) / src_size[row]
    };
    Placement {
        dst,
        uv_matrix: [
            linear(0, 0) as f32,
            linear(1, 0) as f32,
            0.0,
            linear(0, 1) as f32,
            linear(1, 1) as f32,
            0.0,
            offset(0) as f32,
            offset(1) as f32,
            1.0,
        ],
    }
}

#[derive(Debug)]
struct CursorTransformProgram(GlesTexProgram);

fn program(renderer: &mut GlesRenderer) -> Result<GlesTexProgram, GlesError> {
    if let Some(program) = renderer
        .egl_context()
        .user_data()
        .get::<CursorTransformProgram>()
    {
        return Ok(program.0.clone());
    }
    let compiled = renderer.compile_custom_texture_shader(
        include_str!("cursor_transform.frag"),
        &[UniformName::new("uv_matrix", UniformType::Matrix3x3)],
    )?;
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| CursorTransformProgram(compiled.clone()));
    Ok(compiled)
}

/// What the texture and the element were last made from.
#[derive(PartialEq)]
struct Drawn {
    sources: Vec<(Id, CommitCounter, Rectangle<i32, Physical>)>,
    placement: Placement,
}

/// The texture the untransformed cursor is drawn into, kept per output. It is
/// redrawn, and the element's commit moves, only when the cursor's elements or
/// the placement change, so a still cursor adds no damage.
pub struct CursorTransformCache {
    id: Id,
    commit: CommitCounter,
    texture: Option<GlesTexture>,
    drawn: Option<Drawn>,
}

impl Default for CursorTransformCache {
    fn default() -> Self {
        Self {
            id: Id::new(),
            commit: CommitCounter::default(),
            texture: None,
            drawn: None,
        }
    }
}

impl CursorTransformCache {
    /// `elements` as `render_elements` returned them, topmost first; `pivot`
    /// is the hotspot in output pixels. `None` when there is nothing to draw.
    pub fn render(
        &mut self,
        renderer: &mut GlesRenderer,
        elements: &[PointerRenderElement<GlesRenderer>],
        scale: Scale<f64>,
        pivot: Point<f64, Physical>,
        transform: &CursorTransform,
    ) -> Result<Option<TransformedCursorElement>, GlesError> {
        let Some(src) = elements
            .iter()
            .map(|element| element.geometry(scale))
            .reduce(|a, b| a.merge(b))
            .filter(|src| !src.is_empty())
        else {
            return Ok(None);
        };
        let sources = elements
            .iter()
            .map(|element| {
                let geometry = element.geometry(scale);
                (
                    element.id().clone(),
                    element.current_commit(),
                    Rectangle::new(geometry.loc - src.loc, geometry.size),
                )
            })
            .collect::<Vec<_>>();
        let placement = placement(src, pivot, transform);
        let content_unchanged = self
            .drawn
            .as_ref()
            .is_some_and(|drawn| drawn.sources == sources);
        if let (Some(drawn), Some(texture)) = (&self.drawn, &self.texture)
            && content_unchanged
        {
            if drawn.placement != placement {
                self.commit.increment();
                self.drawn = Some(Drawn { sources, placement });
            }
            return Ok(Some(TransformedCursorElement {
                id: self.id.clone(),
                commit: self.commit,
                texture: texture.clone(),
                placement,
                program: program(renderer)?,
            }));
        }
        let mut texture = match self.texture.take() {
            Some(texture)
                if (texture.width(), texture.height())
                    == (src.size.w as u32, src.size.h as u32) =>
            {
                texture
            }
            _ => Offscreen::<GlesTexture>::create_buffer(
                renderer,
                Fourcc::Abgr8888,
                (src.size.w, src.size.h).into(),
            )?,
        };
        {
            let mut framebuffer = renderer.bind(&mut texture)?;
            let mut frame = renderer.render(&mut framebuffer, src.size, Transform::Normal)?;
            frame.clear(Color32F::TRANSPARENT, &[Rectangle::from_size(src.size)])?;
            for (element, (_, _, dst)) in elements.iter().zip(&sources).rev() {
                let dst = *dst;
                element.draw(
                    &mut frame,
                    element.src(),
                    dst,
                    &[Rectangle::from_size(dst.size)],
                    &[],
                    None,
                )?;
            }
            let _ = frame.finish()?;
        }
        self.commit.increment();
        self.drawn = Some(Drawn { sources, placement });
        let element = TransformedCursorElement {
            id: self.id.clone(),
            commit: self.commit,
            texture: texture.clone(),
            placement,
            program: program(renderer)?,
        };
        self.texture = Some(texture);
        Ok(Some(element))
    }
}

#[derive(Debug)]
pub struct TransformedCursorElement {
    id: Id,
    commit: CommitCounter,
    texture: GlesTexture,
    placement: Placement,
    program: GlesTexProgram,
}

impl Element for TransformedCursorElement {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.commit
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(self.texture.size()).to_f64()
    }

    fn transform(&self) -> Transform {
        Transform::Normal
    }

    fn geometry(&self, _scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.placement.dst
    }

    fn alpha(&self) -> f32 {
        1.0
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for TransformedCursorElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        _opaque_regions: &[Rectangle<i32, Physical>],
        _cache: Option<&UserDataMap>,
    ) -> Result<(), GlesError> {
        frame.render_texture_from_to(
            &self.texture,
            src,
            dst,
            damage,
            &[],
            Transform::Normal,
            1.0,
            Some(&self.program),
            &[Uniform::new(
                "uv_matrix",
                UniformValue::Matrix3x3 {
                    matrices: vec![self.placement.uv_matrix],
                    transpose: false,
                },
            )],
        )
    }

    fn underlying_storage(&self, _renderer: &mut GlesRenderer) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uv(placement: &Placement, x: f64, y: f64) -> (f64, f64) {
        let dst = placement.dst;
        let v = (
            (x - dst.loc.x as f64) / dst.size.w as f64,
            (y - dst.loc.y as f64) / dst.size.h as f64,
        );
        let m = placement.uv_matrix.map(f64::from);
        (
            m[0] * v.0 + m[3] * v.1 + m[6],
            m[1] * v.0 + m[4] * v.1 + m[7],
        )
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    #[test]
    fn a_quarter_turn_swings_the_image_clockwise_around_the_hotspot() {
        let src = Rectangle::new((100, 100).into(), (10, 20).into());
        let transform = CursorTransform {
            rotate: 90.0,
            ..Default::default()
        };
        let placement = placement(src, (100.0, 100.0).into(), &transform);
        // Right of the hotspot turns to below it, below it turns to its left.
        assert_eq!(
            placement.dst,
            Rectangle::new((80, 100).into(), (20, 10).into())
        );
        assert!(close(uv(&placement, 100.0, 110.0), (1.0, 0.0)));
        assert!(close(uv(&placement, 80.0, 100.0), (0.0, 1.0)));
        assert!(close(uv(&placement, 100.0, 100.0), (0.0, 0.0)));
    }

    #[test]
    fn a_stretch_grows_the_image_away_from_the_hotspot() {
        let src = Rectangle::new((0, 0).into(), (16, 16).into());
        let transform = CursorTransform {
            scale_x: 2.0,
            scale_y: 0.5,
            ..Default::default()
        };
        let placement = placement(src, (4.0, 8.0).into(), &transform);
        assert_eq!(
            placement.dst,
            Rectangle::new((-4, 4).into(), (32, 8).into())
        );
        // The hotspot stays put, the far corner moves out with the scale.
        assert!(close(uv(&placement, 4.0, 8.0), (0.25, 0.5)));
        assert!(close(uv(&placement, 28.0, 12.0), (1.0, 1.0)));
    }

    #[test]
    fn a_stretch_along_a_turned_axis_leaves_the_image_upright() {
        let src = Rectangle::new((0, 0).into(), (10, 10).into());
        let transform = CursorTransform {
            scale_x: 2.0,
            scale_axis: 45.0,
            ..Default::default()
        };
        let placement = placement(src, (0.0, 0.0).into(), &transform);
        // The corner on the diagonal goes twice as far along it; the two
        // beside it are pushed half their length along it and no further.
        assert_eq!(
            placement.dst,
            Rectangle::new((0, 0).into(), (20, 20).into())
        );
        assert!(close(uv(&placement, 20.0, 20.0), (1.0, 1.0)));
        assert!(close(uv(&placement, 15.0, 5.0), (1.0, 0.0)));
        assert!(close(uv(&placement, 5.0, 15.0), (0.0, 1.0)));
        // A quarter turn of the axis is the same stretch on the other one.
        let turned = CursorTransform {
            scale_x: 2.0,
            scale_axis: 90.0,
            ..Default::default()
        };
        let other = CursorTransform {
            scale_y: 2.0,
            ..Default::default()
        };
        for (a, b) in turned
            .matrix()
            .iter()
            .flatten()
            .zip(other.matrix().iter().flatten())
        {
            assert!((a - b).abs() < 1e-9, "{a} {b}");
        }
    }

    #[test]
    fn identity_and_bad_values_are_told_apart() {
        assert_eq!(CursorTransform::default().validated(), Ok(None));
        let full_turn = CursorTransform {
            rotate: -360.0,
            ..Default::default()
        };
        assert_eq!(full_turn.validated(), Ok(None));
        let mirrored = CursorTransform {
            scale_x: -1.0,
            ..Default::default()
        };
        assert_eq!(mirrored.validated(), Ok(Some(mirrored)));
        for bad in [
            CursorTransform {
                scale_y: 0.0,
                ..Default::default()
            },
            CursorTransform {
                scale_x: 9.0,
                ..Default::default()
            },
            CursorTransform {
                rotate: f64::NAN,
                ..Default::default()
            },
            CursorTransform {
                scale_axis: f64::INFINITY,
                ..Default::default()
            },
        ] {
            assert!(bad.validated().is_err(), "{bad:?} should be refused");
        }
    }
}
