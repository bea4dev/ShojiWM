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

/// The winit window shows its GL framebuffer bottom row first, so what the
/// nested session displays is the readback with its rows reversed. Rendering
/// with `winit_render_transform` must therefore show exactly what a tty
/// scanout of the same transform shows.
#[test]
fn winit_framebuffer_flip_matches_tty_presentation_for_every_transform() {
    use super::winit::{ALL_TRANSFORMS, winit_render_transform};

    let flip_rows = |rect: Rectangle<i32, smithay::utils::Buffer>| {
        Rectangle::new(
            (rect.loc.x, OUT_H - rect.loc.y - rect.size.h).into(),
            rect.size,
        )
    };
    for transform in ALL_TRANSFORMS {
        let Some((tty_red, tty_blue)) = texture_bounds_for_transform(transform) else {
            eprintln!("skipping: no render node available");
            return;
        };
        let (nested_red, nested_blue) =
            texture_bounds_for_transform(winit_render_transform(transform))
                .expect("render node vanished mid-test");
        assert_eq!(flip_rows(nested_red), tty_red, "{transform:?}: red quadrant");
        assert_eq!(flip_rows(nested_blue), tty_blue, "{transform:?}: blue body");
    }
}

/// The HDR10 path composites into an fp16 intermediate and the DRM pass draws
/// that through the PQ encode element. The red rect must still land where the
/// direct render puts it, for every transform: the intermediate used to be
/// rendered with the output transform and then rotated again by the DRM pass.
#[test]
fn hdr_pipeline_keeps_the_direct_render_orientation() {
    use super::winit::ALL_TRANSFORMS;
    use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};

    // Red through the PQ encode is no longer pure red, so look for "reddish".
    let reddish_bounds = |bytes: &[u8]| {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in 0..OUT_H {
            for x in 0..OUT_W {
                let offset = ((y * OUT_W + x) * 4) as usize;
                let (r, g) = (i32::from(bytes[offset]), i32::from(bytes[offset + 1]));
                if r > 60 && r > g + 20 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        Rectangle::<i32, smithay::utils::Buffer>::new((x0, y0).into(), (x1 - x0 + 1, y1 - y0 + 1).into())
    };

    for transform in ALL_TRANSFORMS {
        let Some(direct) = red_bounds_for_transform(transform) else {
            eprintln!("skipping: no GPU render node available");
            return;
        };
        let mut renderer = try_renderer().expect("render node vanished mid-test");
        let output = Output::new(
            "HDR-PROBE".into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "probe".into(),
                model: "probe".into(),
                serial_number: "probe".into(),
            },
        );
        output.change_current_state(
            Some(Mode { size: (OUT_W, OUT_H).into(), refresh: 60_000 }),
            Some(transform),
            Some(OutputScale::Integer(1)),
            None,
        );
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
        let mut pipeline = None;
        let (encode, _) = crate::backend::hdr_pipeline::render_hdr_pipeline(
            &mut renderer,
            &mut pipeline,
            &output,
            &[element],
            [0.0, 0.0, 0.0, 1.0],
            crate::backend::hdr_pipeline::EncodeParams::new(203.0, 1000.0, crate::color::primaries::SRGB),
        )
        .expect("HDR pipeline should render")
        .expect("output has a mode");

        // Stand-in for the DRM pass: a damage tracker carrying the output transform.
        let physical_size = Size::<i32, smithay::utils::Physical>::from((OUT_W, OUT_H));
        let buffer_size = physical_size.to_logical(1).to_buffer(1, Transform::Normal);
        let mut buffer: GlesRenderbuffer = renderer
            .create_buffer(Fourcc::Abgr8888, buffer_size)
            .expect("scanout stand-in should allocate");
        let mut fb = renderer.bind(&mut buffer).expect("scanout stand-in should bind");
        OutputDamageTracker::new(physical_size, 1.0, transform)
            .render_output(&mut renderer, &mut fb, 0, &[encode], Color32F::new(0.0, 0.0, 0.0, 1.0))
            .expect("encode pass should render");
        let mapping = renderer
            .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
            .expect("readback should succeed");
        let bytes = renderer.map_texture(&mapping).expect("readback should map");
        assert_eq!(reddish_bounds(bytes), direct, "{transform:?}");
    }
}

/// The HDR encode carries compositing-space values past SDR white up to the
/// display's peak: 1.0 is SDR white, 2.0 a highlight (2^2.2 x SDR white), and
/// anything brighter than the peak clamps to it rather than to SDR white.
#[test]
fn hdr_encode_extends_past_sdr_white_to_the_display_peak() {
    use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};

    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    const SDR_WHITE: f32 = 203.0;
    const PEAK: f32 = 1000.0;
    let output = Output::new(
        "HDR-ENCODE".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "probe".into(),
            model: "probe".into(),
            serial_number: "probe".into(),
        },
    );
    output.change_current_state(
        Some(Mode { size: (30, 10).into(), refresh: 60_000 }),
        Some(Transform::Normal),
        Some(OutputScale::Integer(1)),
        None,
    );
    // Three 10x10 patches: SDR white, a highlight, and something past the peak.
    let patches = [1.0f32, 2.0, 4.0];
    let buffers = patches.map(|value| {
        SolidColorBuffer::new(
            Size::<i32, smithay::utils::Logical>::from((10, 10)),
            [value, value, value, 1.0],
        )
    });
    let elements: Vec<_> = buffers
        .iter()
        .enumerate()
        .map(|(index, buffer)| {
            SolidColorRenderElement::from_buffer(
                buffer,
                Point::<i32, smithay::utils::Physical>::from((index as i32 * 10, 0)),
                1.0,
                1.0,
                Kind::Unspecified,
            )
        })
        .collect();
    let mut pipeline = None;
    let (encode, _) = crate::backend::hdr_pipeline::render_hdr_pipeline(
        &mut renderer,
        &mut pipeline,
        &output,
        &elements,
        [0.0, 0.0, 0.0, 1.0],
        crate::backend::hdr_pipeline::EncodeParams::new(SDR_WHITE, PEAK, crate::color::primaries::SRGB),
    )
    .expect("HDR pipeline should render")
    .expect("output has a mode");

    let size = Size::<i32, smithay::utils::Physical>::from((30, 10));
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let mut target: GlesRenderbuffer = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .expect("scanout stand-in should allocate");
    let mut fb = renderer.bind(&mut target).expect("scanout stand-in should bind");
    OutputDamageTracker::new(size, 1.0, Transform::Normal)
        .render_output(&mut renderer, &mut fb, 0, &[encode], Color32F::new(0.0, 0.0, 0.0, 1.0))
        .expect("encode pass should render");
    let mapping = renderer
        .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .expect("readback should succeed");
    let bytes = renderer.map_texture(&mapping).expect("readback should map");

    // An 8-bit stand-in for the 10-bit scanout is precise enough to tell these
    // PQ levels apart (~0.58, ~0.75, ~0.75).
    let red_at = |x: i32| f32::from(bytes[(5 * 30 + x) as usize * 4]) / 255.0;
    let pq = |nits: f32| crate::color::colorimetry::pq_inverse_eotf(nits as f64) as f32;
    let gamma = crate::backend::hdr_pipeline::sdr_reference_gamma();
    let expected = [
        pq(SDR_WHITE),
        pq((2f32.powf(gamma) * SDR_WHITE).min(PEAK)),
        pq(PEAK),
    ];
    for (index, expected) in expected.into_iter().enumerate() {
        let got = red_at(index as i32 * 10 + 5);
        assert!(
            (got - expected).abs() < 1.5 / 255.0,
            "patch {} ({}): PQ {got}, expected {expected}",
            index,
            patches[index]
        );
    }
}

/// Encodes one solid compositing-space color through the HDR pipeline at
/// SDR white 203 / peak 1000 and returns the PQ signal read back (8-bit).
fn encode_solid(
    renderer: &mut GlesRenderer,
    color: [f32; 3],
    primaries: crate::color::primaries::PrimariesChromaticities,
) -> [f32; 3] {
    use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};

    let output = Output::new(
        "HDR-GAMUT".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "probe".into(),
            model: "probe".into(),
            serial_number: "probe".into(),
        },
    );
    output.change_current_state(
        Some(Mode { size: (10, 10).into(), refresh: 60_000 }),
        Some(Transform::Normal),
        Some(OutputScale::Integer(1)),
        None,
    );
    let buffer = SolidColorBuffer::new(
        Size::<i32, smithay::utils::Logical>::from((10, 10)),
        [color[0], color[1], color[2], 1.0],
    );
    let element = SolidColorRenderElement::from_buffer(
        &buffer,
        Point::<i32, smithay::utils::Physical>::from((0, 0)),
        1.0,
        1.0,
        Kind::Unspecified,
    );
    let mut pipeline = None;
    let (encode, _) = crate::backend::hdr_pipeline::render_hdr_pipeline(
        renderer,
        &mut pipeline,
        &output,
        &[element],
        [0.0, 0.0, 0.0, 1.0],
        crate::backend::hdr_pipeline::EncodeParams::new(203.0, 1000.0, primaries),
    )
    .expect("HDR pipeline should render")
    .expect("output has a mode");
    let size = Size::<i32, smithay::utils::Physical>::from((10, 10));
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let mut target: GlesRenderbuffer = renderer
        .create_buffer(Fourcc::Abgr8888, buffer_size)
        .expect("scanout stand-in should allocate");
    let mut fb = renderer.bind(&mut target).expect("scanout stand-in should bind");
    OutputDamageTracker::new(size, 1.0, Transform::Normal)
        .render_output(renderer, &mut fb, 0, &[encode], Color32F::new(0.0, 0.0, 0.0, 1.0))
        .expect("encode pass should render");
    let mapping = renderer
        .copy_framebuffer(&fb, Rectangle::from_size(buffer_size), Fourcc::Abgr8888)
        .expect("readback should succeed");
    let bytes = renderer.map_texture(&mapping).expect("readback should map");
    let at = (5 * 10 + 5) * 4;
    [0, 1, 2].map(|channel| f32::from(bytes[at + channel]) / 255.0)
}

/// SDR content shown in the panel's native gamut: pure compositing-space red
/// reaches the PQ signal as the panel's own red, not sRGB red.
#[test]
fn hdr_encode_takes_compositing_primaries_into_bt2020() {
    use crate::color::colorimetry::{chromaticity_conversion_matrix, pq_inverse_eotf};
    use crate::color::primaries::{BT2020, Chromaticity, PrimariesChromaticities, SRGB};

    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    let native = PrimariesChromaticities {
        red: Chromaticity { x: 0.6826, y: 0.3164 },
        green: Chromaticity { x: 0.2451, y: 0.7139 },
        blue: Chromaticity { x: 0.1396, y: 0.0439 },
        white: Chromaticity { x: 0.3125, y: 0.3291 },
    };
    for primaries in [SRGB, native] {
        let matrix = chromaticity_conversion_matrix(primaries, BT2020);
        let expected = [0, 1, 2].map(|row| pq_inverse_eotf(matrix[row][0].max(0.0) * 203.0) as f32);
        let got = encode_solid(&mut renderer, [1.0, 0.0, 0.0], primaries);
        for channel in 0..3 {
            assert!(
                (got[channel] - expected[channel]).abs() < 1.5 / 255.0,
                "{primaries:?} channel {channel}: PQ {} expected {}",
                got[channel],
                expected[channel]
            );
        }
    }
    // The two must differ, or the test proves nothing.
    let srgb = encode_solid(&mut renderer, [1.0, 0.0, 0.0], SRGB);
    let wide = encode_solid(&mut renderer, [1.0, 0.0, 0.0], native);
    assert!((srgb[1] - wide[1]).abs() > 5.0 / 255.0, "{srgb:?} vs {wide:?}");
}

/// The encode element reports only what stage 1 redrew, so the DRM pass
/// re-encodes a moved window's old and new rectangles instead of the whole
/// output, and a change of encode parameters re-encodes everything.
#[test]
fn hdr_encode_damage_follows_stage1_damage() {
    use crate::backend::hdr_pipeline::{EncodeParams, render_hdr_pipeline};
    use smithay::backend::renderer::element::Element;
    use smithay::utils::Scale;
    use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};

    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    let output = Output::new(
        "HDR-DAMAGE".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "probe".into(),
            model: "probe".into(),
            serial_number: "probe".into(),
        },
    );
    output.change_current_state(
        Some(Mode { size: (100, 100).into(), refresh: 60_000 }),
        Some(Transform::Normal),
        Some(OutputScale::Integer(1)),
        None,
    );
    let buffer = SolidColorBuffer::new(
        Size::<i32, smithay::utils::Logical>::from((10, 10)),
        [1.0, 0.5, 0.25, 1.0],
    );
    let at = |x: i32, y: i32| {
        SolidColorRenderElement::from_buffer(
            &buffer,
            Point::<i32, smithay::utils::Physical>::from((x, y)),
            1.0,
            1.0,
            Kind::Unspecified,
        )
    };
    let params = EncodeParams::new(203.0, 1000.0, crate::color::primaries::SRGB);
    let mut pipeline = None;
    let mut render = |element, params| {
        render_hdr_pipeline(
            &mut renderer,
            &mut pipeline,
            &output,
            &[element],
            [0.0, 0.0, 0.0, 1.0],
            params,
        )
        .expect("HDR pipeline should render")
        .expect("output has a mode")
        .0
    };
    let scale = Scale::from(1.0);
    let damage_of = |encode: &crate::backend::hdr_pipeline::HdrEncodeElement, since| {
        encode.damage_since(scale, Some(since)).into_iter().collect::<Vec<_>>()
    };
    let full = Rectangle::<i32, smithay::utils::Physical>::from_size((100, 100).into());

    let first = render(at(10, 10), params);
    let first_commit = first.current_commit();

    let unchanged = render(at(10, 10), params);
    assert_eq!(unchanged.current_commit(), first_commit);
    assert!(damage_of(&unchanged, first_commit).is_empty());

    let moved = render(at(50, 60), params);
    let damage = damage_of(&moved, first_commit);
    assert!(!damage.is_empty());
    assert!(!damage.contains(&full), "{damage:?}");
    for rect in [Rectangle::new((10, 10).into(), (10, 10).into()), Rectangle::new((50, 60).into(), (10, 10).into())] {
        assert!(
            damage.iter().any(|damaged| damaged.contains_rect(rect)),
            "{rect:?} missing from {damage:?}"
        );
    }
    let covered: i32 = damage.iter().map(|rect| rect.size.w * rect.size.h).sum();
    assert!(covered < 100 * 100 / 4, "{damage:?}");

    let brighter = render(at(50, 60), EncodeParams::new(300.0, 1000.0, crate::color::primaries::SRGB));
    assert_eq!(damage_of(&brighter, moved.current_commit()), vec![full]);

    // A commit the history no longer holds is a full re-encode too.
    assert_eq!(
        brighter.damage_since(scale, None).into_iter().collect::<Vec<_>>(),
        vec![full]
    );
}

/// The hardware cursor on HDR outputs is encoded on the CPU; it has to come
/// out as the encode pass would have drawn the same pixel.
#[test]
fn hdr_cursor_cpu_encode_matches_the_encode_pass() {
    use crate::color::primaries::{Chromaticity, PrimariesChromaticities, SRGB};

    let Some(mut renderer) = try_renderer() else {
        eprintln!("skipping: no GPU render node available");
        return;
    };
    let native = PrimariesChromaticities {
        red: Chromaticity { x: 0.6826, y: 0.3164 },
        green: Chromaticity { x: 0.2451, y: 0.7139 },
        blue: Chromaticity { x: 0.1396, y: 0.0439 },
        white: Chromaticity { x: 0.3125, y: 0.3291 },
    };
    for primaries in [SRGB, native] {
        let params = crate::backend::hdr_pipeline::EncodeParams::new(203.0, 1000.0, primaries);
        for pixel in [[255u8, 255, 255], [128, 128, 128], [200, 40, 90], [10, 230, 60]] {
            let gpu = encode_solid(&mut renderer, pixel.map(|value| value as f32 / 255.0), primaries);
            let cpu = crate::backend::hdr_cursor::encode_pixel(pixel, 255, &params);
            for channel in 0..3 {
                assert!(
                    (gpu[channel] * 255.0 - cpu[channel] as f32).abs() <= 1.0,
                    "{primaries:?} {pixel:?} channel {channel}: GPU {} CPU {}",
                    gpu[channel] * 255.0,
                    cpu[channel]
                );
            }
        }
    }
}
