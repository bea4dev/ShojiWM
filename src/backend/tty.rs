use std::{collections::HashMap, path::Path, time::Duration};

use smithay::{
    backend::{
        allocator::{Fourcc, gbm::GbmAllocator},
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            compositor::FrameFlags,
            exporter::gbm::{GbmFramebufferExporter, NodeFilter},
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay, context::ContextPriority},
        renderer::{
            element::{
                AsRenderElements,
                memory::MemoryRenderBuffer,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
        },
        session::{Session, libseat::LibSeatSession},
    },
    input::pointer::{CursorImageAttributes, CursorImageStatus},
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::EventLoop,
        drm::control::{ModeTypeFlags, connector, crtc},
        gbm::{BufferObjectFlags, Device, Format},
        rustix::fs::OFlags,
    },
    render_elements,
    utils::{DeviceFd, IsAlive, Scale, Transform},
    wayland::compositor,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{debug, info, trace, warn};

use crate::{
    backend::decoration,
    backend::window as window_render,
    drawing::PointerRenderElement,
    state::ShojiWM,
};

const CLEAR_COLOR: [f32; 4] = [0.08, 0.10, 0.13, 1.0];

type GbmDrmOutput =
    DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

struct SurfaceData {
    output: Output,
    drm_output: GbmDrmOutput,
}

pub struct BackendData {
    pub drm_scanner: DrmScanner,
    pub drm_output_manager: DrmOutputManager<
        GbmAllocator<DrmDeviceFd>,
        GbmFramebufferExporter<DrmDeviceFd>,
        (),
        DrmDeviceFd,
    >,
    pub renderer: GlesRenderer,
    surfaces: HashMap<crtc::Handle, SurfaceData>,
}

pub fn device_added(
    state: &mut ShojiWM,
    event_loop: &EventLoop<ShojiWM>,
    session: &mut LibSeatSession,
    node: DrmNode,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(?node, path = ?path, "opening drm device");
    let fd = session.open(
        path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_events) = DrmDevice::new(fd.clone(), true)?;
    let gbm = Device::new(fd.clone())?;

    let egl = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new_with_priority(&egl, ContextPriority::High)?;
    let renderer = unsafe { GlesRenderer::new(ctx)? };

    let allocator = GbmAllocator::new(
        gbm.clone(),
        BufferObjectFlags::RENDERING | BufferObjectFlags::SCANOUT,
    );
    let exporter = GbmFramebufferExporter::new(gbm.clone(), NodeFilter::from(node));

    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        [Format::Argb8888],
        render_formats,
    );

    let backend = BackendData {
        drm_scanner: DrmScanner::new(),
        drm_output_manager,
        renderer,
        surfaces: HashMap::new(),
    };
    state.tty_backends.insert(node.clone(), backend);
    info!(?node, "drm backend stored in state");

    let backend = state.tty_backends.get_mut(&node).unwrap();

    event_loop
        .handle()
        .insert_source(drm_events, move |event, _, state| {
            if let DrmEvent::VBlank(crtc) = event {
                trace!(?node, ?crtc, "received drm vblank");
                frame_finish(state, node, crtc);
            }
        })?;

    for scan in backend
        .drm_scanner
        .scan_connectors(backend.drm_output_manager.device())?
    {
        debug!(?node, ?scan, "connector scan event");
        if let DrmScanEvent::Connected {
            connector,
            crtc: Some(crtc),
        } = scan
        {
            connector_connected(state, node, crtc, connector)?;
        }
    }

    Ok(())
}

fn frame_finish(state: &mut ShojiWM, node: DrmNode, crtc: crtc::Handle) {
    let Some(backend) = state.tty_backends.get_mut(&node) else {
        warn!(?node, ?crtc, "frame_finish without backend");
        return;
    };
    let Some(surface) = backend.surfaces.get_mut(&crtc) else {
        warn!(?node, ?crtc, "frame_finish without surface");
        return;
    };

    trace!(?node, ?crtc, "marking frame submitted");
    let _ = surface.drm_output.frame_submitted();
}

pub fn render_if_needed(state: &mut ShojiWM) -> Result<(), Box<dyn std::error::Error>> {
    if !state.needs_redraw {
        return Ok(());
    }

    state.refresh_window_decorations()?;

    debug!(
        backend_count = state.tty_backends.len(),
        window_count = state.space.elements().count(),
        "rendering pending redraw"
    );
    state.needs_redraw = false;

    let nodes: Vec<_> = state.tty_backends.keys().copied().collect();
    for node in nodes {
        let crtcs: Vec<_> = state
            .tty_backends
            .get(&node)
            .unwrap()
            .surfaces
            .keys()
            .copied()
            .collect();

        for crtc in crtcs {
            render_surface(state, node, crtc)?;
        }
    }

    Ok(())
}

render_elements! {
    pub TtyRenderElements<=GlesRenderer>;
    Window=WaylandSurfaceRenderElement<GlesRenderer>,
    Decoration=SolidColorRenderElement,
    Cursor=PointerRenderElement<GlesRenderer>,
}

fn render_surface(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = state
        .tty_backends
        .get(&node)
        .and_then(|backend| backend.surfaces.get(&crtc))
        .map(|surface| surface.output.clone())
        .unwrap();

    let ShojiWM {
        space,
        tty_backends,
        start_time,
        cursor_status,
        cursor_theme,
        pointer_element,
        seat,
        ..
    } = state;

    let backend = tty_backends.get_mut(&node).unwrap();
    let surface = backend.surfaces.get_mut(&crtc).unwrap();

    let mut elements: Vec<TtyRenderElements> =
        Vec::new();

    let pointer_pos = seat.get_pointer().unwrap().current_location();
    let output_geo = space.output_geometry(&output).unwrap();
    let scale = Scale::from(output.current_scale().fractional_scale());

    if output_geo.to_f64().contains(pointer_pos) {
        let reset = matches!(cursor_status, CursorImageStatus::Surface(surface) if !surface.alive());
        if reset {
            *cursor_status = CursorImageStatus::default_named();
        }

        let hotspot = if let CursorImageStatus::Surface(surface) = cursor_status {
            compositor::with_states(surface, |states| {
                states
                    .data_map
                    .get::<std::sync::Mutex<CursorImageAttributes>>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .hotspot
            })
        } else {
            let icon = match cursor_status {
                CursorImageStatus::Named(icon) => *icon,
                _ => smithay::input::pointer::CursorIcon::Default,
            };
            let frame = cursor_theme.get_image(icon, 1, start_time.elapsed());
            let buffer = MemoryRenderBuffer::from_slice(
                &frame.pixels_rgba,
                Fourcc::Argb8888,
                (frame.width as i32, frame.height as i32),
                1,
                Transform::Normal,
                None,
            );
            pointer_element.set_buffer(buffer);
            (frame.xhot as i32, frame.yhot as i32).into()
        };

        pointer_element.set_status(cursor_status.clone());

        let cursor_location = (pointer_pos - output_geo.loc.to_f64() - hotspot.to_f64())
            .to_physical(scale)
            .to_i32_round();

        elements.extend(pointer_element.render_elements(
            &mut backend.renderer,
            cursor_location,
            scale,
            1.0,
        )
        .into_iter()
        .map(TtyRenderElements::Cursor));
    }

    for window in space.elements_for_output(&output).rev() {
        let Some(window_location) = space.element_location(window) else {
            continue;
        };
        let render_location = window_location - window.geometry().loc;
        let physical_location = (render_location - output_geo.loc).to_physical_precise_round(scale);

        elements.extend(
            window_render::popup_elements(window, &mut backend.renderer, physical_location, scale, 1.0)
                .into_iter()
                .map(TtyRenderElements::Window),
        );

        elements.extend(
            decoration::solid_elements_for_window(space, &state.window_decorations, &output, window)
                .into_iter()
                .map(TtyRenderElements::Decoration),
        );

        elements.extend(
            window_render::surface_elements(
                window,
                &mut backend.renderer,
                physical_location,
                scale,
                1.0,
            )
            .into_iter()
            .map(TtyRenderElements::Window),
        );
    }

    debug!(
        ?node,
        ?crtc,
        output = %output.name(),
        window_count = space.elements().count(),
        render_element_count = elements.len(),
        cursor_status = ?cursor_status,
        "rendering tty surface"
    );

    let result = surface
        .drm_output
        .render_frame(&mut backend.renderer, &elements, CLEAR_COLOR, FrameFlags::DEFAULT)?;

    if !result.is_empty {
        debug!(output = %output.name(), "queueing tty frame");
        surface.drm_output.queue_frame(())?;

        space.elements().for_each(|window| {
            window.send_frame(
                &output,
                start_time.elapsed(),
                Some(Duration::ZERO),
                |_, _| Some(output.clone()),
            );
        });
    } else {
        trace!(output = %output.name(), "tty frame had no damage");
    }

    Ok(())
}

fn connector_connected(
    state: &mut ShojiWM,
    node: DrmNode,
    crtc: crtc::Handle,
    connector: connector::Info,
) -> Result<(), Box<dyn std::error::Error>> {
    let mode = connector
        .modes()
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .unwrap_or(connector.modes()[0]);

    let output = Output::new(
        format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        ),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: connector.subpixel().into(),
            make: "Unknown".into(),
            model: "Unknown".into(),
            serial_number: "Unknown".into(),
        },
    );
    let wl_mode = WlMode::from(mode);
    output.set_preferred(wl_mode);
    output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));
    output.create_global::<ShojiWM>(&state.display_handle);
    state.space.map_output(&output, (0, 0));
    info!(
        ?node,
        ?crtc,
        output = %output.name(),
        size = ?wl_mode.size,
        "connected tty output"
    );

    let backend = state.tty_backends.get_mut(&node).unwrap();

    let drm_output = backend
        .drm_output_manager
        .lock()
        .initialize_output::<_, WaylandSurfaceRenderElement<GlesRenderer>>(
            crtc,
            mode,
            &[connector.handle()],
            &output,
            None,
            &mut backend.renderer,
            &DrmOutputRenderElements::default(),
        )?;

    let surface = SurfaceData {
        output: output.clone(),
        drm_output,
    };
    backend.surfaces.insert(crtc, surface);
    debug!(?node, ?crtc, "stored tty surface");

    render_now(
        backend.surfaces.get_mut(&crtc).unwrap(),
        &mut backend.renderer,
    )?;
    Ok(())
}

fn render_now(
    surface: &mut SurfaceData,
    renderer: &mut GlesRenderer,
) -> Result<(), Box<dyn std::error::Error>> {
    let elements: Vec<SolidColorRenderElement> = Vec::new();

    debug!(output = %surface.output.name(), "rendering initial tty frame");
    let result =
        surface
            .drm_output
            .render_frame(renderer, &elements, CLEAR_COLOR, FrameFlags::DEFAULT)?;

    if !result.is_empty {
        surface.drm_output.queue_frame(())?;
    }

    Ok(())
}
