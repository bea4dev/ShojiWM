//! The TypeScript config runtime: an embedded V8 isolate (RustyScript /
//! deno_core) on its own thread, running `tools/decoration-runtime.ts` and the
//! user's `index.tsx`. This is the runtime the default `shoji_wm` binary
//! ships with; it is one [`ConfigRuntime`] among possible others. The
//! compositor itself lives in `shojiwm_lib`.

mod embedded_runtime;
pub mod evaluator;
pub mod paths;

use tracing::warn;

use shojiwm_lib::{
    runtime_api::{
        ConfigRuntime, DecorationRequest, EffectRequest, InputRequest, LaunchContext,
        RuntimeError, RuntimeEvent, RuntimeLauncher, RuntimeReply, RuntimeRequest,
        WindowRequest, WorkspaceRequest, cli::ArgSpec,
    },
    ssd::DecorationEvaluator,
};

pub use evaluator::EmbeddedDecorationEvaluator;

#[derive(Debug, Clone, Copy, Default)]
pub struct TypeScriptLauncher;

const EXTRA_ARGS: &[ArgSpec] = &[ArgSpec {
    name: paths::DECORATION_RUNTIME_ARG,
    env: Some("SHOJI_DECORATION_RUNTIME"),
    value_name: "PATH",
    help: "Entry script of the TypeScript runtime",
}];

impl RuntimeLauncher for TypeScriptLauncher {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn default_config_path(&self, dev: bool) -> Option<std::path::PathBuf> {
        Some(paths::default_config_path(dev))
    }

    fn extra_args(&self) -> &'static [ArgSpec] {
        EXTRA_ARGS
    }

    fn launch(&self, context: LaunchContext) -> Box<dyn ConfigRuntime> {
        let paths = paths::decoration_runtime_paths(&context);
        let evaluator =
            EmbeddedDecorationEvaluator::for_paths(paths.script_path, paths.config_path)
                .with_working_dir(paths.working_dir)
                .with_host(context.host);
        Box::new(TypeScriptRuntime { evaluator })
    }
}

pub struct TypeScriptRuntime {
    evaluator: EmbeddedDecorationEvaluator,
}

impl TypeScriptRuntime {
    pub fn new(evaluator: EmbeddedDecorationEvaluator) -> Self {
        Self { evaluator }
    }
}

impl ConfigRuntime for TypeScriptRuntime {
    fn preload(&mut self) -> Result<(), RuntimeError> {
        self.evaluator.preload()
    }

    fn enable(&mut self) -> Result<(), RuntimeError> {
        self.evaluator.lifecycle_enable("initial", None).map(|_| ())
    }

    fn reload(&mut self) -> Result<(), RuntimeError> {
        // `onDisable` hands back the state the config wants to keep; the fresh
        // isolate receives it in `onEnable`.
        let persisted = match self.evaluator.lifecycle_disable("reload") {
            Ok(state) => state,
            Err(error) => {
                warn!(?error, "failed to collect runtime reload state");
                serde_json::Value::Object(Default::default())
            }
        };
        self.evaluator = self.evaluator.fresh_like();
        self.evaluator
            .lifecycle_enable("reload", Some(&persisted))
            .map(|_| ())
    }

    fn request(
        &mut self,
        now_ms: f64,
        request: RuntimeRequest<'_>,
    ) -> Result<RuntimeReply, RuntimeError> {
        let evaluator = &self.evaluator;
        let now = now_ms as u64;
        let reply = match request {
            RuntimeRequest::Decoration(request) => match request {
                DecorationRequest::Evaluate { window, preview } => {
                    let result = if preview {
                        evaluator.evaluate_window_preview(window, now)?
                    } else {
                        evaluator.evaluate_window(window, now)?
                    };
                    RuntimeReply::Evaluation(Box::new(result))
                }
                DecorationRequest::EvaluateCached {
                    window_id,
                    window,
                    force_full,
                } => RuntimeReply::CachedEvaluation(Box::new(
                    evaluator.evaluate_cached_window(window_id, window, now, force_full)?,
                )),
                DecorationRequest::Policy { window, context } => RuntimeReply::DecorationPolicy(
                    evaluator.window_decoration_policy(window, context)?,
                ),
                DecorationRequest::InvokeHandler {
                    window_id,
                    handler_id,
                } => RuntimeReply::Handler(Box::new(
                    evaluator.invoke_handler(window_id, handler_id, now)?,
                )),
                DecorationRequest::StartClose { window_id } => {
                    RuntimeReply::Handler(Box::new(evaluator.start_close(window_id, now)?))
                }
                DecorationRequest::Closed { window_id } => {
                    evaluator.window_closed(window_id)?;
                    RuntimeReply::Done
                }
            },
            RuntimeRequest::SchedulerTick(tick) => RuntimeReply::SchedulerTick(
                evaluator.scheduler_tick(now_ms, tick.output, tick.frame_outputs)?,
            ),
            RuntimeRequest::Window(request) => match request {
                WindowRequest::Resize { window_id, event } => {
                    RuntimeReply::WindowResize(evaluator.window_resize(window_id, event, now)?)
                }
                WindowRequest::Move { window_id, event } => {
                    RuntimeReply::WindowMove(evaluator.window_move(window_id, event, now)?)
                }
                WindowRequest::Maximize { window, event } => RuntimeReply::WindowStateRequest(
                    evaluator.window_maximize_request(window, event, now)?,
                ),
                WindowRequest::Minimize { window, event } => RuntimeReply::WindowStateRequest(
                    evaluator.window_minimize_request(window, event, now)?,
                ),
                WindowRequest::Fullscreen { window, event } => RuntimeReply::WindowStateRequest(
                    evaluator.window_fullscreen_request(window, event, now)?,
                ),
                WindowRequest::Activate { window, event } => RuntimeReply::WindowStateRequest(
                    evaluator.window_activate_request(window, event, now)?,
                ),
            },
            RuntimeRequest::Input(request) => match request {
                InputRequest::KeyBinding { binding_id } => {
                    RuntimeReply::KeyBinding(evaluator.invoke_key_binding(binding_id, now)?)
                }
                InputRequest::PointerMove(event) => {
                    RuntimeReply::PointerHook(evaluator.pointer_move(event, now)?)
                }
                InputRequest::GestureSwipe(event) => {
                    RuntimeReply::PointerHook(evaluator.gesture_swipe(event, now)?)
                }
                InputRequest::PointerRegion(event) => {
                    RuntimeReply::PointerHook(evaluator.pointer_region(event, now)?)
                }
                InputRequest::InputGrab { grab_id, event } => {
                    RuntimeReply::PointerHook(evaluator.input_grab_event(grab_id, event, now)?)
                }
            },
            RuntimeRequest::Effect(request) => match request {
                EffectRequest::Background => {
                    RuntimeReply::BackgroundEffect(evaluator.background_effect_config()?)
                }
                EffectRequest::Layers {
                    output_name,
                    layers,
                } => RuntimeReply::LayerEffects(
                    evaluator.evaluate_layer_effects(output_name, layers, now)?,
                ),
                EffectRequest::Popups {
                    output_name,
                    popups,
                } => RuntimeReply::PopupEffects(
                    evaluator.evaluate_popup_effects(output_name, popups, now)?,
                ),
            },
            RuntimeRequest::Workspace(WorkspaceRequest::Activate(event)) => {
                RuntimeReply::Handler(Box::new(evaluator.workspace_activate(event, now)?))
            }
        };
        evaluator.forward_published_state();
        Ok(reply)
    }

    fn post(&mut self, now_ms: f64, event: RuntimeEvent) {
        let now = now_ms as u64;
        match event {
            RuntimeEvent::DisplayState(state) => self.evaluator.set_display_state(state),
            RuntimeEvent::InputState(state) => self.evaluator.set_input_state(state),
            RuntimeEvent::KeyboardLayout(layout) => {
                self.evaluator.set_keyboard_layout(layout);
            }
            RuntimeEvent::PointerMove(event) => self.evaluator.pointer_move_async(event, now),
            RuntimeEvent::GestureSwipe(event) => self.evaluator.gesture_swipe_async(event, now),
        }
    }

    fn shutdown(&mut self) {
        self.evaluator.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use shojiwm_lib::runtime_api::{
        HostMessage, NullRuntime, ReloadPreparation, RuntimeHandle, RuntimeHost, cli::CommonArgs,
    };

    fn key_binding_ids(host: &RuntimeHost) -> Option<Vec<String>> {
        std::iter::from_fn(|| host.pop())
            .filter_map(|message| match message {
                HostMessage::KeyBindings(update) => Some(
                    update
                        .entries
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .last()
    }

    /// The whole lifecycle through the trait the compositor uses: config deltas
    /// reach the host, requests are answered, and `reload` swaps the config.
    #[test]
    fn typescript_runtime_runs_through_the_runtime_api() {
        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-runtime-api-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        let write_config = |extra_binding: bool| {
            let extra = if extra_binding {
                r#"COMPOSITOR.key.bind("second", "Super+Y", () => {});"#
            } else {
                ""
            };
            std::fs::write(
                &config_path,
                format!(
                    r#"
import {{ COMPOSITOR, Label }} from "shoji_wm";
COMPOSITOR.key.bind("first", "Super+T", () => {{}});
{extra}
COMPOSITOR.window.composition = (window) => <Label text={{window.title}} />;
"#
                ),
            )
            .expect("test config should be written");
        };

        write_config(false);
        let args = CommonArgs::parse(
            &[
                "--dev".to_string(),
                "--config".to_string(),
                config_path.display().to_string(),
                "--runtime-dir".to_string(),
                repository_root.display().to_string(),
            ],
            TypeScriptLauncher.extra_args(),
        );
        let host = RuntimeHost::detached();
        let mut runtime =
            shojiwm_lib::runtime_api::RuntimeBoot::new(Box::new(TypeScriptLauncher), &args)
                .launch(host.clone());
        assert_eq!(runtime.name(), "typescript");

        runtime.preload().expect("config should preload");
        runtime.enable().expect("config should enable");
        assert_eq!(key_binding_ids(&host), Some(vec!["first".to_string()]));

        let window = shojiwm_lib::ssd::WaylandWindowSnapshot {
            output_name: None,
            id: "w1".into(),
            title: "hello".into(),
            app_id: Some("test".into()),
            position: Default::default(),
            rect: Default::default(),
            is_focused: true,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: Default::default(),
        };
        let evaluation = runtime
            .evaluate_window(&window, 1)
            .expect("window should evaluate");
        assert!(matches!(
            evaluation.node.kind,
            shojiwm_lib::ssd::DecorationNodeKind::Label(ref label) if label.text == "hello"
        ));
        runtime.scheduler_tick(2.0).expect("scheduler should tick");

        write_config(true);
        assert_eq!(
            runtime.prepare_reload().expect("reload should prepare"),
            ReloadPreparation::Ready
        );
        runtime.reload().expect("config should hot reload");
        assert_eq!(
            key_binding_ids(&host),
            Some(vec!["first".to_string(), "second".to_string()])
        );

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    /// `defineRegion` / `bindHotCorner` reach the compositor as pointer config,
    /// and region events reach `onPointerRegion` and the hot corner handler.
    #[test]
    fn typescript_runtime_delivers_pointer_regions() {
        use shojiwm_lib::runtime_pointer_region::{
            PointerRegionAnchor, PointerRegionEventSnapshot, PointerRegionPhase,
        };

        let repository_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root should exist");
        let test_dir = std::env::temp_dir().join(format!(
            "shojiwm-pointer-region-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&test_dir).expect("test directory should be created");
        let config_path = test_dir.join("config.tsx");
        std::fs::write(
            &config_path,
            r#"
import { COMPOSITOR, Label } from "shoji_wm";
COMPOSITOR.pointer.defineRegion("strip", {
  anchor: "top",
  size: { width: 10, height: 2 },
  pressure: 5,
});
COMPOSITOR.pointer.bindHotCorner("corner", "bottom-right", (event) => {
  COMPOSITOR.key.bind(`corner-${event.phase}`, "Super+F1", () => {});
});
COMPOSITOR.event.onPointerRegion((event) => {
  if (event.id === "strip") {
    COMPOSITOR.key.bind(`strip-${event.phase}-${event.outputName}`, "Super+F2", () => {});
  }
});
COMPOSITOR.window.composition = (window) => <Label text={window.title} />;
"#,
        )
        .expect("test config should be written");
        let args = CommonArgs::parse(
            &[
                "--dev".to_string(),
                "--config".to_string(),
                config_path.display().to_string(),
                "--runtime-dir".to_string(),
                repository_root.display().to_string(),
            ],
            TypeScriptLauncher.extra_args(),
        );
        let host = RuntimeHost::detached();
        let mut runtime =
            shojiwm_lib::runtime_api::RuntimeBoot::new(Box::new(TypeScriptLauncher), &args)
                .launch(host.clone());
        runtime.preload().expect("config should preload");
        runtime.enable().expect("config should enable");

        let regions = std::iter::from_fn(|| host.pop())
            .filter_map(|message| match message {
                HostMessage::Pointer(update) => Some(update.regions),
                _ => None,
            })
            .last()
            .expect("pointer config should be published");
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].id, "strip");
        assert_eq!(regions[0].anchor, PointerRegionAnchor::Top);
        assert_eq!(regions[0].pressure, Some(5.0));
        assert!(!regions[0].outer_edges_only);
        assert_eq!(regions[1].id, "hot-corner:corner");
        assert_eq!(regions[1].anchor, PointerRegionAnchor::BottomRight);
        assert!(regions[1].outer_edges_only);
        assert!(regions[1].ignore_when_fullscreen);
        assert_eq!(regions[1].leave_margin, 8.0);

        let event = |id: &str, phase| PointerRegionEventSnapshot {
            id: id.into(),
            phase,
            output_name: "eDP-1".into(),
            position: shojiwm_lib::ssd::PointerMovePointSnapshot { x: 0.0, y: 0.0 },
            modifiers: shojiwm_lib::ssd::PointerModifierStateSnapshot {
                logo: false,
                alt: false,
                ctrl: false,
                shift: false,
            },
            timestamp: 0,
        };
        assert!(
            runtime
                .pointer_region(&event("strip", PointerRegionPhase::Pressure), 1)
                .expect("region event should be delivered")
                .invoked
        );
        assert!(
            runtime
                .pointer_region(&event("hot-corner:corner", PointerRegionPhase::Enter), 2)
                .expect("hot corner event should be delivered")
                .invoked
        );
        let ids = key_binding_ids(&host).expect("handlers should bind keys");
        assert!(ids.contains(&"strip-pressure-eDP-1".to_string()), "{ids:?}");
        assert!(ids.contains(&"corner-enter".to_string()), "{ids:?}");

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn unhandled_requests_fall_back_to_built_in_behavior() {
        let mut runtime =
            RuntimeHandle::new("none", Box::new(NullRuntime), RuntimeHost::detached());
        let window = shojiwm_lib::ssd::WaylandWindowSnapshot {
            output_name: None,
            id: "w1".into(),
            title: "hello".into(),
            app_id: None,
            position: Default::default(),
            rect: Default::default(),
            is_focused: false,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: Default::default(),
        };
        runtime
            .evaluate_window(&window, 0)
            .expect("static decoration should be used");
        assert!(runtime.scheduler_tick(0.0).unwrap().next_poll_in_ms.is_none());
        assert!(!runtime.invoke_key_binding("any", 0).unwrap().invoked);
        assert!(matches!(
            runtime.reload(),
            Err(RuntimeError::Unsupported(_))
        ));
    }
}
