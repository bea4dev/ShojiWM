//! Render path for wlr-screencopy frames.
//!
//! Given the elements that were just built for an output's normal render, this
//! produces a copy of the output (or a sub-region of it) into a client-provided
//! SHM or DMA-BUF buffer.

use std::ptr;

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
use smithay::output::Output;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};
use smithay::wayland::shm;

use crate::backend::tty::TtyRenderElements;
use crate::protocols::screencopy::{Screencopy, ScreencopyBuffer, ScreencopyManagerState};
use crate::state::ShojiWM;

/// Render and submit all queued screencopies that target `output`.
///
/// For with-damage screencopies, the per-queue damage tracker is consulted and
/// only submits when there's something new. Without-damage screencopies are
/// always rendered and submitted.
/// Render queued screencopies for `output`.
///
/// `content_elements` is the output's normal render minus the cursor;
/// `cursor_elements` is the cursor stack. The cursor is composited on top of
/// content only when the client requested `overlay_cursor=1`. That gives us
/// HIDDEN vs EMBEDDED cursor modes for output captures, since our compositor's
/// normal render path always includes the cursor.
pub fn process_screencopy_queue_for_output(
    screencopy_state: &mut ScreencopyManagerState,
    loop_handle: &LoopHandle<'_, ShojiWM>,
    renderer: &mut GlesRenderer,
    output: &Output,
    content_elements: &[TtyRenderElements],
    cursor_elements: &[TtyRenderElements],
) {
    timescope::scope!("screencopy queue output");
    let profile = std::env::var_os("SHOJI_SCREENCOPY_PROFILE").is_some();
    let entry_at = std::time::Instant::now();
    let mut processed = 0usize;
    let mut waited = 0usize;
    let output_name = output.name();

    screencopy_state.with_queues_mut(|queue| {
        loop {
            let (damage_tracker, front) = queue.split();
            let Some(front) = front else {
                return;
            };
            if front.output() != output {
                return;
            }
            let with_damage = front.with_damage();

            let buffer_kind = match front.buffer() {
                ScreencopyBuffer::Dmabuf(_) => "dmabuf",
                ScreencopyBuffer::Shm(_) => "shm",
            };
            let render_started_at = std::time::Instant::now();
            // Compose a per-frame element list by reference (TtyRenderElements
            // isn't Clone-able as a whole). Cursor is included only when the
            // client requested overlay_cursor (= EMBEDDED in portal terms).
            let composed_refs: Vec<&TtyRenderElements> = if front.overlay_cursor() {
                cursor_elements
                    .iter()
                    .chain(content_elements.iter())
                    .collect()
            } else {
                content_elements.iter().collect()
            };
            let render_result = {
                timescope::scope!("screencopy render frame");
                render_for_screencopy(renderer, output, &composed_refs, damage_tracker, front)
            };
            let render_elapsed = render_started_at.elapsed();

            match render_result {
                Ok((sync, damages)) => match damages {
                    Some(damages) => {
                        if with_damage {
                            let transform = output.current_transform();
                            let physical_size = transform.transform_size(front.buffer_size());
                            let damages = damages
                                .iter()
                                .map(|dmg| {
                                    dmg.to_logical(1).to_buffer(
                                        1,
                                        transform.invert(),
                                        &physical_size.to_logical(1),
                                    )
                                })
                                .collect::<Vec<_>>();
                            front.damage(damages.into_iter());
                        }
                        let has_sync = sync.is_some();
                        let screencopy = queue.pop();
                        screencopy.submit_after_sync(false, sync, loop_handle);
                        processed += 1;
                        if profile {
                            tracing::info!(
                                output = %output_name,
                                buffer_kind,
                                render_ms = render_elapsed.as_secs_f64() * 1000.0,
                                has_sync,
                                with_damage,
                                "screencopy: submitted frame"
                            );
                        }
                    }
                    None => {
                        waited += 1;
                        if profile {
                            tracing::info!(
                                output = %output_name,
                                buffer_kind,
                                render_ms = render_elapsed.as_secs_f64() * 1000.0,
                                "screencopy: no damage, deferring"
                            );
                        }
                        return;
                    }
                },
                Err(err) => {
                    tracing::warn!("error rendering for screencopy: {err:?}");
                    *damage_tracker = OutputDamageTracker::new((0, 0), 1.0, Transform::Normal);
                    let _ = queue.pop();
                }
            }
        }
    });

    if profile && (processed > 0 || waited > 0) {
        tracing::info!(
            output = %output_name,
            processed,
            waited,
            total_ms = entry_at.elapsed().as_secs_f64() * 1000.0,
            "screencopy: queue drain done"
        );
    }
}

/// `(sync point, damage regions)` produced by a screencopy render pass.
type ScreencopyRenderResult = Result<
    (Option<SyncPoint>, Option<Vec<Rectangle<i32, Physical>>>),
    Box<dyn std::error::Error>,
>;

fn render_for_screencopy(
    renderer: &mut GlesRenderer,
    output: &Output,
    elements: &[&TtyRenderElements],
    damage_tracker: &mut OutputDamageTracker,
    screencopy: &Screencopy,
) -> ScreencopyRenderResult
{
    use smithay::output::OutputModeSource;

    let OutputModeSource::Static {
        size: last_size,
        scale: last_scale,
        transform: last_transform,
    } = damage_tracker.mode().clone()
    else {
        return Err("damage tracker must have static mode".into());
    };

    let size = screencopy.buffer_size();
    let scale: Scale<f64> = output.current_scale().fractional_scale().into();
    let transform = output.current_transform();

    if size != last_size || scale != last_scale || transform != last_transform {
        *damage_tracker = OutputDamageTracker::new(size, scale, transform);
    }

    let region_loc = screencopy.region_loc();
    let relocated_elements = elements
        .iter()
        .map(|element| {
            // `element` is `&&TtyRenderElements`; deref once so the wrapped
            // type stays `&TtyRenderElements`.
            RelocateRenderElement::from_element(
                *element,
                region_loc.upscale(-1),
                Relocate::Relative,
            )
        })
        .collect::<Vec<_>>();

    let (damages, _) = damage_tracker
        .damage_output(1, &relocated_elements)
        .map_err(|e| format!("damage_output error: {e:?}"))?;
    let damages = damages.cloned();
    if screencopy.with_damage() && damages.is_none() {
        return Ok((None, None));
    }

    let sync = match screencopy.buffer() {
        ScreencopyBuffer::Dmabuf(dmabuf) => {
            let sync = render_to_dmabuf(
                renderer,
                dmabuf.clone(),
                size,
                scale,
                transform,
                &relocated_elements,
            )?;
            Some(sync)
        }
        ScreencopyBuffer::Shm(wl_buffer) => {
            render_to_shm(
                renderer,
                wl_buffer,
                size,
                scale,
                transform,
                &relocated_elements,
            )?;
            None
        }
    };

    Ok((sync, damages))
}

fn render_to_dmabuf(
    renderer: &mut GlesRenderer,
    mut dmabuf: Dmabuf,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    elements: &[impl RenderElement<GlesRenderer>],
) -> Result<SyncPoint, Box<dyn std::error::Error>> {
    timescope::scope!("screencopy render_to_dmabuf");
    let profile = std::env::var_os("SHOJI_SCREENCOPY_PROFILE").is_some();
    let bind_at = std::time::Instant::now();
    let mut target = renderer.bind(&mut dmabuf)?;
    let bind_ms = bind_at.elapsed().as_secs_f64() * 1000.0;
    let render_at = std::time::Instant::now();
    let mut damage_tracker = OutputDamageTracker::new(size, scale, transform);
    let sync = {
        timescope::scope!("screencopy dmabuf render_output");
        damage_tracker
            .render_output(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::new(0.0, 0.0, 0.0, 1.0),
            )?
            .sync
    };
    let render_ms = render_at.elapsed().as_secs_f64() * 1000.0;
    if profile {
        tracing::info!(bind_ms, render_ms, "screencopy: dmabuf bind+render");
    }
    Ok(sync)
}

fn render_to_shm(
    renderer: &mut GlesRenderer,
    buffer: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    size: Size<i32, Physical>,
    scale: Scale<f64>,
    transform: Transform,
    elements: &[impl RenderElement<GlesRenderer>],
) -> Result<(), Box<dyn std::error::Error>> {
    timescope::scope!("screencopy render_to_shm");
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
    timescope::scope!("screencopy render_and_download");
    let buffer_size = size.to_logical(1).to_buffer(1, Transform::Normal);
    let mut texture: GlesTexture = renderer.create_buffer(fourcc, buffer_size)?;
    {
        let mut target = renderer.bind(&mut texture)?;
        let mut damage_tracker = OutputDamageTracker::new(size, scale, transform);
        let _ = {
            timescope::scope!("screencopy shm render_output");
            damage_tracker.render_output(
                renderer,
                &mut target,
                0,
                elements,
                Color32F::new(0.0, 0.0, 0.0, 1.0),
            )?
        };
    }
    let target = renderer.bind(&mut texture)?;
    let mapping = {
        timescope::scope!("screencopy shm copy_framebuffer");
        renderer.copy_framebuffer(&target, Rectangle::from_size(buffer_size), fourcc)?
    };
    Ok(mapping)
}
