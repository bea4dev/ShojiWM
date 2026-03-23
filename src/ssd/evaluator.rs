use std::{
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{debug, warn};

use super::{
    BackgroundEffectConfig, DecorationBridgeError, DecorationLayoutError, DecorationNode,
    DecorationTree, WindowTransform, WireBackgroundEffectConfig, decode_tree_json,
};
use super::window_model::{WaylandWindowAction, WaylandWindowSnapshot};

/// Dynamic decoration evaluation boundary.
///
/// This trait represents the future hand-off point to the Node/TS runtime. For now it allows
/// ShojiWM to build and validate window-aware decoration trees while keeping the dynamic
/// evaluation contract explicit.
pub trait DecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError>;

    fn evaluate_cached_window(
        &self,
        _window_id: &str,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        Err(DecorationEvaluationError::RuntimeProtocol(
            "cached window evaluation unsupported".into(),
        ))
    }

    fn scheduler_tick(
        &self,
        _now_ms: u64,
    ) -> Result<DecorationSchedulerTick, DecorationEvaluationError> {
        Ok(DecorationSchedulerTick::default())
    }

    fn window_closed(&self, _window_id: &str) -> Result<(), DecorationEvaluationError> {
        Ok(())
    }

    fn invoke_handler(
        &self,
        _window_id: &str,
        _handler_id: &str,
        _now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        Ok(DecorationHandlerInvocation::default())
    }

    fn start_close(
        &self,
        _window_id: &str,
        _now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        Ok(DecorationHandlerInvocation::default())
    }
}

#[derive(Debug, Clone)]
pub struct DecorationEvaluationResult {
    pub node: DecorationNode,
    pub transform: WindowTransform,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationSchedulerTick {
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DecorationHandlerInvocation {
    pub invoked: bool,
    pub node: Option<DecorationNode>,
    pub transform: Option<WindowTransform>,
    pub dirty_window_ids: Vec<String>,
    pub actions: Vec<RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RuntimeWindowAction {
    #[serde(rename = "windowId")]
    pub window_id: String,
    pub action: WaylandWindowAction,
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
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
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

        Ok(DecorationEvaluationResult {
            node: decode_tree_json(&json)?,
            transform: WindowTransform::default(),
            next_poll_in_ms: None,
        })
    }
}

pub fn evaluate_dynamic_decoration<E: DecorationEvaluator>(
    evaluator: &E,
    window: &WaylandWindowSnapshot,
) -> Result<DecorationTree, DecorationEvaluationError> {
    evaluator
        .evaluate_window(window)
        .map(|result| DecorationTree::new(result.node))
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
    #[error("decoration runtime returned invalid json: {0}")]
    InvalidResponse(String),
    #[error("decoration runtime protocol error: {0}")]
    RuntimeProtocol(String),
}

pub struct NodeDecorationEvaluator {
    program: PathBuf,
    base_args: Vec<OsString>,
    script_path: PathBuf,
    config_path: PathBuf,
    working_dir: Option<PathBuf>,
    transport: RuntimeTransportKind,
    runtime: Arc<Mutex<Option<NodeDecorationRuntime>>>,
}

struct NodeDecorationRuntime {
    child: Child,
    connection: RuntimeConnection,
    next_request_id: u64,
    stderr_log: Arc<Mutex<String>>,
}

enum RuntimeConnection {
    Stdio {
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    Uds {
        writer: UnixStream,
        reader: BufReader<UnixStream>,
        socket_path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy)]
enum RuntimeTransportKind {
    Stdio,
    Uds,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RuntimeRequest<'a> {
    Evaluate {
        #[serde(rename = "requestId")]
        request_id: u64,
        snapshot: &'a WaylandWindowSnapshot,
    },
    SchedulerTick {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "nowMs")]
        now_ms: u64,
    },
    WindowClosed {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
    },
    InvokeHandler {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        #[serde(rename = "handlerId")]
        handler_id: &'a str,
        #[serde(rename = "nowMs")]
        now_ms: u64,
    },
    StartClose {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
        #[serde(rename = "nowMs")]
        now_ms: u64,
    },
    EvaluateCached {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: &'a str,
    },
    GetEffectConfig {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
}

#[derive(serde::Deserialize)]
struct RuntimeEvaluateResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    serialized: Option<serde_json::Value>,
    transform: Option<WindowTransform>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeSchedulerResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    dirty: Option<bool>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeClosedResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeInvokeHandlerResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    serialized: Option<serde_json::Value>,
    transform: Option<WindowTransform>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeStartCloseResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    invoked: Option<bool>,
    serialized: Option<serde_json::Value>,
    transform: Option<WindowTransform>,
    #[serde(rename = "dirtyWindowIds")]
    dirty_window_ids: Option<Vec<String>>,
    actions: Option<Vec<RuntimeWindowAction>>,
    #[serde(rename = "nextPollInMs")]
    next_poll_in_ms: Option<u64>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct RuntimeFailureResponse {
    #[serde(rename = "requestId")]
    request_id: i64,
    ok: bool,
    error: String,
}

#[derive(serde::Deserialize)]
struct RuntimeEffectConfigResponse {
    #[serde(rename = "requestId")]
    request_id: u64,
    kind: String,
    ok: bool,
    #[serde(rename = "backgroundEffect")]
    background_effect: Option<WireBackgroundEffectConfig>,
    error: Option<String>,
}

impl std::fmt::Debug for NodeDecorationEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeDecorationEvaluator")
            .field("program", &self.program)
            .field("base_args", &self.base_args)
            .field("script_path", &self.script_path)
            .field("config_path", &self.config_path)
            .field("working_dir", &self.working_dir)
            .finish()
    }
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
            script_path: PathBuf::from("tools/decoration-runtime.ts"),
            config_path,
            working_dir: None,
            transport: RuntimeTransportKind::Uds,
            runtime: Arc::new(Mutex::new(None)),
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
            transport: RuntimeTransportKind::Stdio,
            runtime: Arc::new(Mutex::new(None)),
        }
    }

    fn ensure_runtime<'a>(
        &'a self,
        runtime: &'a mut Option<NodeDecorationRuntime>,
    ) -> Result<&'a mut NodeDecorationRuntime, DecorationEvaluationError> {
        if runtime.is_none() {
            *runtime = Some(match self.transport {
                RuntimeTransportKind::Stdio => self.spawn_stdio_runtime()?,
                RuntimeTransportKind::Uds => self.spawn_uds_runtime()?,
            });
        }

        runtime
            .as_mut()
            .ok_or_else(|| DecorationEvaluationError::RuntimeProtocol("runtime unavailable".into()))
    }

    fn spawn_stdio_runtime(&self) -> Result<NodeDecorationRuntime, DecorationEvaluationError> {
        let mut command = Command::new(&self.program);
        command.args(&self.base_args);
        command.arg(&self.script_path);
        command.arg(&self.config_path);
        if let Some(cwd) = &self.working_dir {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stderr_log = spawn_stderr_drain(&mut child);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| DecorationEvaluationError::RuntimeProtocol("missing runtime stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| DecorationEvaluationError::RuntimeProtocol("missing runtime stdout".into()))?;

        Ok(NodeDecorationRuntime {
            child,
            connection: RuntimeConnection::Stdio {
                stdin,
                stdout: BufReader::new(stdout),
            },
            next_request_id: 1,
            stderr_log,
        })
    }

    fn spawn_uds_runtime(&self) -> Result<NodeDecorationRuntime, DecorationEvaluationError> {
        debug!("spawning node decoration runtime over uds");
        let socket_path = std::env::temp_dir().join(format!(
            "shojiwm-decoration-runtime-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        listener.set_nonblocking(true)?;

        let mut command = Command::new(&self.program);
        command.args(&self.base_args);
        command.arg(&self.script_path);
        command.arg(&self.config_path);
        command.arg(&socket_path);
        if let Some(cwd) = &self.working_dir {
            command.current_dir(cwd);
        }
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stderr_log = spawn_stderr_drain(&mut child);
        let accept_started_at = Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Some(status) = child.try_wait()? {
                        let status = status.code().unwrap_or(-1);
                        let stderr = stderr_log
                            .lock()
                            .map(|stderr| stderr.clone())
                            .unwrap_or_default();
                        let _ = std::fs::remove_file(&socket_path);
                        return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
                    }

                    if accept_started_at.elapsed() > Duration::from_secs(5) {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = std::fs::remove_file(&socket_path);
                        return Err(DecorationEvaluationError::RuntimeProtocol(
                            "timed out waiting for decoration runtime socket".into(),
                        ));
                    }

                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&socket_path);
                    return Err(DecorationEvaluationError::Io(error));
                }
            }
        };
        let writer = stream.try_clone()?;

        Ok(NodeDecorationRuntime {
            child,
            connection: RuntimeConnection::Uds {
                writer,
                reader: BufReader::new(stream),
                socket_path,
            },
            next_request_id: 1,
            stderr_log,
        })
    }

    pub fn background_effect_config(
        &self,
    ) -> Result<Option<BackgroundEffectConfig>, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::GetEffectConfig { request_id })
            .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeEffectConfigResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "getEffectConfig" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for getEffectConfig: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        response
            .background_effect
            .map(TryInto::try_into)
            .transpose()
            .map_err(DecorationEvaluationError::Bridge)
    }

}

impl Clone for NodeDecorationEvaluator {
    fn clone(&self) -> Self {
        Self {
            program: self.program.clone(),
            base_args: self.base_args.clone(),
            script_path: self.script_path.clone(),
            config_path: self.config_path.clone(),
            working_dir: self.working_dir.clone(),
            transport: self.transport,
            runtime: Arc::clone(&self.runtime),
        }
    }
}

impl NodeDecorationRuntime {
    fn write_request(&mut self, request: &str) -> Result<(), DecorationEvaluationError> {
        match &mut self.connection {
            RuntimeConnection::Stdio { stdin, .. } => {
                writeln!(stdin, "{request}")?;
                stdin.flush()?;
            }
            RuntimeConnection::Uds { writer, .. } => {
                writeln!(writer, "{request}")?;
                writer.flush()?;
            }
        }
        Ok(())
    }
}

impl Drop for NodeDecorationRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let RuntimeConnection::Uds { socket_path, .. } = &self.connection {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

fn spawn_stderr_drain(child: &mut Child) -> Arc<Mutex<String>> {
    let stderr_log = Arc::new(Mutex::new(String::new()));

    if let Some(stderr) = child.stderr.take() {
        let stderr_log_clone = Arc::clone(&stderr_log);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();

            loop {
                line.clear();
                let Ok(bytes) = reader.read_line(&mut line) else {
                    break;
                };
                if bytes == 0 {
                    break;
                }

                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    warn!(target: "shoji_wm::ssd::runtime", line = %trimmed, "decoration runtime stderr");
                }

                if let Ok(mut log) = stderr_log_clone.lock() {
                    log.push_str(trimmed);
                    log.push('\n');
                    if log.len() > 64 * 1024 {
                        let keep_from = log.len().saturating_sub(64 * 1024);
                        log.drain(..keep_from);
                    }
                }
            }
        });
    }

    stderr_log
}

impl DecorationEvaluator for NodeDecorationEvaluator {
    fn evaluate_window(
        &self,
        window: &WaylandWindowSnapshot,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::Evaluate {
            request_id,
            snapshot: window,
        })
            .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeEvaluateResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluate" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluate: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let Some(serialized) = response.serialized else {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                "missing serialized tree".into(),
            ));
        };
        let stdout = serde_json::to_string(&serialized)
            .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
        Ok(DecorationEvaluationResult {
            node: decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?,
            transform: response.transform.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
        })
    }

    fn evaluate_cached_window(
        &self,
        window_id: &str,
    ) -> Result<DecorationEvaluationResult, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;
        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::EvaluateCached {
            request_id,
            window_id,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeEvaluateResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "evaluateCached" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for evaluateCached: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let Some(serialized) = response.serialized else {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                "missing serialized tree".into(),
            ));
        };
        let stdout = serde_json::to_string(&serialized)
            .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
        Ok(DecorationEvaluationResult {
            node: decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?,
            transform: response.transform.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
        })
    }

    fn scheduler_tick(
        &self,
        now_ms: u64,
    ) -> Result<DecorationSchedulerTick, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationSchedulerTick::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::SchedulerTick { request_id, now_ms })
            .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeSchedulerResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "schedulerTick" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for schedulerTick: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(DecorationSchedulerTick {
            dirty: response.dirty.unwrap_or(false),
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
        })
    }

    fn window_closed(&self, window_id: &str) -> Result<(), DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::WindowClosed {
            request_id,
            window_id,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeClosedResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "windowClosed" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for windowClosed: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        Ok(())
    }

    fn invoke_handler(
        &self,
        window_id: &str,
        handler_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationHandlerInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::InvokeHandler {
            request_id,
            window_id,
            handler_id,
            now_ms,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let response: RuntimeInvokeHandlerResponse = match serde_json::from_str(line.trim()) {
            Ok(response) => response,
            Err(err) => {
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(err.to_string()));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "invokeHandler" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for invokeHandler: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = if let Some(serialized) = response.serialized {
            let stdout = serde_json::to_string(&serialized)
                .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
            Some(decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?)
        } else {
            None
        };

        Ok(DecorationHandlerInvocation {
            invoked: response.invoked.unwrap_or(false),
            node,
            transform: response.transform,
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
        })
    }

    fn start_close(
        &self,
        window_id: &str,
        now_ms: u64,
    ) -> Result<DecorationHandlerInvocation, DecorationEvaluationError> {
        let mut runtime_guard = self
            .runtime
            .lock()
            .map_err(|_| DecorationEvaluationError::RuntimeProtocol("runtime mutex poisoned".into()))?;

        let Some(_) = runtime_guard.as_ref() else {
            return Ok(DecorationHandlerInvocation::default());
        };

        let runtime = self.ensure_runtime(&mut runtime_guard)?;
        let request_id = runtime.next_request_id;
        runtime.next_request_id += 1;

        let request = serde_json::to_string(&RuntimeRequest::StartClose {
            request_id,
            window_id,
            now_ms,
        })
        .map_err(|err| DecorationEvaluationError::SnapshotSerialization(err.to_string()))?;
        runtime.write_request(&request)?;

        let mut line = String::new();
        let bytes = match &mut runtime.connection {
            RuntimeConnection::Stdio { stdout, .. } => stdout.read_line(&mut line)?,
            RuntimeConnection::Uds { reader, .. } => reader.read_line(&mut line)?,
        };
        if bytes == 0 {
            let status = runtime.child.try_wait()?.and_then(|status| status.code()).unwrap_or(-1);
            let stderr = runtime
                .stderr_log
                .lock()
                .map(|stderr| stderr.clone())
                .unwrap_or_default();
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeFailed { status, stderr });
        }

        let trimmed = line.trim();
        let response: RuntimeStartCloseResponse = match serde_json::from_str(trimmed) {
            Ok(response) => response,
            Err(err) => {
                if let Ok(failure) = serde_json::from_str::<RuntimeFailureResponse>(trimmed) {
                    *runtime_guard = None;
                    return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                        "runtime returned failure for startClose (requestId={} ok={}): {}",
                        failure.request_id, failure.ok, failure.error
                    )));
                }
                *runtime_guard = None;
                return Err(DecorationEvaluationError::InvalidResponse(format!(
                    "{err}; payload={trimmed}"
                )));
            }
        };
        if response.request_id != request_id {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response id: expected {request_id}, got {}",
                response.request_id
            )));
        }
        if response.kind != "startClose" {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(format!(
                "mismatched response kind for startClose: {}",
                response.kind
            )));
        }
        if !response.ok {
            *runtime_guard = None;
            return Err(DecorationEvaluationError::RuntimeProtocol(
                response
                    .error
                    .unwrap_or_else(|| "runtime returned failure".into()),
            ));
        }

        let node = if let Some(serialized) = response.serialized {
            let stdout = serde_json::to_string(&serialized)
                .map_err(|err| DecorationEvaluationError::InvalidResponse(err.to_string()))?;
            Some(decode_tree_json(stdout.trim()).map_err(DecorationEvaluationError::Bridge)?)
        } else {
            None
        };

        Ok(DecorationHandlerInvocation {
            invoked: response.invoked.unwrap_or(false),
            node,
            transform: response.transform,
            dirty_window_ids: response.dirty_window_ids.unwrap_or_default(),
            actions: response.actions.unwrap_or_default(),
            next_poll_in_ms: response.next_poll_in_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssd::{
        DecorationNodeKind,
        window_model::{WaylandWindowSnapshot, WindowPositionSnapshot},
    };

    fn make_window(is_focused: bool) -> WaylandWindowSnapshot {
        WaylandWindowSnapshot {
            id: "1".into(),
            title: "Kitty".into(),
            app_id: Some("kitty".into()),
            position: WindowPositionSnapshot {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            },
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
read _
cat <<'JSON'
{"requestId":1,"ok":true,"serialized":{"kind":"WindowBorder","props":{"style":{"border":{"px":1,"color":"#ffffff"}}},"children":[{"kind":"Window","props":{},"children":[]}]}}
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

        assert!(matches!(node.node.kind, DecorationNodeKind::WindowBorder));
        fs::remove_dir_all(temp_dir).ok();
    }
}
