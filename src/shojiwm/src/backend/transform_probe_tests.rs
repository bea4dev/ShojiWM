//! Offscreen probes that pin the output-transform orientation contract of the
//! smithay fork: a solid rect rendered through `OutputDamageTracker` must land
//! at the position a physically rotated monitor would show it. Runs on the
//! render node without a session; skips when no GPU is available.
#![cfg(test)]

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::solid::{SolidColorBuffer, SolidColorRenderElement};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderbuffer, GlesRenderer};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
use smithay::utils::{Point, Rectangle, Size, Transform};

const OUT_W: i32 = 200;
const OUT_H: i32 = 100;
// Logical-space rect, well inside the top-left quadrant.
const RECT_X: i32 = 10;
const RECT_Y: i32 = 10;
const RECT_W: i32 = 40;
const RECT_H: i32 = 20;

fn try_renderer() -> Option<GlesRenderer> {
    let gbm = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/dri/renderD128")
        .ok()
        .and_then(|fd| smithay::backend::allocator::gbm::GbmDevice::new(fd).ok())?;
    let egl = unsafe { EGLDisplay::new(gbm).ok()? };
    let ctx = EGLContext::new(&egl).ok()?;
    unsafe { GlesRenderer::new(ctx).ok() }
}

/// Renders a red rect at (10,10,40,20) logical through the given output
/// transform and returns the bounding box of red pixels in the physical
/// readback (buffer rows top-to-bottom as returned by copy_framebuffer).
fn red_bounds_for_transform(transform: Transform) -> Option<Rectangle<i32, smithay::utils::Buffer>> {
    let mut renderer = try_renderer()?;
    let physical_size = Size::<i32, smithay::utils::Physical>::from((OUT_W, OUT_H));
    let mut buffer: GlesRenderbuffer = renderer
        .create_buffer(Fourcc::Abgr8888, physical_size.to_logical(1).to_buffer(1, Transform::Normal))
        .ok()?;
    let mut fb = renderer.bind(&mut buffer).ok()?;

    let mut tracker = OutputDamageTracker::new(physical_size, 1.0, transform);
    let solid = SolidColorBuffer::new(
        Size::<i32, smithay::utils::Logical>::from((RECT_W, RECT_H)),
        [1.0, 0.0, 0.0, 1.0],
    );
    let element = SolidColorRenderElement::from_buffer(
        &solid,
        Point::<i32, smithay::utils::Physical>::from((RECT_X, RECT_Y)),
        1.0,
        1.0,
        Kind::Unspecified,
    );
    tracker
        .render_output(
            &mut renderer,
            &mut fb,
            0,
            &[element],
            Color32F::new(0.0, 0.0, 0.0, 1.0),
        )
        .ok()?;

    let copy_rect = Rectangle::from_size(physical_size.to_logical(1).to_buffer(1, Transform::Normal));
    let mapping = renderer.copy_framebuffer(&fb, copy_rect, Fourcc::Abgr8888).ok()?;
    let bytes = renderer.map_texture(&mapping).ok()?;

    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for y in 0..OUT_H {
        for x in 0..OUT_W {
            let offset = ((y * OUT_W + x) * 4) as usize;
            let (r, g, b) = (bytes[offset], bytes[offset + 1], bytes[offset + 2]);
            if r > 200 && g < 50 && b < 50 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x == i32::MAX {
        return Some(Rectangle::default());
    }
    Some(Rectangle::new(
        (min_x, min_y).into(),
        (max_x - min_x + 1, max_y - min_y + 1).into(),
    ))
}

/// Renders a 40x20 memory texture (top-left 10x10 red, rest blue) at logical
/// (10,10) and returns (red_bounds, blue_bounds) in the physical readback.
fn texture_bounds_for_transform(
    transform: Transform,
) -> Option<(
    Rectangle<i32, smithay::utils::Buffer>,
    Rectangle<i32, smithay::utils::Buffer>,
)> {
    use smithay::backend::renderer::element::memory::{
        MemoryBuffer, MemoryRenderBuffer, MemoryRenderBufferRenderElement,
    };

    let mut renderer = try_renderer()?;
    let physical_size = Size::<i32, smithay::utils::Physical>::from((OUT_W, OUT_H));
    let mut buffer: GlesRenderbuffer = renderer
        .create_buffer(
            Fourcc::Abgr8888,
            physical_size.to_logical(1).to_buffer(1, Transform::Normal),
        )
        .ok()?;
    let mut fb = renderer.bind(&mut buffer).ok()?;

    let mut pixels = vec![0u8; (RECT_W * RECT_H * 4) as usize];
    for y in 0..RECT_H {
        for x in 0..RECT_W {
            let offset = ((y * RECT_W + x) * 4) as usize;
            let red = x < 10 && y < 10;
            pixels[offset] = if red { 255 } else { 0 };
            pixels[offset + 2] = if red { 0 } else { 255 };
            pixels[offset + 3] = 255;
        }
    }
    let mem = MemoryBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        Size::<i32, smithay::utils::Buffer>::from((RECT_W, RECT_H)),
    );
    let render_buffer = MemoryRenderBuffer::from_memory(mem, 1, Transform::Normal, None);
    let element = MemoryRenderBufferRenderElement::from_buffer(
        &mut renderer,
        Point::<f64, smithay::utils::Physical>::from((RECT_X as f64, RECT_Y as f64)),
        &render_buffer,
        None,
        None,
        None,
        Kind::Unspecified,
    )
    .ok()?;

    let mut tracker = OutputDamageTracker::new(physical_size, 1.0, transform);
    tracker
        .render_output(
            &mut renderer,
            &mut fb,
            0,
            &[element],
            Color32F::new(0.0, 0.0, 0.0, 1.0),
        )
        .ok()?;

    let copy_rect =
        Rectangle::from_size(physical_size.to_logical(1).to_buffer(1, Transform::Normal));
    let mapping = renderer
        .copy_framebuffer(&fb, copy_rect, Fourcc::Abgr8888)
        .ok()?;
    let bytes = renderer.map_texture(&mapping).ok()?;

    let bounds = |is_red: bool| -> Rectangle<i32, smithay::utils::Buffer> {
        let mut min_x = i32::MAX;
        let mut min_y = i32::MAX;
        let mut max_x = i32::MIN;
        let mut max_y = i32::MIN;
        for y in 0..OUT_H {
            for x in 0..OUT_W {
                let offset = ((y * OUT_W + x) * 4) as usize;
                let (r, b) = (bytes[offset], bytes[offset + 2]);
                let hit = if is_red {
                    r > 200 && b < 50
                } else {
                    b > 200 && r < 50
                };
                if hit {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        if min_x == i32::MAX {
            return Rectangle::default();
        }
        Rectangle::new(
            (min_x, min_y).into(),
            (max_x - min_x + 1, max_y - min_y + 1).into(),
        )
    };
    Some((bounds(true), bounds(false)))
}

// Positions below are what a physically rotated monitor must show for a
// logical rect at (10,10,40,20) on a 200x100 output. The readback rows are in
// scanout order, so these pin the DRM/tty presentation contract.
#[test]
fn output_transform_rotates_texture_content_on_offscreen_targets() {
    let Some((normal_red, normal_blue)) = texture_bounds_for_transform(Transform::Normal) else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    assert_eq!(normal_blue, Rectangle::new((10, 10).into(), (40, 20).into()));
    assert_eq!(normal_red, Rectangle::new((10, 10).into(), (10, 10).into()));

    // 180°: the rect lands at the antipode and its red top-left corner ends up
    // at the rotated rect's bottom-right.
    let (red, blue) = texture_bounds_for_transform(Transform::_180).unwrap();
    assert_eq!(blue, Rectangle::new((150, 70).into(), (40, 20).into()));
    assert_eq!(red, Rectangle::new((180, 80).into(), (10, 10).into()));

    // 90°: logical space is portrait (100x200); the rect near the logical
    // top-left renders into the physical bottom-left with swapped extents.
    let (red, blue) = texture_bounds_for_transform(Transform::_90).unwrap();
    assert_eq!(blue, Rectangle::new((10, 50).into(), (20, 40).into()));
    assert_eq!(red, Rectangle::new((10, 80).into(), (10, 10).into()));
}

#[test]
fn output_transform_places_solid_rects_on_offscreen_targets() {
    let Some(normal) = red_bounds_for_transform(Transform::Normal) else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    assert_eq!(normal, Rectangle::new((10, 10).into(), (40, 20).into()));
    assert_eq!(
        red_bounds_for_transform(Transform::_180).unwrap(),
        Rectangle::new((150, 70).into(), (40, 20).into())
    );
    assert_eq!(
        red_bounds_for_transform(Transform::_90).unwrap(),
        Rectangle::new((10, 50).into(), (20, 40).into())
    );
    assert_eq!(
        red_bounds_for_transform(Transform::Flipped).unwrap(),
        Rectangle::new((150, 10).into(), (40, 20).into())
    );
}

/// Round-trip contract for the backdrop framebuffer capture on transformed
/// outputs: content rendered through an output transform (framebuffer
/// orientation) and passed through `unrotate_captured_texture` must come back
/// in untransformed element orientation. Pins the transform-direction
/// convention the capture path in shader_effect.rs relies on.
#[test]
fn backdrop_capture_unrotate_restores_element_orientation() {
    use smithay::backend::renderer::element::memory::{
        MemoryBuffer, MemoryRenderBuffer, MemoryRenderBufferRenderElement,
    };
    use smithay::backend::renderer::gles::GlesTexture;

    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipping: no GPU render node available");
        return;
    };

    let mut pixels = vec![0u8; (RECT_W * RECT_H * 4) as usize];
    for y in 0..RECT_H {
        for x in 0..RECT_W {
            let offset = ((y * RECT_W + x) * 4) as usize;
            let red = x < 10 && y < 10;
            pixels[offset] = if red { 255 } else { 0 };
            pixels[offset + 2] = if red { 0 } else { 255 };
            pixels[offset + 3] = 255;
        }
    }
    let mem = MemoryBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        Size::<i32, smithay::utils::Buffer>::from((RECT_W, RECT_H)),
    );
    let render_buffer = MemoryRenderBuffer::from_memory(mem, 1, Transform::Normal, None);

    for output_transform in [
        Transform::_90,
        Transform::_180,
        Transform::_270,
        Transform::Flipped,
        Transform::Flipped90,
    ] {
        let physical_size = Size::<i32, smithay::utils::Physical>::from((OUT_W, OUT_H));
        // Untransformed (element-space) dimensions of the full-output capture.
        let element_size = output_transform.transform_size(physical_size);

        // 1. Render the pattern through the output transform, like the real
        //    frame target does.
        let mut frame_target: GlesTexture = renderer
            .create_buffer(
                Fourcc::Abgr8888,
                physical_size.to_logical(1).to_buffer(1, Transform::Normal),
            )
            .unwrap();
        {
            let element = MemoryRenderBufferRenderElement::from_buffer(
                &mut renderer,
                Point::<f64, smithay::utils::Physical>::from((RECT_X as f64, RECT_Y as f64)),
                &render_buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            )
            .unwrap();
            let mut fb = renderer.bind(&mut frame_target).unwrap();
            let mut tracker = OutputDamageTracker::new(physical_size, 1.0, output_transform);
            tracker
                .render_output(
                    &mut renderer,
                    &mut fb,
                    0,
                    &[element],
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                )
                .unwrap();
        }

        // 2. Un-rotate the captured framebuffer pixels back to element space.
        let element_buffer_size =
            Size::<i32, smithay::utils::Buffer>::from((element_size.w, element_size.h));
        let mut capture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, element_buffer_size)
            .unwrap();
        crate::backend::shader_effect::unrotate_captured_texture(
            &mut renderer,
            frame_target.clone(),
            output_transform,
            &mut capture,
            element_buffer_size,
        )
        .unwrap();

        // 3. The pattern must be back at its element-space position.
        let fb = renderer.bind(&mut capture).unwrap();
        let copy_rect = Rectangle::from_size(element_buffer_size);
        let mapping = renderer
            .copy_framebuffer(&fb, copy_rect, Fourcc::Abgr8888)
            .unwrap();
        let bytes = renderer.map_texture(&mapping).unwrap();

        let bounds = |is_red: bool| -> Rectangle<i32, smithay::utils::Buffer> {
            let mut min_x = i32::MAX;
            let mut min_y = i32::MAX;
            let mut max_x = i32::MIN;
            let mut max_y = i32::MIN;
            for y in 0..element_size.h {
                for x in 0..element_size.w {
                    let offset = ((y * element_size.w + x) * 4) as usize;
                    let (r, b) = (bytes[offset], bytes[offset + 2]);
                    let hit = if is_red {
                        r > 200 && b < 50
                    } else {
                        b > 200 && r < 50
                    };
                    if hit {
                        min_x = min_x.min(x);
                        min_y = min_y.min(y);
                        max_x = max_x.max(x);
                        max_y = max_y.max(y);
                    }
                }
            }
            if min_x == i32::MAX {
                return Rectangle::default();
            }
            Rectangle::new(
                (min_x, min_y).into(),
                (max_x - min_x + 1, max_y - min_y + 1).into(),
            )
        };

        assert_eq!(
            bounds(false),
            Rectangle::new((RECT_X, RECT_Y).into(), (RECT_W, RECT_H).into()),
            "blue rect must return to element space under {output_transform:?}"
        );
        assert_eq!(
            bounds(true),
            Rectangle::new((RECT_X, RECT_Y).into(), (10, 10).into()),
            "red corner must return to element top-left under {output_transform:?}"
        );
    }
}
