use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::CStr,
    os::unix::fs::FileTypeExt,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

thread_local! {
    static RUNTIME_CURRENT_DIR: RefCell<PathBuf> = RefCell::new(
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
    );
}

use rustyscript::{
    Module, Runtime, RuntimeOptions,
    deno_core::{
        GarbageCollected, ModuleSource, ModuleSourceCode, ModuleSpecifier,
        error::ModuleLoaderError, extension, op2, v8,
    },
    json_args,
    module_loader::ImportProvider,
};
use serde::{Deserialize, Serialize};

use super::{
    DecorationNode,
    bridge::WireDecorationNode,
    window_model::{
        ManagedWindowRectSnapshot, ManagedWindowState, TransformOrigin, WaylandOutputSnapshot,
        WaylandWindowSnapshot, WindowTransform,
    },
};
use crate::runtime_input::RuntimeInputDeviceSnapshot;

/// Composition requests cross the CppGC bridge as V8 values instead of JSON
/// frames. Ownership moves into the request envelope, so large snapshots are
/// converted exactly once on the runtime thread.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NativeCompositionRequest {
    Evaluate {
        #[serde(rename = "requestId")]
        request_id: u64,
        snapshot: WaylandWindowSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    EvaluatePreview {
        #[serde(rename = "requestId")]
        request_id: u64,
        snapshot: WaylandWindowSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    EvaluateCached {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: String,
        snapshot: Option<WaylandWindowSnapshot>,
        #[serde(rename = "forceFullReevaluation")]
        force_full_reevaluation: bool,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSchedulerRequest {
    pub request_id: u64,
    pub kind: &'static str,
    pub now_ms: u64,
    pub display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
    pub input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
}

enum BridgeRequest {
    Json(String),
    Composition(NativeCompositionRequest),
    Scheduler(NativeSchedulerRequest),
    CachedFast {
        request_id: u64,
        window_id: String,
        force_full_reevaluation: bool,
        now_ms: u64,
    },
    SchedulerFast {
        request_id: u64,
        now_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub enum NativeCompositionPatch {
    /// A structural or otherwise generic node change. This remains the
    /// compatibility fallback and performs one serde_v8 conversion.
    ReplaceNode {
        node_id: String,
        node: DecorationNode,
    },
    /// The steady animation fast path. Mutate one uniform in the compositor's
    /// persistent tree without decoding or rebuilding the shader pipeline.
    ShaderUniform {
        node_id: String,
        stage_index: usize,
        name: String,
        value: super::ShaderUniformValue,
    },
}

pub const SHADER_INPUT_STAGE_INDEX: usize = u32::MAX as usize;

impl NativeCompositionPatch {
    pub fn node_id(&self) -> &str {
        match self {
            Self::ReplaceNode { node_id, .. } | Self::ShaderUniform { node_id, .. } => node_id,
        }
    }

    pub fn replacement_node(&self) -> Option<&DecorationNode> {
        match self {
            Self::ReplaceNode { node, .. } => Some(node),
            Self::ShaderUniform { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum NativeCompositionUpdate {
    Full {
        window_id: String,
        node: DecorationNode,
    },
    Patches {
        window_id: String,
        patches: Vec<NativeCompositionPatch>,
    },
}

#[derive(Debug)]
pub struct NativeSchedulerResponse {
    pub request_id: u64,
    pub dirty: bool,
    pub runtime_dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: HashMap<String, Vec<String>>,
    pub dirty_layer_ids: Vec<String>,
    pub dirty_layer_node_ids: HashMap<String, Vec<String>>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug)]
pub struct NativeCachedResponse {
    pub request_id: u64,
    pub transform: WindowTransform,
    pub managed_window: ManagedWindowState,
    pub dirty_node_ids: Vec<String>,
    pub managed_window_only: bool,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug)]
pub enum EmbeddedRuntimeResponse {
    Json(Vec<u8>),
    Scheduler(NativeSchedulerResponse),
    Cached(NativeCachedResponse),
}

impl NativeCompositionUpdate {
    pub fn window_id(&self) -> &str {
        match self {
            Self::Full { window_id, .. } | Self::Patches { window_id, .. } => window_id,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WireNativeCompositionUpdate {
    Full {
        #[serde(rename = "windowId")]
        window_id: String,
        tree: WireDecorationNode,
    },
    Patches {
        #[serde(rename = "windowId")]
        window_id: String,
        patches: Vec<WireNativeCompositionPatch>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNativeCompositionPatch {
    node_id: String,
    node: WireDecorationNode,
}

#[op2]
#[serde]
fn op_shoji_environment() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[op2]
#[string]
fn op_shoji_current_dir() -> String {
    RUNTIME_CURRENT_DIR.with(|path| path.borrow().to_string_lossy().into_owned())
}

#[op2(fast)]
fn op_shoji_path_exists(#[string] path: &str) -> bool {
    std::path::Path::new(path).exists()
}

#[op2(fast)]
fn op_shoji_remove_unix_socket(#[string] path: &str) -> Result<bool, std::io::Error> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to remove a non-socket IPC path",
        ));
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[op2(fast)]
fn op_shoji_process_id() -> u32 {
    std::process::id()
}

#[op2(fast)]
fn op_shoji_wake_compositor() {
    #[cfg(not(test))]
    // SAFETY: SIGUSR1 is blocked process-wide before compositor threads start
    // and consumed by calloop's signalfd source.
    unsafe {
        libc::kill(std::process::id() as libc::pid_t, libc::SIGUSR1);
    }
}

struct BridgeRegistration {
    requests: tokio::sync::mpsc::UnboundedReceiver<BridgeRequest>,
    responses: Sender<EmbeddedRuntimeResponse>,
    composition_updates: Arc<Mutex<HashMap<u64, NativeCompositionUpdate>>>,
}

static NEXT_BRIDGE_ID: AtomicU32 = AtomicU32::new(1);
static BRIDGE_REGISTRATIONS: OnceLock<Mutex<HashMap<u32, BridgeRegistration>>> = OnceLock::new();

fn bridge_registrations() -> &'static Mutex<HashMap<u32, BridgeRegistration>> {
    BRIDGE_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[repr(C)]
struct ShojiRuntimeBridge {
    requests: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<BridgeRequest>>,
    responses: Sender<EmbeddedRuntimeResponse>,
    composition_updates: Arc<Mutex<HashMap<u64, NativeCompositionUpdate>>>,
    composition_uniform_slots: Mutex<HashMap<u32, NativeShaderUniformSlot>>,
    pending_native_response: Mutex<Option<EmbeddedRuntimeResponse>>,
}

#[derive(Debug, Clone)]
struct NativeShaderUniformSlot {
    window_id: String,
    node_id: String,
    stage_index: usize,
    name: String,
}

#[repr(C)]
struct RuntimeRequestEnvelope {
    request: Mutex<Option<BridgeRequest>>,
}

unsafe impl GarbageCollected for ShojiRuntimeBridge {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static CStr {
        c"ShojiRuntimeBridge"
    }
}

unsafe impl GarbageCollected for RuntimeRequestEnvelope {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static CStr {
        c"RuntimeRequestEnvelope"
    }
}

#[op2]
impl RuntimeRequestEnvelope {
    #[string]
    fn json(&self) -> Option<String> {
        let mut request = self.request.lock().ok()?;
        if !matches!(request.as_ref(), Some(BridgeRequest::Json(_))) {
            return None;
        }
        match request.take() {
            Some(BridgeRequest::Json(request)) => Some(request),
            _ => None,
        }
    }

    #[serde]
    fn composition(&self) -> Option<NativeCompositionRequest> {
        let mut request = self.request.lock().ok()?;
        if !matches!(request.as_ref(), Some(BridgeRequest::Composition(_))) {
            return None;
        }
        match request.take() {
            Some(BridgeRequest::Composition(request)) => Some(request),
            _ => None,
        }
    }

    #[serde]
    fn scheduler(&self) -> Option<NativeSchedulerRequest> {
        let mut request = self.request.lock().ok()?;
        if !matches!(request.as_ref(), Some(BridgeRequest::Scheduler(_))) {
            return None;
        }
        match request.take() {
            Some(BridgeRequest::Scheduler(request)) => Some(request),
            _ => None,
        }
    }

    #[fast]
    fn fast_kind(&self) -> u32 {
        let Ok(request) = self.request.lock() else {
            return 0;
        };
        match request.as_ref() {
            Some(BridgeRequest::CachedFast { .. }) => 1,
            Some(BridgeRequest::SchedulerFast { .. }) => 2,
            _ => 0,
        }
    }

    #[fast]
    fn fast_request_id(&self) -> f64 {
        let Ok(request) = self.request.lock() else {
            return -1.0;
        };
        match request.as_ref() {
            Some(BridgeRequest::CachedFast { request_id, .. })
            | Some(BridgeRequest::SchedulerFast { request_id, .. }) => *request_id as f64,
            _ => -1.0,
        }
    }

    #[string]
    fn fast_window_id(&self) -> String {
        let Ok(request) = self.request.lock() else {
            return String::new();
        };
        match request.as_ref() {
            Some(BridgeRequest::CachedFast { window_id, .. }) => window_id.clone(),
            _ => String::new(),
        }
    }

    #[fast]
    fn fast_force_full_reevaluation(&self) -> bool {
        let Ok(request) = self.request.lock() else {
            return false;
        };
        match request.as_ref() {
            Some(BridgeRequest::CachedFast {
                force_full_reevaluation,
                ..
            }) => *force_full_reevaluation,
            _ => false,
        }
    }

    #[fast]
    fn fast_now_ms(&self) -> f64 {
        let Ok(request) = self.request.lock() else {
            return -1.0;
        };
        match request.as_ref() {
            Some(BridgeRequest::CachedFast { now_ms, .. })
            | Some(BridgeRequest::SchedulerFast { now_ms, .. }) => *now_ms as f64,
            _ => -1.0,
        }
    }

    #[fast]
    fn finish_fast(&self) -> Result<(), std::io::Error> {
        let mut request = self
            .request
            .lock()
            .map_err(|_| std::io::Error::other("runtime request envelope is poisoned"))?;
        match request.take() {
            Some(BridgeRequest::CachedFast { .. }) | Some(BridgeRequest::SchedulerFast { .. }) => {
                Ok(())
            }
            other => {
                *request = other;
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "runtime request envelope does not contain a fast request",
                ))
            }
        }
    }
}

#[op2]
impl ShojiRuntimeBridge {
    #[constructor]
    #[cppgc]
    fn constructor(bridge_id: u32) -> Result<ShojiRuntimeBridge, std::io::Error> {
        let registration = bridge_registrations()
            .lock()
            .map_err(|_| std::io::Error::other("runtime bridge registry is poisoned"))?
            .remove(&bridge_id)
            .ok_or_else(|| std::io::Error::other("runtime bridge registration is missing"))?;
        Ok(ShojiRuntimeBridge {
            requests: tokio::sync::Mutex::new(registration.requests),
            responses: registration.responses,
            composition_updates: registration.composition_updates,
            composition_uniform_slots: Mutex::new(HashMap::new()),
            pending_native_response: Mutex::new(None),
        })
    }

    #[async_method]
    #[cppgc]
    async fn read_request(&self) -> Option<RuntimeRequestEnvelope> {
        self.requests
            .lock()
            .await
            .recv()
            .await
            .map(|request| RuntimeRequestEnvelope {
                request: Mutex::new(Some(request)),
            })
    }

    #[fast]
    fn write_response(&self, #[string] response: String) -> Result<(), std::io::Error> {
        self.responses
            .send(EmbeddedRuntimeResponse::Json(response.into_bytes()))
            .map_err(|_| std::io::Error::other("runtime response receiver was dropped"))
    }

    #[fast]
    fn begin_scheduler_response(
        &self,
        request_id: f64,
        dirty: bool,
        runtime_dirty: bool,
        next_poll_in_ms: f64,
    ) -> Result<(), std::io::Error> {
        let response = EmbeddedRuntimeResponse::Scheduler(NativeSchedulerResponse {
            request_id: checked_request_id(request_id)?,
            dirty,
            runtime_dirty,
            dirty_window_ids: Vec::new(),
            dirty_managed_window_ids: Vec::new(),
            dirty_window_node_ids: HashMap::new(),
            dirty_layer_ids: Vec::new(),
            dirty_layer_node_ids: HashMap::new(),
            next_poll_in_ms: checked_optional_millis(next_poll_in_ms)?,
        });
        set_pending_native_response(&self.pending_native_response, response)
    }

    #[fast]
    fn add_scheduler_dirty_window(
        &self,
        #[string] window_id: &str,
        managed_only: bool,
    ) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Scheduler(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "scheduler response builder was not started",
            ));
        };
        response.dirty_window_ids.push(window_id.to_owned());
        if managed_only {
            response.dirty_managed_window_ids.push(window_id.to_owned());
        }
        Ok(())
    }

    #[fast]
    fn add_scheduler_dirty_window_node(
        &self,
        #[string] window_id: &str,
        #[string] node_id: &str,
    ) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Scheduler(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "scheduler response builder was not started",
            ));
        };
        response
            .dirty_window_node_ids
            .entry(window_id.to_owned())
            .or_default()
            .push(node_id.to_owned());
        Ok(())
    }

    #[fast]
    fn add_scheduler_dirty_layer(&self, #[string] layer_id: &str) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Scheduler(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "scheduler response builder was not started",
            ));
        };
        response.dirty_layer_ids.push(layer_id.to_owned());
        Ok(())
    }

    #[fast]
    fn add_scheduler_dirty_layer_node(
        &self,
        #[string] layer_id: &str,
        #[string] node_id: &str,
    ) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Scheduler(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "scheduler response builder was not started",
            ));
        };
        response
            .dirty_layer_node_ids
            .entry(layer_id.to_owned())
            .or_default()
            .push(node_id.to_owned());
        Ok(())
    }

    #[fast]
    fn begin_cached_response(&self, #[buffer] payload: &[u8]) -> Result<(), std::io::Error> {
        const FIELD_COUNT: usize = 15;
        if payload.len() != FIELD_COUNT * std::mem::size_of::<f64>() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cached response payload has the wrong size",
            ));
        }
        let mut fields = [0.0; FIELD_COUNT];
        for (index, field) in fields.iter_mut().enumerate() {
            let offset = index * 8;
            *field = f64::from_le_bytes(payload[offset..offset + 8].try_into().unwrap());
        }
        let flags = checked_flags(fields[2])?;
        let rect = (flags & (1 << 7) != 0).then_some(ManagedWindowRectSnapshot {
            x: fields[10],
            y: fields[11],
            width: fields[12],
            height: fields[13],
        });
        let allow_tearing = match (flags >> 8) & 0b11 {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cached response has invalid allowTearing flags",
                ));
            }
        };
        let response = EmbeddedRuntimeResponse::Cached(NativeCachedResponse {
            request_id: checked_request_id(fields[0])?,
            transform: WindowTransform {
                origin: TransformOrigin {
                    x: fields[3],
                    y: fields[4],
                },
                translate_x: fields[5],
                translate_y: fields[6],
                scale_x: fields[7],
                scale_y: fields[8],
                opacity: fields[9] as f32,
            },
            managed_window: ManagedWindowState {
                managed: flags & (1 << 1) != 0,
                rect,
                workspace: None,
                visible_outputs: (flags & (1 << 10) != 0).then(Vec::new),
                visible: flags & (1 << 2) != 0,
                idle: flags & (1 << 3) != 0,
                interactive: flags & (1 << 4) != 0,
                force_rect_size: flags & (1 << 5) != 0,
                tiled: flags & (1 << 6) != 0,
                allow_tearing,
                z_index: (flags & (1 << 11) != 0).then_some(fields[14] as i32),
                transform: WindowTransform {
                    origin: TransformOrigin {
                        x: fields[3],
                        y: fields[4],
                    },
                    translate_x: fields[5],
                    translate_y: fields[6],
                    scale_x: fields[7],
                    scale_y: fields[8],
                    opacity: fields[9] as f32,
                },
            },
            dirty_node_ids: Vec::new(),
            managed_window_only: flags & 1 != 0,
            next_poll_in_ms: checked_optional_millis(fields[1])?,
        });
        set_pending_native_response(&self.pending_native_response, response)
    }

    #[fast]
    fn add_cached_dirty_node(&self, #[string] node_id: &str) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Cached(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "cached response builder was not started",
            ));
        };
        response.dirty_node_ids.push(node_id.to_owned());
        Ok(())
    }

    #[fast]
    fn add_cached_visible_output(&self, #[string] output_name: &str) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Cached(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "cached response builder was not started",
            ));
        };
        let Some(outputs) = response.managed_window.visible_outputs.as_mut() else {
            return Err(std::io::Error::other(
                "cached response has no visibleOutputs field",
            ));
        };
        outputs.push(output_name.to_owned());
        Ok(())
    }

    #[fast]
    fn set_cached_workspace_string(&self, #[string] workspace: &str) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Cached(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "cached response builder was not started",
            ));
        };
        response.managed_window.workspace = Some(serde_json::Value::String(workspace.to_owned()));
        Ok(())
    }

    #[fast]
    fn set_cached_workspace_number(&self, workspace: f64) -> Result<(), std::io::Error> {
        let mut pending = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
        let Some(EmbeddedRuntimeResponse::Cached(response)) = pending.as_mut() else {
            return Err(std::io::Error::other(
                "cached response builder was not started",
            ));
        };
        response.managed_window.workspace =
            serde_json::Number::from_f64(workspace).map(serde_json::Value::Number);
        Ok(())
    }

    #[fast]
    fn finish_native_response(&self) -> Result<(), std::io::Error> {
        let response = self
            .pending_native_response
            .lock()
            .map_err(|_| std::io::Error::other("native response builder is poisoned"))?
            .take()
            .ok_or_else(|| std::io::Error::other("native response builder was not started"))?;
        self.responses
            .send(response)
            .map_err(|_| std::io::Error::other("runtime response receiver was dropped"))
    }

    fn write_composition_update(
        &self,
        request_id: f64,
        #[serde] update: WireNativeCompositionUpdate,
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native composition decode");
        if !request_id.is_finite()
            || request_id < 0.0
            || request_id.fract() != 0.0
            || request_id > u64::MAX as f64
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "composition request id must be a non-negative integer",
            ));
        }
        let update = match update {
            WireNativeCompositionUpdate::Full { window_id, tree } => {
                NativeCompositionUpdate::Full {
                    window_id,
                    node: tree
                        .try_into()
                        .map_err(|error: super::DecorationBridgeError| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                        })?,
                }
            }
            WireNativeCompositionUpdate::Patches { window_id, patches } => {
                let patches = patches
                    .into_iter()
                    .map(|patch| {
                        let node: DecorationNode = patch.node.try_into().map_err(
                            |error: super::DecorationBridgeError| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    error.to_string(),
                                )
                            },
                        )?;
                        if node.stable_id.as_deref() != Some(patch.node_id.as_str()) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!(
                                    "composition patch id mismatch: envelope={}, node={:?}",
                                    patch.node_id, node.stable_id
                                ),
                            ));
                        }
                        Ok(NativeCompositionPatch::ReplaceNode {
                            node_id: patch.node_id,
                            node,
                        })
                    })
                    .collect::<Result<Vec<_>, std::io::Error>>()?;
                NativeCompositionUpdate::Patches { window_id, patches }
            }
        };
        let mut updates = self
            .composition_updates
            .lock()
            .map_err(|_| std::io::Error::other("composition update store is poisoned"))?;
        match updates.entry(request_id as u64) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(update);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "composition update was already written for this request",
                ));
            }
        }
        Ok(())
    }

    #[fast]
    fn begin_composition_patches(
        &self,
        request_id: f64,
        #[string] window_id: &str,
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let mut updates = self
            .composition_updates
            .lock()
            .map_err(|_| std::io::Error::other("composition update store is poisoned"))?;
        match updates.entry(request_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(NativeCompositionUpdate::Patches {
                    window_id: window_id.to_owned(),
                    patches: Vec::new(),
                });
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "composition update was already written for this request",
            )),
        }
    }

    #[fast]
    fn begin_composition_shader_uniform_slot_patches(
        &self,
        request_id: f64,
        slot_id: u32,
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let window_id = self
            .composition_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("composition uniform slot store is poisoned"))?
            .get(&slot_id)
            .map(|slot| slot.window_id.clone())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "composition shader uniform slot is not registered",
                )
            })?;
        let mut updates = self
            .composition_updates
            .lock()
            .map_err(|_| std::io::Error::other("composition update store is poisoned"))?;
        match updates.entry(request_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(NativeCompositionUpdate::Patches {
                    window_id,
                    patches: Vec::new(),
                });
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "composition update was already written for this request",
            )),
        }
    }

    #[fast]
    fn write_composition_shader_uniform_patch(
        &self,
        request_id: f64,
        #[string] node_id: &str,
        stage_index: u32,
        #[string] name: &str,
        value_len: u32,
        x: f64,
        y: f64,
        z: f64,
        w: f64,
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let values = [x, y, z, w].map(|value| value as f32);
        if values
            .iter()
            .take(value_len as usize)
            .any(|value| !value.is_finite())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shader uniform values must be finite",
            ));
        }
        let value = match value_len {
            1 => super::ShaderUniformValue::Float(values[0]),
            2 => super::ShaderUniformValue::Vec2([values[0], values[1]]),
            3 => super::ShaderUniformValue::Vec3([values[0], values[1], values[2]]),
            4 => super::ShaderUniformValue::Vec4(values),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "shader uniform length must be between 1 and 4",
                ));
            }
        };
        let mut updates = self
            .composition_updates
            .lock()
            .map_err(|_| std::io::Error::other("composition update store is poisoned"))?;
        let Some(NativeCompositionUpdate::Patches { patches, .. }) = updates.get_mut(&request_id)
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "composition patch batch was not started",
            ));
        };
        patches.push(NativeCompositionPatch::ShaderUniform {
            node_id: node_id.to_owned(),
            stage_index: stage_index as usize,
            name: name.to_owned(),
            value,
        });
        Ok(())
    }

    #[fast]
    fn register_composition_shader_uniform_slot(
        &self,
        slot_id: u32,
        #[string] window_id: &str,
        #[string] node_id: &str,
        stage_index: u32,
        #[string] name: &str,
    ) -> Result<(), std::io::Error> {
        self.composition_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("composition uniform slot store is poisoned"))?
            .insert(
                slot_id,
                NativeShaderUniformSlot {
                    window_id: window_id.to_owned(),
                    node_id: node_id.to_owned(),
                    stage_index: stage_index as usize,
                    name: name.to_owned(),
                },
            );
        Ok(())
    }

    #[fast]
    fn clear_composition_shader_uniform_slots(
        &self,
        #[string] window_id: &str,
    ) -> Result<(), std::io::Error> {
        self.composition_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("composition uniform slot store is poisoned"))?
            .retain(|_, slot| slot.window_id != window_id);
        Ok(())
    }

    #[fast]
    fn write_composition_shader_uniform_slot_patch(
        &self,
        request_id: f64,
        slot_id: u32,
        value_len: u32,
        x: f64,
        y: f64,
        z: f64,
        w: f64,
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native uniform slot patch");
        let request_id = checked_request_id(request_id)?;
        let values = [x, y, z, w].map(|value| value as f32);
        if values
            .iter()
            .take(value_len as usize)
            .any(|value| !value.is_finite())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "shader uniform values must be finite",
            ));
        }
        let value = match value_len {
            1 => super::ShaderUniformValue::Float(values[0]),
            2 => super::ShaderUniformValue::Vec2([values[0], values[1]]),
            3 => super::ShaderUniformValue::Vec3([values[0], values[1], values[2]]),
            4 => super::ShaderUniformValue::Vec4(values),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "shader uniform length must be between 1 and 4",
                ));
            }
        };
        let slot = self
            .composition_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("composition uniform slot store is poisoned"))?
            .get(&slot_id)
            .cloned()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "composition shader uniform slot is not registered",
                )
            })?;
        let mut updates = self
            .composition_updates
            .lock()
            .map_err(|_| std::io::Error::other("composition update store is poisoned"))?;
        let Some(NativeCompositionUpdate::Patches { window_id, patches }) =
            updates.get_mut(&request_id)
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "composition patch batch was not started",
            ));
        };
        if *window_id != slot.window_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "composition shader uniform slot belongs to a different window",
            ));
        }
        patches.push(NativeCompositionPatch::ShaderUniform {
            node_id: slot.node_id,
            stage_index: slot.stage_index,
            name: slot.name,
            value,
        });
        Ok(())
    }

    #[fast]
    fn log(&self, #[string] level: &str, #[string] message: &str) {
        match level {
            "debug" => tracing::debug!(target: "shoji_wm::ssd::runtime", "{message}"),
            "warn" => tracing::warn!(target: "shoji_wm::ssd::runtime", "{message}"),
            "error" => tracing::error!(target: "shoji_wm::ssd::runtime", "{message}"),
            _ => tracing::info!(target: "shoji_wm::ssd::runtime", "{message}"),
        }
    }
}

fn checked_request_id(request_id: f64) -> Result<u64, std::io::Error> {
    if !request_id.is_finite()
        || request_id < 0.0
        || request_id.fract() != 0.0
        || request_id > u64::MAX as f64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "runtime request id must be a non-negative integer",
        ));
    }
    Ok(request_id as u64)
}

fn checked_optional_millis(value: f64) -> Result<Option<u64>, std::io::Error> {
    if value == -1.0 {
        return Ok(None);
    }
    checked_request_id(value).map(Some)
}

fn checked_flags(value: f64) -> Result<u64, std::io::Error> {
    checked_request_id(value)
}

fn set_pending_native_response(
    slot: &Mutex<Option<EmbeddedRuntimeResponse>>,
    response: EmbeddedRuntimeResponse,
) -> Result<(), std::io::Error> {
    let mut slot = slot
        .lock()
        .map_err(|_| std::io::Error::other("native response builder is poisoned"))?;
    if slot.is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a native response is already being built",
        ));
    }
    *slot = Some(response);
    Ok(())
}

extension!(
    shoji_runtime_bridge,
    ops = [
        op_shoji_environment,
        op_shoji_current_dir,
        op_shoji_path_exists,
        op_shoji_remove_unix_socket,
        op_shoji_process_id,
        op_shoji_wake_compositor,
    ],
    objects = [ShojiRuntimeBridge, RuntimeRequestEnvelope],
    esm_entry_point = "ext:shoji_runtime_bridge/native.js",
    esm = [
        dir "src/ssd",
        "ext:shoji_runtime_bridge/native.js" = "embedded_runtime.js",
    ],
);

struct ShojiImportProvider {
    package_modules: Vec<(&'static str, String)>,
}

impl ShojiImportProvider {
    fn new(runtime_root: &std::path::Path) -> Result<Self, String> {
        let source_root = runtime_root.join("packages/shoji_wm/src");
        let module_url = |path: &str| {
            ModuleSpecifier::from_file_path(source_root.join(path))
                .map(|specifier| specifier.to_string())
                .map_err(|_| format!("failed to create module URL for {path}"))
        };
        Ok(Self {
            // Match longer public subpaths before the package root.
            package_modules: vec![
                (
                    "shoji_wm/default-composition",
                    module_url("default-composition.tsx")?,
                ),
                (
                    "shoji_wm/jsx-dev-runtime",
                    module_url("jsx-dev-runtime.ts")?,
                ),
                ("shoji_wm/jsx-runtime", module_url("jsx-runtime.ts")?),
                ("shoji_wm/types", module_url("types.ts")?),
                ("shoji_wm/ipc", module_url("ipc.ts")?),
                ("shoji_wm", module_url("index.ts")?),
            ],
        })
    }

    fn rewrite_package_specifiers(&self, mut source: String) -> String {
        for (specifier, replacement) in &self.package_modules {
            source = source.replace(&format!("\"{specifier}\""), &format!("\"{replacement}\""));
            source = source.replace(&format!("'{specifier}'"), &format!("'{replacement}'"));
        }
        source
    }
}

impl ImportProvider for ShojiImportProvider {
    fn resolve(
        &mut self,
        specifier: &ModuleSpecifier,
        _referrer: &str,
        _kind: deno_core::ResolutionKind,
    ) -> Option<Result<ModuleSpecifier, ModuleLoaderError>> {
        if specifier.scheme() != "file" {
            return None;
        }
        let path = specifier.to_file_path().ok()?;
        if path.exists() {
            return None;
        }
        let candidates = [
            path.with_extension("ts"),
            path.with_extension("tsx"),
            path.join("index.ts"),
            path.join("index.tsx"),
        ];
        let resolved = candidates
            .into_iter()
            .find(|candidate| candidate.is_file())?;
        Some(ModuleSpecifier::from_file_path(&resolved).map_err(|_| {
            ModuleLoaderError::generic(format!(
                "failed to resolve TypeScript module {}",
                resolved.display()
            ))
        }))
    }

    fn import(
        &mut self,
        specifier: &ModuleSpecifier,
        _referrer: Option<&ModuleSpecifier>,
        _is_dynamic_import: bool,
    ) -> Option<Result<String, ModuleLoaderError>> {
        if specifier.scheme() != "file" {
            return None;
        }
        let path = match specifier.to_file_path() {
            Ok(path) => path,
            Err(()) => return Some(Err(ModuleLoaderError::not_supported())),
        };
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                return Some(Err(ModuleLoaderError::generic(format!(
                    "failed to load {}: {error}",
                    path.display()
                ))));
            }
        };
        let is_jsx = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("tsx" | "jsx")
        );
        if is_jsx && !source.contains("@jsxImportSource") {
            Some(Ok(format!("/** @jsxImportSource shoji_wm */\n{source}")))
        } else {
            Some(Ok(source))
        }
    }

    fn post_process(
        &mut self,
        _specifier: &ModuleSpecifier,
        mut source: ModuleSource,
    ) -> Result<ModuleSource, ModuleLoaderError> {
        if let ModuleSourceCode::String(code) = source.code {
            source.code =
                ModuleSourceCode::String(self.rewrite_package_specifiers(code.to_string()).into());
        }
        Ok(source)
    }
}

pub struct EmbeddedRuntime {
    requests: Option<tokio::sync::mpsc::UnboundedSender<BridgeRequest>>,
    responses: Receiver<EmbeddedRuntimeResponse>,
    composition_updates: Arc<Mutex<HashMap<u64, NativeCompositionUpdate>>>,
    worker: Option<JoinHandle<()>>,
    worker_error: Arc<Mutex<Option<String>>>,
}

pub struct EmbeddedRuntimeExitStatus {
    code: i32,
}

impl EmbeddedRuntimeExitStatus {
    pub fn code(&self) -> Option<i32> {
        Some(self.code)
    }
}

impl EmbeddedRuntime {
    pub fn start(
        script_path: PathBuf,
        config_path: PathBuf,
        working_dir: Option<PathBuf>,
    ) -> Result<Self, String> {
        let bridge_id = NEXT_BRIDGE_ID.fetch_add(1, Ordering::Relaxed);
        let (request_tx, request_rx) = tokio::sync::mpsc::unbounded_channel();
        let (response_tx, response_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_error = Arc::new(Mutex::new(None));
        let worker_error_for_thread = Arc::clone(&worker_error);
        let composition_updates = Arc::new(Mutex::new(HashMap::new()));

        bridge_registrations()
            .lock()
            .map_err(|_| "runtime bridge registry is poisoned".to_owned())?
            .insert(
                bridge_id,
                BridgeRegistration {
                    requests: request_rx,
                    responses: response_tx,
                    composition_updates: Arc::clone(&composition_updates),
                },
            );

        let worker = match thread::Builder::new()
            .name("shoji-deno-runtime".to_owned())
            .spawn(move || {
                let result = run_runtime(
                    bridge_id,
                    &script_path,
                    &config_path,
                    working_dir.as_deref(),
                    &ready_tx,
                );
                if let Err(error) = result {
                    if let Ok(mut registrations) = bridge_registrations().lock() {
                        registrations.remove(&bridge_id);
                    }
                    if let Ok(mut slot) = worker_error_for_thread.lock() {
                        *slot = Some(error.clone());
                    }
                    let _ = ready_tx.send(Err(error));
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                if let Ok(mut registrations) = bridge_registrations().lock() {
                    registrations.remove(&bridge_id);
                }
                return Err(format!("failed to spawn embedded runtime thread: {error}"));
            }
        };

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                requests: Some(request_tx),
                responses: response_rx,
                composition_updates,
                worker: Some(worker),
                worker_error,
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err("embedded runtime exited before initialization".to_owned())
            }
        }
    }

    pub fn write_request(&self, request: &str) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::Json(request.to_owned()))
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn write_composition_request(
        &self,
        request: NativeCompositionRequest,
    ) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::Composition(request))
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn write_scheduler_request(&self, request: NativeSchedulerRequest) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::Scheduler(request))
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn write_cached_fast_request(
        &self,
        request_id: u64,
        window_id: String,
        force_full_reevaluation: bool,
        now_ms: u64,
    ) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::CachedFast {
                request_id,
                window_id,
                force_full_reevaluation,
                now_ms,
            })
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn write_scheduler_fast_request(&self, request_id: u64, now_ms: u64) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::SchedulerFast { request_id, now_ms })
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn take_composition_update(
        &self,
        request_id: u64,
    ) -> Result<Option<NativeCompositionUpdate>, String> {
        self.composition_updates
            .lock()
            .map_err(|_| "composition update store is poisoned".to_owned())
            .map(|mut updates| updates.remove(&request_id))
    }

    pub fn read_response(&self) -> Result<Option<EmbeddedRuntimeResponse>, String> {
        match self.responses.recv() {
            Ok(response) => Ok(Some(response)),
            Err(_) => {
                // The V8 runtime drops its response sender immediately before
                // the worker records the terminal error. Give that hand-off a
                // short bounded window so callers receive the real JS/op error
                // instead of a misleading clean EOF.
                for _ in 0..20 {
                    if let Some(error) = self
                        .worker_error
                        .lock()
                        .ok()
                        .and_then(|error| error.clone())
                    {
                        return Err(error);
                    }
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok(None)
            }
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<EmbeddedRuntimeExitStatus>> {
        let Some(worker) = self.worker.as_ref() else {
            return Ok(Some(EmbeddedRuntimeExitStatus { code: 0 }));
        };
        if !worker.is_finished() {
            return Ok(None);
        }
        let worker = self.worker.take().expect("worker checked above");
        let code = if worker.join().is_ok() { 0 } else { -1 };
        Ok(Some(EmbeddedRuntimeExitStatus { code }))
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        self.requests.take();
        Ok(())
    }

    pub fn wait(&mut self) -> std::io::Result<EmbeddedRuntimeExitStatus> {
        self.requests.take();
        let code = self
            .worker
            .take()
            .map(|worker| if worker.join().is_ok() { 0 } else { -1 })
            .unwrap_or_default();
        Ok(EmbeddedRuntimeExitStatus { code })
    }

    fn failure_message(&self, fallback: &str) -> String {
        self.worker_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
            .unwrap_or_else(|| fallback.to_owned())
    }
}

impl Drop for EmbeddedRuntime {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_runtime(
    bridge_id: u32,
    script_path: &PathBuf,
    config_path: &PathBuf,
    working_dir: Option<&std::path::Path>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let runtime_working_dir = working_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    RUNTIME_CURRENT_DIR.with(|path| {
        *path.borrow_mut() = runtime_working_dir.clone();
    });

    let runtime_root = script_path
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| {
            format!(
                "failed to derive TypeScript runtime root from {}",
                script_path.display()
            )
        })?;
    let module = Module::load(script_path)
        .map_err(|error| format!("failed to read {}: {error}", script_path.display()))?;
    let mut runtime = Runtime::new(RuntimeOptions {
        extensions: vec![shoji_runtime_bridge::init()],
        import_provider: Some(Box::new(ShojiImportProvider::new(runtime_root)?)),
        ..Default::default()
    })
    .map_err(|error| format!("failed to create RustyScript runtime: {error}"))?;

    runtime
        .set_current_dir(&runtime_working_dir)
        .map_err(|error| format!("failed to set runtime working directory: {error}"))?;

    let handle = runtime
        .load_module(&module)
        .map_err(|error| format!("failed to load TypeScript runtime: {error:?}"))?;
    ready
        .send(Ok(()))
        .map_err(|_| "runtime owner disappeared during initialization".to_owned())?;

    runtime
        .call_function::<()>(
            Some(&handle),
            "runEmbeddedRuntime",
            json_args!(config_path.to_string_lossy(), bridge_id),
        )
        .map_err(|error| format!("embedded TypeScript runtime failed: {error}"))
}
