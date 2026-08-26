//! Render path for `ext-image-copy-capture-v1` frames.
//!
//! `Frame` objects arrive via [`ImageCopyCaptureHandler::frame`] but rendering
//! has to happen inside the backend's render loop where the `GlesRenderer` is
//! accessible. The handler therefore parks each request in
//! [`ShojiWM::image_copy_capture_pending`] alongside enough context to route
//! it to the right output (or, in Phase 5b-iii, the right toplevel). The
//! backend drains its share of the queue once per render pass.

use std::ptr;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::{AsRenderElements, RenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, ImportAll, ImportMem, Offscreen};
use smithay::desktop::{Space, Window};
use smithay::output::{Output, WeakOutput};
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::render_elements;
use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};
use smithay::wayland::foreign_toplevel_list::{ForeignToplevelHandle, ForeignToplevelWeakHandle};
use smithay::wayland::image_copy_capture::{
    BufferConstraints, CaptureFailureReason, Frame, SessionRef,
};
use smithay::wayland::shm;

use crate::backend::tty::TtyRenderElements;
use crate::drawing::PointerRenderElement;

// Sum type used only by the toplevel-capture render path so we can hand a
// single iterator (window content + translated cursor) to `render_to_shm`.
// The cursor is wrapped by reference because `PointerRenderElement` isn't
// `Clone` (its inner `WaylandSurfaceRenderElement` lacks the impl) and the
// raw cursor stack is still needed afterwards for the DRM render.
render_elements! {
    pub ToplevelCaptureElement<'a, R> where R: ImportAll + ImportMem;
    Window=WaylandSurfaceRenderElement<R>,
    TranslatedCursor=RelocateRenderElement<&'a PointerRenderElement<R>>,
}

/// A pending image-copy-capture frame held in the global queue. Drained by
/// whichever backend code path can satisfy it (outputs in 5b-ii, toplevels in
/// 5b-iii).
pub struct PendingCapture {
    pub frame: Frame,
    pub target: CaptureTarget,
    /// Whether the session asked for cursor (`paint_cursors` option = OBS
    /// "Show cursor" checkbox). When false, render functions skip the cursor
    /// elements before drawing the frame.
    pub draw_cursor: bool,
    /// The session this frame belongs to. Kept so the render path can push
    /// updated buffer constraints when the captured target resizes — the
    /// session events trigger the client to allocate a fresh buffer.
    pub session: SessionRef,
}

pub enum CaptureTarget {
    Output(WeakOutput),
    /// Reserved for Phase 5b-iii. Held in the queue but currently failed
    /// immediately by the render path.
    Toplevel(ForeignToplevelWeakHandle),
}

pub fn has_pending_output_capture(pending: &[PendingCapture], output: &Output) -> bool {
    pending.iter().any(|entry| match &entry.target {
        CaptureTarget::Output(weak) => weak.upgrade().is_some_and(|o| &o == output),
        CaptureTarget::Toplevel(_) => false,
    })
}

pub fn fail_pending_output_capture(
    pending: &mut Vec<PendingCapture>,
    output: &Output,
    reason: CaptureFailureReason,
) {
    let mut i = 0;
    while i < pending.len() {
        let matches = match &pending[i].target {
            CaptureTarget::Output(weak) => weak.upgrade().is_some_and(|o| &o == output),
            CaptureTarget::Toplevel(_) => false,
        };
        if !matches {
            i += 1;
            continue;
        }
        pending.remove(i).frame.fail(reason);
    }
}

/// Render any queued image-copy-capture frames whose target is `output`.
///
/// Consumes those entries from `pending`. Each handled frame either calls
/// `Frame::success` (rendered + presented_time) or `Frame::fail` (validation
/// or render failure).
pub fn process_image_copy_capture_for_output(
    pending: &mut Vec<PendingCapture>,
    renderer: &mut GlesRenderer,
    output: &Output,
    content_elements: &[TtyRenderElements],
    cursor_elements: &[TtyRenderElements],
    presented: Duration,
) {
    let mut i = 0;
    while i < pending.len() {
        let matches = match &pending[i].target {
            CaptureTarget::Output(weak) => weak.upgrade().is_some_and(|o| &o == output),
            CaptureTarget::Toplevel(_) => false,
        };
        if !matches {
            i += 1;
            continue;
        }
        let entry = pending.remove(i);
        let PendingCapture {
            frame, draw_cursor, ..
        } = entry;
        // Compose cursor only when the session asked for it (paint_cursors).
        // Reference slices to avoid cloning non-Clone TtyRenderElements.
        let composed_refs: Vec<&TtyRenderElements> = if draw_cursor {
            cursor_elements
                .iter()
                .chain(content_elements.iter())
                .collect()
        } else {
            content_elements.iter().collect()
        };
        match render_frame_for_output(renderer, output, &composed_refs, &frame) {
            Ok(()) => {
                // The buffer is rendered upright at the transformed (logical
                // orientation) size — see render_frame_for_output — so the
                // content carries no residual transform for the client to
                // undo. Reporting the output's transform here would make
                // transform-aware clients rotate an already-upright image.
                frame.success(Transform::Normal, None, presented);
            }
            Err(err) => {
                tracing::warn!(output = %output.name(), "image-copy-capture render failed: {err}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }
}

/// Render queued image-copy-capture frames whose target is a toplevel.
///
/// Looks up each toplevel's [`Window`] from `space`, then renders that
/// window's content (its surface tree + popups) into the frame's wl_buffer
/// at the window's geometry size. SHM Xrgb8888 only — same format the
/// constraints advertise.
pub fn process_image_copy_capture_for_toplevels(
    pending: &mut Vec<PendingCapture>,
    space: &Space<Window>,
    renderer: &mut GlesRenderer,
    cursor_pointer_elements: &[PointerRenderElement<GlesRenderer>],
    presented: Duration,
) {
    let mut i = 0;
    while i < pending.len() {
        if !matches!(pending[i].target, CaptureTarget::Toplevel(_)) {
            i += 1;
            continue;
        }
        let entry = pending.remove(i);
        let PendingCapture {
            frame,
            target,
            draw_cursor,
            session,
        } = entry;
        let CaptureTarget::Toplevel(weak) = target else {
            continue;
        };
        let Some(handle) = weak.upgrade() else {
            frame.fail(CaptureFailureReason::Unknown);
            continue;
        };

        // Detect window resize. The session advertised a buffer size when it
        // was created; if the window's current desired size differs (e.g.
        // user dragged a resize edge), the client's buffer is the wrong
        // shape and we must push fresh constraints + fail this frame so the
        // client allocates a new one.
        if let Some(window) = space.elements().find(|w| {
            w.user_data()
                .get::<ForeignToplevelHandle>()
                .is_some_and(|h| h.matches(&handle))
        }) && let Some(desired) = compute_desired_buffer_size(space, window)
        {
            let buffer_dims =
                smithay::wayland::shm::with_buffer_contents(&frame.buffer(), |_, _, data| {
                    (data.width, data.height)
                });
            if let Ok((bw, bh)) = buffer_dims
                && (bw != desired.w || bh != desired.h)
            {
                tracing::debug!(
                    bw,
                    bh,
                    desired_w = desired.w,
                    desired_h = desired.h,
                    "toplevel resized; pushing new constraints"
                );
                session.update_constraints(BufferConstraints {
                    size: desired,
                    shm: vec![
                        smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
                    ],
                    dma: None,
                });
                frame.fail(CaptureFailureReason::BufferConstraints);
                continue;
            }
        }

        match render_frame_for_toplevel(
            renderer,
            space,
            &handle,
            &frame,
            if draw_cursor {
                cursor_pointer_elements
            } else {
                &[]
            },
        ) {
            Ok(()) => {
                frame.success(Transform::Normal, None, presented);
            }
            Err(err) => {
                tracing::warn!("toplevel image-copy-capture render failed: {err}");
                frame.fail(CaptureFailureReason::Unknown);
            }
        }
    }
}

/// Compute what the buffer size for a window's capture should be *right now*
/// (mirrors the logic in `resolve_source_size` for toplevel sources). Used
/// to detect when an in-flight session's advertised constraints have gone
/// stale because the window was resized.
fn compute_desired_buffer_size(
    space: &Space<Window>,
    window: &Window,
) -> Option<smithay::utils::Size<i32, smithay::utils::Buffer>> {
    let geom = window.geometry();
    if geom.size.w <= 0 || geom.size.h <= 0 {
        return None;
    }
    let scale = space
        .outputs_for_element(window)
        .into_iter()
        .next()
        .map(|o| o.current_scale().fractional_scale())
        .unwrap_or(1.0);
    let w = ((geom.size.w as f64) * scale).round().max(1.0) as i32;
    let h = ((geom.size.h as f64) * scale).round().max(1.0) as i32;
    Some((w, h).into())
}

fn render_frame_for_toplevel(
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    handle: &ForeignToplevelHandle,
    frame: &Frame,
    cursor_pointer_elements: &[PointerRenderElement<GlesRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    let window = space
        .elements()
        .find(|w| {
            w.user_data()
                .get::<ForeignToplevelHandle>()
                .is_some_and(|h| h.matches(handle))
        })
        .cloned()
        .ok_or("toplevel handle not bound to any mapped window")?;
    let geom = window.geometry();
    if geom.size.w <= 0 || geom.size.h <= 0 {
        return Err("window has zero geometry".into());
    }
    // Cursor elements were built in `tty.rs` at the output's fractional
    // scale: their embedded location is in physical pixels at that scale. To
    // make their coordinates line up with the window content, render the
    // whole frame at the same scale rather than at 1.0. Window content uses
    // logical coords internally and renders cleanly at any scale, so this
    // also gives crisper output for HiDPI windows.
    //
    // `resolve_source_size` in handlers/mod.rs advertises the buffer at the
    // same scale so the negotiated buffer dims and our render size match.
    let scale: Scale<f64> = space
        .outputs_for_element(&window)
        .into_iter()
        .next()
        .map(|o| o.current_scale().fractional_scale().into())
        .unwrap_or_else(|| (1.0_f64).into());
    let size: Size<i32, Physical> = (
        ((geom.size.w as f64) * scale.x).round().max(1.0) as i32,
        ((geom.size.h as f64) * scale.y).round().max(1.0) as i32,
    )
        .into();

    // Element ordering convention: smithay's render_elements path treats the
    // FRONT of the slice as the top of the z-stack, and `render_to_shm`
    // iterates the slice in `.rev()` (so the back is drawn first, the front
    // last, on top). Cursor must therefore appear *before* the window
    // content so that — after the reversal — the window is drawn first and
    // the cursor lands on top.
    let mut elements: Vec<ToplevelCaptureElement<'_, GlesRenderer>> = Vec::new();

    // Cursor (front of slice = drawn on top).
    if !cursor_pointer_elements.is_empty()
        && let Some(window_loc) = space.element_location(&window)
    {
        // Cursor positions are workspace-physical at the output's scale;
        // subtract the window's geometry-origin position in the same
        // coordinate system to land them in buffer-local space.
        let geom_origin_phys: smithay::utils::Point<i32, Physical> = (
            ((window_loc.x + geom.loc.x) as f64 * scale.x).round() as i32,
            ((window_loc.y + geom.loc.y) as f64 * scale.y).round() as i32,
        )
            .into();
        for cursor in cursor_pointer_elements {
            let translated = RelocateRenderElement::from_element(
                cursor,
                (-geom_origin_phys.x, -geom_origin_phys.y),
                Relocate::Relative,
            );
            elements.push(ToplevelCaptureElement::TranslatedCursor(translated));
        }
    }

    // Window content (back of slice = drawn first / underneath). `geom.loc`
    // can be non-zero (CSD insets); subtract it (scaled to physical) so the
    // buffer's top-left lines up with the window's natural top-left.
    let location: smithay::utils::Point<i32, Physical> = (
        (-(geom.loc.x as f64) * scale.x).round() as i32,
        (-(geom.loc.y as f64) * scale.y).round() as i32,
    )
        .into();
    let window_elements: Vec<ToplevelCaptureElement<'_, GlesRenderer>> =
        window.render_elements(renderer, location, scale, 1.0);
    elements.extend(window_elements);

    let buffer = frame.buffer();
    render_to_shm(renderer, &buffer, size, scale, Transform::Normal, &elements)
}

fn render_frame_for_output(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: &[&TtyRenderElements],
    frame: &Frame,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = output.current_mode().ok_or("output has no current mode")?;
    let transform = output.current_transform();
    // The session advertises the buffer at the transformed (logical
    // orientation) size — see resolve_source_size — and the scene elements
    // already live in that space. Render them upright with Transform::Normal:
    // passing the output transform here would rotate the content a second
    // time AND make the damage tracker derive a swapped output geometry from
    // the already-swapped size, clipping the scene against the wrong bounds.
    let size = transform.transform_size(mode.size);
    let scale: Scale<f64> = output.current_scale().fractional_scale().into();

    // No region offset: ext-image-copy-capture captures the whole source.
    let relocated_elements: Vec<_> = elements
        .iter()
        .map(|element| RelocateRenderElement::from_element(*element, (0, 0), Relocate::Relative))
        .collect();
    let buffer = frame.buffer();
    render_to_shm(
        renderer,
        &buffer,
        size,
        scale,
        Transform::Normal,
        &relocated_elements,
    )
}

fn render_to_shm(
    renderer: &mut GlesRenderer,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    elements: &[impl RenderElement<GlesRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    shm::with_buffer_contents_mut(buffer, |shm_buffer, shm_len, buffer_data| {
        if !(buffer_data.format == wl_shm::Format::Xrgb8888
            && buffer_data.width == size.w
            && buffer_data.height == size.h
            && buffer_data.stride == size.w * 4
            && shm_len == buffer_data.stride as usize * buffer_data.height as usize)
        {
            return Err::<(), Box<dyn std::error::Error>>("invalid shm buffer format/size".into());
        }
        let mapping =
            render_and_download(renderer, size, scale, transform, Fourcc::Xrgb8888, elements)?;
        let bytes = renderer.map_texture(&mapping)?;
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), shm_buffer.cast(), shm_len);
        }
        Ok(())
    })??;
    Ok(())
}

fn render_and_download(
    renderer: &mut GlesRenderer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    fourcc: Fourcc,
    elements: &[impl RenderElement<GlesRenderer>],
) -> Result<smithay::backend::renderer::gles::GlesMapping, Box<dyn std::error::Error>> {
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let mut texture: GlesTexture = renderer.create_buffer(fourcc, buffer_size)?;
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut damage_tracker = OutputDamageTracker::new(size, scale, transform);
        let _ = damage_tracker.render_output(
            renderer,
            &mut target,
            0,
            elements,
            Color32F::new(0.0, 0.0, 0.0, 1.0),
        )?;
    }
    let target = renderer.bind(&mut texture)?;
    let mapping = renderer.copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)?;
    Ok(mapping)
}
