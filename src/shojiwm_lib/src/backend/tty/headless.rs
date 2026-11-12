//! The frames of an output with no connector ([`crate::headless_output`]).
//!
//! Nothing scans these out. Screen capture renders an output's scene into the
//! client's buffer on its own (see `render_surface`), so a frame here only
//! tracks damage, for the element states that drive frame callbacks and
//! presentation feedback, and a timer at the output's refresh rate stands in
//! for the vblank.

use std::time::Duration;

use smithay::{
    backend::{
        drm::DrmNode,
        renderer::{ImportDma, damage::OutputDamageTracker},
    },
    desktop::utils::OutputPresentationFeedback,
    output::{Mode, Output},
    reexports::{
        calloop::{
            LoopHandle,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{self, crtc},
    },
    utils::Transform,
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use super::{
    SurfaceData, SurfaceDmabufFeedback, SurfaceTarget, TtyRedrawState, TtyRenderElements,
    TtyRenderFrameResult, announce_output_icc_profiles, frame_finish,
};
use crate::state::ShojiWM;

/// Raw CRTC ids of headless outputs start here, far above any a DRM device
/// hands out, so they share the surface map with real CRTCs.
const FIRST_HEADLESS_CRTC: u32 = 0x7f00_0000;

pub(super) struct HeadlessTarget {
    damage_tracker: OutputDamageTracker,
    /// The output state `damage_tracker` was made for.
    tracked: Option<(Mode, f64, Transform)>,
    in_flight: Option<OutputPresentationFeedback>,
    /// Bumped by every queued frame, so a stale timer does nothing.
    generation: u64,
    frame_duration: Duration,
    node: DrmNode,
    crtc: crtc::Handle,
    loop_handle: LoopHandle<'static, ShojiWM>,
}

impl HeadlessTarget {
    pub(super) fn render_frame(
        &mut self,
        output: &Output,
        elements: &[TtyRenderElements],
    ) -> Result<TtyRenderFrameResult, Box<dyn std::error::Error>> {
        let current = output.current_mode().map(|mode| {
            (
                mode,
                output.current_scale().fractional_scale(),
                output.current_transform(),
            )
        });
        if current != self.tracked {
            self.damage_tracker = OutputDamageTracker::from_output(output);
            self.tracked = current;
        }
        let (damage, states) = self.damage_tracker.damage_output(1, elements)?;
        Ok(TtyRenderFrameResult {
            is_empty: damage.is_none(),
            primary_scanout: false,
            primary_plane_kind: "headless",
            overlay_plane_count: 0,
            overlay_details: Vec::new(),
            cursor_plane_assigned: false,
            states,
        })
    }

    /// Complete the frame one refresh period from now, as a vblank would.
    pub(super) fn queue_frame(&mut self, feedback: OutputPresentationFeedback) {
        self.in_flight = Some(feedback);
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (node, crtc) = (self.node, self.crtc);
        let timer = Timer::from_duration(self.frame_duration);
        let inserted = self.loop_handle.insert_source(timer, move |_, _, state| {
            let current = state
                .tty_backends
                .get(&node)
                .and_then(|backend| backend.surfaces.get(&crtc))
                .and_then(|surface| match &surface.target {
                    SurfaceTarget::Headless(headless) => Some(headless.generation),
                    SurfaceTarget::Drm(_) => None,
                });
            if current == Some(generation) {
                let loop_handle = state.loop_handle.clone();
                frame_finish(state, &loop_handle, node, crtc, &mut None);
                state.flush_pointer_dispatch();
            }
            TimeoutAction::Drop
        });
        if let Err(error) = inserted {
            tracing::warn!(?error, "could not schedule a headless output frame");
        }
    }

    pub(super) fn frame_submitted(&mut self) -> Option<OutputPresentationFeedback> {
        self.in_flight.take()
    }

    pub(super) fn reset_buffers(&mut self) {
        self.tracked = None;
    }
}

/// Give `output` a surface in the render loop of the GPU clients render
/// with. Without a TTY backend (a nested session, or tests) it gets none.
pub(crate) fn attach_headless_output(
    state: &mut ShojiWM,
    output: &Output,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(node) = state
        .tty_render_node
        .filter(|node| state.tty_backends.contains_key(node))
        .or_else(|| state.tty_backends.keys().next().copied())
    else {
        return Ok(());
    };
    let mode = output
        .current_mode()
        .ok_or("a headless output needs a mode")?;
    let frame_duration = Duration::from_secs_f64(1_000f64 / mode.refresh as f64);
    let loop_handle = state.loop_handle.clone();
    let backend = state.tty_backends.get_mut(&node).unwrap();
    let crtc = (FIRST_HEADLESS_CRTC..u32::MAX)
        .filter_map(control::from_u32::<crtc::Handle>)
        .find(|crtc| !backend.surfaces.contains_key(crtc))
        .ok_or("no free headless output id")?;
    let render =
        DmabufFeedbackBuilder::new(node.dev_id(), backend.renderer.dmabuf_formats()).build()?;
    let target = HeadlessTarget {
        damage_tracker: OutputDamageTracker::from_output(output),
        tracked: None,
        in_flight: None,
        generation: 0,
        frame_duration,
        node,
        crtc,
        loop_handle,
    };
    let mut surface = SurfaceData::new(
        output.clone(),
        SurfaceTarget::Headless(target),
        Vec::new(),
        SurfaceDmabufFeedback {
            scanout: render.clone(),
            render,
        },
        frame_duration,
        false,
        None,
    );
    surface.redraw_state = TtyRedrawState::Queued;
    backend.surfaces.insert(crtc, surface);
    announce_output_icc_profiles(state);
    Ok(())
}

/// Take `output` out of the render loop.
pub(crate) fn detach_headless_output(state: &mut ShojiWM, output: &Output) {
    for backend in state.tty_backends.values_mut() {
        backend.surfaces.retain(|_, surface| {
            !(matches!(surface.target, SurfaceTarget::Headless(_)) && surface.output == *output)
        });
    }
}
