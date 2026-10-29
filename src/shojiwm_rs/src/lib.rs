//! Write a ShojiWM config in Rust.
//!
//! Two levels are available:
//!
//! - **Reactive config** (recommended): the same model as the TypeScript
//!   SDK. Register everything on [`COMPOSITOR`] from a setup function and
//!   describe decorations with view builders whose props are signals; the
//!   runtime tracks dependencies per node and sends the compositor minimal
//!   patches.
//!
//!   ```no_run
//!   use shojiwm_rs::prelude::*;
//!
//!   fn main() -> std::process::ExitCode {
//!       run_config(|| {
//!           COMPOSITOR.key.bind("terminal", "Super+T", || {
//!               COMPOSITOR.process.spawn(Command::exec(["kitty"]));
//!           });
//!           COMPOSITOR.window.composition(|window| {
//!               let border = window
//!                   .is_focused()
//!                   .map(|focused| if *focused { hex("#d7ba7d") } else { hex("#4f5666") });
//!               WindowBorder::new()
//!                   .style(Style::new().border(2.0, border).border_radius(10.0))
//!                   .child(
//!                       Flex::column()
//!                           .child(Label::new(window.title()).style(Style::new().height(30.0)))
//!                           .child(ClientWindow::new()),
//!                   )
//!           });
//!       })
//!   }
//!   ```
//!
//! - **Raw runtime**: implement [`ConfigRuntime`] yourself and answer
//!   [`RuntimeRequest`]s in place (see [`RustLauncher`]).
//!
//! A raw config is an ordinary crate whose `main` hands a [`RustLauncher`] to
//! [`run`]. The config itself is a [`ConfigRuntime`]: it is called on the
//! compositor thread, answers [`RuntimeRequest`]s in place (no serialization,
//! no extra thread) and sends config deltas through the [`RuntimeHost`] it is
//! built with.
//!
//! ```no_run
//! use shojiwm_rs::*;
//!
//! struct MyConfig {
//!     host: RuntimeHost,
//! }
//!
//! impl ConfigRuntime for MyConfig {
//!     fn request(
//!         &mut self,
//!         _now_ms: f64,
//!         _request: RuntimeRequest<'_>,
//!     ) -> Result<RuntimeReply, RuntimeError> {
//!         // Anything not handled falls back to the compositor's defaults.
//!         Ok(RuntimeReply::Unhandled)
//!     }
//! }
//!
//! fn main() -> std::process::ExitCode {
//!     run(RustLauncher::new(|context| MyConfig { host: context.host }))
//! }
//! ```

pub mod adapter;
pub mod animation;
pub mod assets;
pub mod compositor;
pub mod dnd;
pub mod effect;
pub mod input_grab;
pub mod ipc;
pub mod output_composition;
pub mod overlay;
mod published;
pub mod reactive;
mod runtime;
pub mod style;
pub mod view;
pub mod window;
pub mod window_stack;

pub use adapter::{ConfigBuilder, ReactiveRuntime, run_config};
pub use compositor::COMPOSITOR;

/// Everything a reactive config usually needs.
pub mod prelude {
    pub use crate::{
        COMPOSITOR, ConfigBuilder,
        animation::{
            Animation, AnimationOptions, Easing, PollHandle, PollOutput, Repeat, TimerHandle,
            create_poll, create_poll_for_each_output, cubic_bezier, now_ms, set_interval,
            set_timeout,
        },
        compositor::{
            Command, CursorPosition, CursorPositionSnapshot, DisableEvent, EnableEvent,
            FramePacing, HotCorner, HotCornerOptions, InputChangeEvent, LockBackground,
            OutputChangeEvent, OutputConfig, OutputContext, OutputPower, OutputPowerOptions,
            PointerRegion, PointerRegionAnchor, PointerRegionEventSnapshot, PointerRegionOffset,
            PointerRegionPhase, PointerRegionSize, Sender, SurfaceRef,
        },
        dnd::{
            DragAction, DragActions, DragDevice, DragEnd, DragEndKind, DragIconInfo, DragPoint,
            DragSession, DragView, DragViewAnchor, DragViewOptions,
        },
        effect::{
            Effect, Include, Invalidate, PaintShader, ShaderStage, Source, Stage, StateTexture,
            SurfaceEffect, SurfaceEffects, Uniform, backdrop_source, blend, dual_kawase_blur,
            image_source, layer_source, noise, paint_shader, popup_source, render_to,
            render_to_if_dirty, save, saved, shader_input, shader_stage, state_source,
            state_texture, unit, window_source, xray_backdrop_source,
        },
        input_grab::{
            InputGrab, InputGrabCancelReason, InputGrabKeyEvent, InputGrabOptions,
            InputGrabPointerButtonEvent, InputGrabPointerMotionEvent, InputGrabScrollEvent,
            InputGrabState,
        },
        output_composition::{
            Camera, CompositionColor, CompositionRect, LayerName, LayerPopups, Layers, Mat4,
            OutputNode, OutputStack, PickablePlane, Plane, ProjectedPoint, RenderTexture, Scene3D,
            ScreenCamera, Solid, TextureView, Transform3D, Windows, look_at, output_logical_size,
            perspective, pick_plane, project_point, screen_camera, transform3d,
        },
        overlay::{Overlay, OverlayHandle, OverlayPlacement, snapshot_source},
        reactive::{
            Get, Memo, Prop, ReadSignal, Scope, Signal, batch, derive, effect, memo, on_cleanup,
            signal, untrack,
        },
        run_config,
        style::{Border, FontWeight, Style, Transform2D, hex, inset_shadow, rgba, shadow},
        view::{
            AppIcon, Button, Child, ClientWindow, Composition, Direction, Element, Flex, Image,
            Label, ManagedTransform, ManagedWindow, Popup, PopupTrigger, Rect, ShaderEffect,
            WindowBorder,
        },
        window::{AnimationMode, ManagedAnimation, Window, WindowStateKey},
        window_stack::{Placement, WindowStack},
    };
    pub use shojiwm_lib::ssd::{
        AlignItems, BlendMode, BoxShadow, Color, EffectRegion, ImageFit, JustifyContent, Overflow,
        PointerEvents, PopupAlign, PopupCollision, PopupDismissReason, PopupLayer, PopupMode,
        PopupPlacement, StylePosition, WindowAction,
    };
}

pub use shojiwm_lib::run;
pub use shojiwm_lib::runtime_api::{self, *};
/// Data types a config reads (snapshots) and builds (decoration trees,
/// effect configs).
pub use shojiwm_lib::ssd;
/// Config types the compositor consumes (outputs, input, processes, ...).
pub use shojiwm_lib::{
    config, cursor, frame_pacing, keyboard_layout, runtime_debug, runtime_input,
    runtime_input_grab, runtime_key_binding, runtime_pointer_region, runtime_process,
    runtime_workspace,
};

/// [`RuntimeLauncher`] for a config compiled into the binary.
pub struct RustLauncher<F> {
    name: &'static str,
    factory: F,
}

impl<F, R> RustLauncher<F>
where
    F: Fn(LaunchContext) -> R,
    R: ConfigRuntime + 'static,
{
    /// `factory` builds the config once the command line is parsed. Keep the
    /// context's `host` to send config deltas later.
    pub fn new(factory: F) -> Self {
        Self {
            name: "rust",
            factory,
        }
    }

    /// Name shown in logs and `--help` (default `"rust"`).
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }
}

impl<F, R> RuntimeLauncher for RustLauncher<F>
where
    F: Fn(LaunchContext) -> R,
    R: ConfigRuntime + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn launch(&self, context: LaunchContext) -> Box<dyn ConfigRuntime> {
        Box::new((self.factory)(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BindOnEnable {
        host: RuntimeHost,
    }

    impl ConfigRuntime for BindOnEnable {
        fn enable(&mut self) -> Result<(), RuntimeError> {
            self.host.send(HostMessage::ProcessActions(Vec::new()));
            Ok(())
        }

        fn request(
            &mut self,
            _now_ms: f64,
            request: RuntimeRequest<'_>,
        ) -> Result<RuntimeReply, RuntimeError> {
            Ok(match request {
                RuntimeRequest::Input(InputRequest::KeyBinding { .. }) => {
                    RuntimeReply::KeyBinding(ssd::DecorationKeyBindingInvocation {
                        invoked: true,
                        ..Default::default()
                    })
                }
                _ => RuntimeReply::Unhandled,
            })
        }
    }

    #[test]
    fn rust_runtime_answers_in_place_and_falls_back_otherwise() {
        let launcher = RustLauncher::new(|context| BindOnEnable { host: context.host });
        let args = cli::CommonArgs::parse(&[], launcher.extra_args());
        let host = RuntimeHost::detached();
        let mut runtime = RuntimeBoot::new(Box::new(launcher), &args).launch(host.clone());
        assert_eq!(runtime.name(), "rust");

        runtime.enable().unwrap();
        assert!(matches!(host.pop(), Some(HostMessage::ProcessActions(_))));
        assert!(runtime.invoke_key_binding("any", 0).unwrap().invoked);
        assert!(
            runtime
                .scheduler_tick(0.0)
                .unwrap()
                .next_poll_in_ms
                .is_none()
        );
    }

    /// A runtime whose new config takes a while to build: `prepare_reload`
    /// starts the build on another thread and returns at once, the old config
    /// keeps answering, and `ReloadReady` tells the compositor when `reload`
    /// can swap.
    struct SlowBuild {
        host: RuntimeHost,
        generation: u32,
        build: Option<std::sync::mpsc::Receiver<u32>>,
    }

    impl ConfigRuntime for SlowBuild {
        fn prepare_reload(&mut self) -> Result<ReloadPreparation, RuntimeError> {
            let (sender, receiver) = std::sync::mpsc::channel();
            let host = self.host.clone();
            let next = self.generation + 1;
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                sender.send(next).unwrap();
                host.send(HostMessage::ReloadReady(Ok(())));
            });
            self.build = Some(receiver);
            Ok(ReloadPreparation::Pending)
        }

        fn reload(&mut self) -> Result<(), RuntimeError> {
            let build = self
                .build
                .take()
                .ok_or(RuntimeError::Unsupported("reload without a prepared build"))?;
            self.generation = build
                .recv()
                .map_err(|error| RuntimeError::RuntimeProtocol(error.to_string()))?;
            Ok(())
        }

        fn request(
            &mut self,
            _now_ms: f64,
            request: RuntimeRequest<'_>,
        ) -> Result<RuntimeReply, RuntimeError> {
            Ok(match request {
                RuntimeRequest::Input(InputRequest::KeyBinding { .. }) => {
                    RuntimeReply::KeyBinding(ssd::DecorationKeyBindingInvocation {
                        invoked: self.generation > 0,
                        ..Default::default()
                    })
                }
                _ => RuntimeReply::Unhandled,
            })
        }
    }

    #[test]
    fn a_reload_can_be_prepared_in_the_background() {
        let host = RuntimeHost::detached();
        let runtime = SlowBuild {
            host: host.clone(),
            generation: 0,
            build: None,
        };
        let mut runtime = RuntimeHandle::new("slow", Box::new(runtime), host.clone());

        assert_eq!(
            runtime.prepare_reload().unwrap(),
            ReloadPreparation::Pending
        );
        // The old config keeps answering while the build runs.
        assert!(!runtime.invoke_key_binding("any", 0).unwrap().invoked);

        let ready = loop {
            if let Some(message) = host.pop() {
                break message;
            }
            std::thread::yield_now();
        };
        assert!(matches!(ready, HostMessage::ReloadReady(Ok(()))));
        runtime.reload().unwrap();
        assert!(runtime.invoke_key_binding("any", 0).unwrap().invoked);
    }
}
