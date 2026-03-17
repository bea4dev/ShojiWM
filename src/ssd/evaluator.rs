use std::{
    ffi::OsString,
    path::PathBuf,
    process::Command,
    time::Instant,
};
use tracing::debug;

use super::{DecorationBridgeError, DecorationLayoutError, DecorationNode, DecorationTree, decode_tree_json};
use super::window_model::WaylandWindowSnapshot;

/// Dynamic decoration evaluation boundary.
///
/// This trait represents the future hand-off point to the Node/TS runtime. For now it allows
/// ShojiWM to build and validate window-aware decoration trees while keeping the dynamic
/// evaluation contract explicit.
pub trait DecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationNode, DecorationEvaluationError>;
}

/// Temporary Rust-side evaluator that mirrors the intended TS-level behavior:
///
/// - focused windows get a yellow border
/// - unfocused windows get a white border
/// - title is reflected into a label node
///
/// This exists only to establish the per-window reevaluation flow for milestone 3.
#[derive(Debug, Default, Clone, Copy)]
pub struct StaticDecorationEvaluator;

impl DecorationEvaluator for StaticDecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationNode, DecorationEvaluationError> {
        let border_color = if window.is_focused {
            "#ffff00"
        } else {
            "#ffffff"
        };

        let json = format!(
            r##"{{
                "kind": "WindowBorder",
                "props": {{
                    "style": {{
                        "border": {{ "px": 1, "color": "{border_color}" }}
                    }}
                }},
                "children": [
                    {{
                        "kind": "Box",
                        "props": {{
                            "direction": "column"
                        }},
                        "children": [
                            {{
                                "kind": "Box",
                                "props": {{
                                    "direction": "row",
                                    "style": {{
                                        "height": 28,
                                        "paddingX": 8,
                                        "gap": 8
                                    }}
                                }},
                                "children": [
                                    {{
                                        "kind": "Label",
                                        "props": {{
                                            "text": {title:?}
                                        }},
                                        "children": []
                                    }},
                                    {{
                                        "kind": "Box",
                                        "props": {{
                                            "style": {{ "flexGrow": 1 }}
                                        }},
                                        "children": []
                                    }},
                                    {{
                                        "kind": "Button",
                                        "props": {{
                                            "onClick": "close"
                                        }},
                                        "children": []
                                    }}
                                ]
                            }},
                            {{
                                "kind": "Window",
                                "props": {{}},
                                "children": []
                            }}
                        ]
                    }}
                ]
            }}"##,
            title = window.title,
        );

        decode_tree_json(&json).map_err(Into::into)
    }
}

pub fn evaluate_dynamic_decoration<E: DecorationEvaluator>(
    evaluator: &E,
    window: &WaylandWindowSnapshot,
) -> Result<DecorationTree, DecorationEvaluationError> {
    evaluator.evaluate_window(window).map(DecorationTree::new)
}

#[derive(Debug, thiserror::Error)]
pub enum DecorationEvaluationError {
    #[error(transparent)]
    Bridge(#[from] DecorationBridgeError),
    #[error("failed to compute decoration layout: {0:?}")]
    Layout(DecorationLayoutError),
    #[error("failed to serialize window snapshot for evaluation: {0}")]
    SnapshotSerialization(String),
    #[error("failed to execute decoration runtime: {0}")]
    Io(#[from] std::io::Error),
    #[error("decoration runtime exited with status {status}: {stderr}")]
    RuntimeFailed { status: i32, stderr: String },
    #[error("decoration runtime returned invalid utf-8 output")]
    InvalidUtf8,
}

#[derive(Debug, Clone)]
pub struct NodeDecorationEvaluator {
    program: PathBuf,
    base_args: Vec<OsString>,
    script_path: PathBuf,
    config_path: PathBuf,
    working_dir: Option<PathBuf>,
}

impl NodeDecorationEvaluator {
    pub fn for_workspace(config_path: impl Into<PathBuf>) -> Self {
        let config_path = config_path.into();
        let local_tsx = PathBuf::from("node_modules/.bin/tsx");
        let program = if local_tsx.exists() {
            local_tsx
        } else {
            PathBuf::from("tsx")
        };

        Self {
            program,
            base_args: Vec::new(),
            script_path: PathBuf::from("tools/evaluate-decoration.ts"),
            config_path,
            working_dir: None,
        }
    }

    pub fn with_working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }

    pub fn with_command(
        program: impl Into<PathBuf>,
        base_args: Vec<OsString>,
        script_path: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            base_args,
            script_path: script_path.into(),
            config_path: config_path.into(),
            working_dir: None,
        }
    }
}

impl DecorationEvaluator for NodeDecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationNode, DecorationEvaluationError> {
        let started_at = Instant::now();
        let snapshot_json = serde_json::to_string(window)
            .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;

        let mut command = Command::new(&self.program);
        command.args(&self.base_args);
        command.arg(&self.script_path);
        command.arg(&self.config_path);
        command.arg(snapshot_json);
        if let Some(cwd) = &self.working_dir {
            command.current_dir(cwd);
        }

        let output = command.output()?;
        if !output.status.success() {
            return Err(DecorationEvaluationError::RuntimeFailed {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| DecorationEvaluationError::InvalidUtf8)?;
        debug!(
            window_id = window.id,
            title = window.title,
            elapsed_ms = started_at.elapsed().as_secs_f64() * 1000.0,
            "node decoration evaluation finished"
        );
        decode_tree_json(stdout.trim()).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{DecorationNodeKind, window_model::WaylandWindowSnapshot};

    fn make_window(is_focused: bool) -> WaylandWindowSnapshot {
        WaylandWindowSnapshot {
            id: "1".into(),
            title: "Kitty".into(),
            app_id: Some("kitty".into()),
            is_focused,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            icon: None,
            interaction: crate::ssd::DecorationInteractionSnapshot::default(),
        }
    }

    #[test]
    fn evaluator_reflects_title_into_tree() {
        let tree = evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(false))
            .expect("evaluation should succeed");

        let title_node = &tree.root.children[0].children[0].children[0];
        assert!(matches!(&title_node.kind, DecorationNodeKind::Label(label) if label.text == "Kitty"));
    }

    #[test]
    fn evaluator_changes_border_color_for_focused_window() {
        let focused = evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(true))
            .expect("focused evaluation should succeed");
        let unfocused = evaluate_dynamic_decoration(&StaticDecorationEvaluator, &make_window(false))
            .expect("unfocused evaluation should succeed");

        assert_ne!(focused.root.style.border, unfocused.root.style.border);
    }

    #[test]
    fn node_evaluator_can_decode_runtime_output() {
        use std::{fs, os::unix::fs::PermissionsExt, time::{SystemTime, UNIX_EPOCH}};

        let temp_dir = std::env::temp_dir().join(format!(
            "shoji_wm-node-eval-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&temp_dir).expect("create temp dir");

        let script_path = temp_dir.join("mock-evaluator.sh");
        fs::write(
            &script_path,
            r##"#!/bin/sh
cat <<'JSON'
{
  "kind": "WindowBorder",
  "props": {
    "style": {
      "border": { "px": 1, "color": "#ffffff" }
    }
  },
  "children": [
    { "kind": "Window", "props": {}, "children": [] }
  ]
}
JSON
"##,
        )
        .expect("write mock evaluator");
        let mut permissions = fs::metadata(&script_path)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("chmod");

        let evaluator = NodeDecorationEvaluator::with_command(
            &script_path,
            Vec::new(),
            "ignored-script.ts",
            "ignored-config.tsx",
        );

        let node = evaluator
            .evaluate_window(&make_window(false))
            .expect("node evaluator should decode output");

        assert!(matches!(node.kind, DecorationNodeKind::WindowBorder));
        fs::remove_dir_all(temp_dir).ok();
    }
}
