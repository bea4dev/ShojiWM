use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::CStr,
    os::unix::fs::FileTypeExt,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
    BackgroundEffectConfig, CompiledEffect, DecorationNode, EffectInput, EffectStage,
    OpaqueRegionPolicy, ShaderStage, SurfacePolicy, WindowEffectConfig,
    bridge::{WireCompiledEffect, WireDecorationNode, WireWindowEffectConfig},
    window_model::{
        GestureSwipeEventSnapshot, ManagedWindowRectSnapshot, ManagedWindowState,
        PointerMoveEventSnapshot, TransformOrigin, WaylandLayerSnapshot, WaylandOutputSnapshot,
        WaylandPopupSnapshot, WaylandWindowSnapshot, WindowMoveEventSnapshot,
        WindowResizeEventSnapshot, WindowTransform,
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

/// Effect requests use the same CppGC request envelope as composition
/// requests. The snapshots cross V8 once through serde_v8; no JSON frame is
/// created or parsed on either side.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NativeEffectRequest {
    GetEffectConfig {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    EvaluateLayerEffects {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "outputName")]
        output_name: String,
        layers: Vec<WaylandLayerSnapshot>,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    EvaluatePopupEffects {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "outputName")]
        output_name: String,
        popups: Vec<WaylandPopupSnapshot>,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
}

/// High-frequency pointer, gesture, move and resize events cross the CppGC
/// bridge as V8 values. This avoids allocating and parsing JSON frames while
/// retaining the existing TypeScript event shapes.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NativeInteractionRequest {
    PointerMove {
        #[serde(rename = "requestId")]
        request_id: u64,
        event: PointerMoveEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    PointerMoveAsync {
        #[serde(rename = "requestId")]
        request_id: u64,
        event: PointerMoveEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    GestureSwipe {
        #[serde(rename = "requestId")]
        request_id: u64,
        event: GestureSwipeEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    GestureSwipeAsync {
        #[serde(rename = "requestId")]
        request_id: u64,
        event: GestureSwipeEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowMove {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: String,
        event: WindowMoveEventSnapshot,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "displayState")]
        display_state: std::collections::BTreeMap<String, WaylandOutputSnapshot>,
        #[serde(rename = "inputState")]
        input_state: std::collections::BTreeMap<String, RuntimeInputDeviceSnapshot>,
    },
    WindowResize {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(rename = "windowId")]
        window_id: String,
        event: WindowResizeEventSnapshot,
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
    Effect(NativeEffectRequest),
    Interaction(NativeInteractionRequest),
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

// boxing left as a follow-up (touches all construction/match sites)
#[allow(clippy::large_enum_variant)]
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

// boxing left as a follow-up (see NativeCompositionPatch above)
#[allow(clippy::large_enum_variant)]
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

#[derive(Debug, Clone)]
pub struct NativeLayerEffectAssignment {
    pub layer_id: String,
    pub effects: Option<WindowEffectConfig>,
}

#[derive(Debug, Clone)]
pub struct NativePopupEffectAssignment {
    pub popup_id: String,
    pub effects: Option<WindowEffectConfig>,
    pub surface_policy: Option<SurfacePolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeEffectUpdateKind {
    Background,
    Window,
    Layers,
    Popups,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeEffectTargetKind {
    Background,
    Window,
    Layer,
    Popup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeEffectSlotKind {
    Background,
    Behind,
    BehindRootSurface,
    InFront,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum NativeEffectShaderPathSegment {
    Input,
    Pipeline { index: usize },
    Texture { name: String },
}

#[derive(Debug, Clone)]
struct NativeEffectUniformPatch {
    target_kind: NativeEffectTargetKind,
    target_id: String,
    effect_slot: NativeEffectSlotKind,
    shader_path: Vec<NativeEffectShaderPathSegment>,
    name: String,
    value: super::ShaderUniformValue,
}

#[derive(Debug, Clone)]
pub(super) struct NativeEffectUniformPatchBatch {
    kind: NativeEffectUpdateKind,
    target_ids: Vec<String>,
    patches: Vec<NativeEffectUniformPatch>,
}

// boxing left as a follow-up (see NativeCompositionPatch above)
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum NativeEffectUpdate {
    Background(Option<BackgroundEffectConfig>),
    Window {
        window_id: String,
        effects: Option<WindowEffectConfig>,
    },
    Layers(Vec<NativeLayerEffectAssignment>),
    Popups(Vec<NativePopupEffectAssignment>),
    UniformPatches(NativeEffectUniformPatchBatch),
}

#[derive(Debug, Clone)]
pub struct ResolvedNativeEffectUpdate {
    pub update: NativeEffectUpdate,
    pub uniform_only: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeInteractionKind {
    PointerMove,
    PointerMoveAsync,
    GestureSwipe,
    GestureSwipeAsync,
    WindowMove,
    WindowResize,
}

impl NativeInteractionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PointerMove => "pointerMove",
            Self::PointerMoveAsync => "pointerMoveAsync",
            Self::GestureSwipe => "gestureSwipe",
            Self::GestureSwipeAsync => "gestureSwipeAsync",
            Self::WindowMove => "windowMove",
            Self::WindowResize => "windowResize",
        }
    }
}

#[derive(Debug)]
pub struct NativeInteractionResponse {
    pub request_id: u64,
    pub kind: NativeInteractionKind,
    pub invoked: bool,
    pub dirty: bool,
    pub dirty_window_ids: Vec<String>,
    pub dirty_managed_window_ids: Vec<String>,
    pub dirty_window_node_ids: HashMap<String, Vec<String>>,
    pub dirty_layer_node_ids: HashMap<String, Vec<String>>,
    pub actions: Vec<super::evaluator::RuntimeWindowAction>,
    pub next_poll_in_ms: Option<u64>,
}

#[derive(Debug)]
pub enum EmbeddedRuntimeResponse {
    Json(Vec<u8>),
    Scheduler(NativeSchedulerResponse),
    Cached(NativeCachedResponse),
    Interaction(NativeInteractionResponse),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNativeInteractionResponse {
    request_id: u64,
    kind: String,
    invoked: bool,
    dirty: bool,
    dirty_window_ids: Vec<String>,
    #[serde(default)]
    dirty_managed_window_ids: Vec<String>,
    #[serde(default)]
    dirty_window_node_ids: HashMap<String, Vec<String>>,
    #[serde(default)]
    dirty_layer_node_ids: HashMap<String, Vec<String>>,
    actions: Vec<super::evaluator::RuntimeWindowAction>,
    next_poll_in_ms: Option<u64>,
}

impl NativeCompositionUpdate {
    pub fn window_id(&self) -> &str {
        match self {
            Self::Full { window_id, .. } | Self::Patches { window_id, .. } => window_id,
        }
    }
}

// wire counterpart of NativeCompositionUpdate; same rationale
#[allow(clippy::large_enum_variant)]
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

// wire counterpart of NativeEffectUpdate; same rationale
#[allow(clippy::large_enum_variant)]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WireNativeEffectUpdate {
    Background {
        effect: Option<WireCompiledEffect>,
    },
    Window {
        #[serde(rename = "windowId")]
        window_id: String,
        effects: Option<WireWindowEffectConfig>,
    },
    Layers {
        assignments: Vec<WireNativeLayerEffectAssignment>,
    },
    Popups {
        assignments: Vec<WireNativePopupEffectAssignment>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNativeLayerEffectAssignment {
    layer_id: String,
    effects: Option<WireWindowEffectConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNativePopupEffectAssignment {
    popup_id: String,
    effects: Option<WireWindowEffectConfig>,
    surface_policy: Option<WireNativeSurfacePolicy>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNativeSurfacePolicy {
    opaque_region: Option<String>,
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

/// Bind the IPC socket.
///
/// The TS side used `Deno.listen`, which needs RustyScript's `web` feature and
/// so drags in deno_net -> deno_tls -> rustls/ring/reqwest/quinn, and hands
/// configs outbound network access they have no use for. Line framing lives
/// here too, so the TS side needs no TextDecoder/TextEncoder either.
#[op2]
#[cppgc]
fn op_shoji_ipc_listen(#[string] path: &str) -> Result<ShojiIpcListener, std::io::Error> {
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    let inner = Arc::new(IpcListenerInner {
        listener: tokio::sync::Mutex::new(Some(tokio::net::UnixListener::from_std(listener)?)),
        cancel: tokio::sync::Notify::new(),
        closed: AtomicBool::new(false),
    });
    IPC_LISTENERS.with(|cell| cell.borrow_mut().push(Arc::downgrade(&inner)));
    Ok(ShojiIpcListener { inner })
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
    effect_updates: Arc<Mutex<HashMap<u64, NativeEffectUpdate>>>,
    effect_uniform_patch_count: Arc<AtomicU32>,
}

static NEXT_BRIDGE_ID: AtomicU32 = AtomicU32::new(1);
static BRIDGE_REGISTRATIONS: OnceLock<Mutex<HashMap<u32, BridgeRegistration>>> = OnceLock::new();

fn bridge_registrations() -> &'static Mutex<HashMap<u32, BridgeRegistration>> {
    BRIDGE_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct IpcListenerInner {
    // Option so close can drop the fd instead of waiting for a cppgc
    // finalizer, which is not guaranteed to run before isolate teardown.
    listener: tokio::sync::Mutex<Option<tokio::net::UnixListener>>,
    cancel: tokio::sync::Notify,
    closed: AtomicBool,
}

struct IpcConnectionInner {
    reader: tokio::sync::Mutex<Option<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>>,
    writer: tokio::sync::Mutex<Option<tokio::net::unix::OwnedWriteHalf>>,
    cancel: tokio::sync::Notify,
    closed: AtomicBool,
}

// A reload builds a fresh isolate rather than resetting this one, so sockets
// are tracked weakly and closed explicitly at teardown.
//
// Thread-local, not global: each runtime owns a dedicated thread (as
// RUNTIME_CURRENT_DIR above relies on), and a reload overlaps a new runtime
// with the old one's teardown. A process-wide registry would let the outgoing
// runtime close the incoming runtime's freshly bound listener.
thread_local! {
    static IPC_LISTENERS: RefCell<Vec<Weak<IpcListenerInner>>> = const { RefCell::new(Vec::new()) };
    static IPC_CONNECTIONS: RefCell<Vec<Weak<IpcConnectionInner>>> = const { RefCell::new(Vec::new()) };
}

fn close_ipc_listener(inner: &IpcListenerInner) {
    if !inner.closed.swap(true, Ordering::AcqRel) {
        inner.cancel.notify_waiters();
    }
    // Fails while an accept holds the lock; that accept drops it on wake.
    if let Ok(mut listener) = inner.listener.try_lock() {
        listener.take();
    }
}

fn close_ipc_connection(inner: &IpcConnectionInner) {
    if !inner.closed.swap(true, Ordering::AcqRel) {
        inner.cancel.notify_waiters();
    }
    if let Ok(mut reader) = inner.reader.try_lock() {
        reader.take();
    }
    if let Ok(mut writer) = inner.writer.try_lock() {
        writer.take();
    }
}

/// Close the IPC sockets opened by this runtime's thread. Called before the
/// isolate drops so a reload cycle cannot leak listener fds.
fn close_ipc_sockets() {
    let listeners = IPC_LISTENERS.with(|cell| cell.take());
    for weak in listeners {
        if let Some(inner) = weak.upgrade() {
            close_ipc_listener(&inner);
        }
    }
    let connections = IPC_CONNECTIONS.with(|cell| cell.take());
    for weak in connections {
        if let Some(inner) = weak.upgrade() {
            close_ipc_connection(&inner);
        }
    }
}

#[repr(C)]
struct ShojiIpcListener {
    inner: Arc<IpcListenerInner>,
}

#[repr(C)]
struct ShojiIpcConnection {
    inner: Arc<IpcConnectionInner>,
}

#[repr(C)]
struct ShojiRuntimeBridge {
    requests: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<BridgeRequest>>,
    responses: Sender<EmbeddedRuntimeResponse>,
    composition_updates: Arc<Mutex<HashMap<u64, NativeCompositionUpdate>>>,
    effect_updates: Arc<Mutex<HashMap<u64, NativeEffectUpdate>>>,
    composition_uniform_slots: Mutex<HashMap<u32, NativeShaderUniformSlot>>,
    effect_uniform_slots: Mutex<HashMap<u32, NativeEffectUniformSlot>>,
    effect_uniform_patch_count: Arc<AtomicU32>,
    pending_native_response: Mutex<Option<EmbeddedRuntimeResponse>>,
}

#[derive(Debug, Clone)]
struct NativeShaderUniformSlot {
    window_id: String,
    node_id: String,
    stage_index: usize,
    name: String,
}

#[derive(Debug, Clone)]
struct NativeEffectUniformSlot {
    target_kind: NativeEffectTargetKind,
    target_id: String,
    effect_slot: NativeEffectSlotKind,
    shader_path: Vec<NativeEffectShaderPathSegment>,
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

unsafe impl GarbageCollected for ShojiIpcListener {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static CStr {
        c"ShojiIpcListener"
    }
}

unsafe impl GarbageCollected for ShojiIpcConnection {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static CStr {
        c"ShojiIpcConnection"
    }
}

#[op2]
impl ShojiIpcListener {
    // Resolves to null once the listener is closed.
    #[async_method]
    #[cppgc]
    async fn accept(&self) -> Result<Option<ShojiIpcConnection>, std::io::Error> {
        let inner = Arc::clone(&self.inner);
        let cancelled = inner.cancel.notified();
        tokio::pin!(cancelled);
        // enable() registers the waiter before the flag is read, so a close
        // landing between the check and the await still wakes this future.
        cancelled.as_mut().enable();
        if inner.closed.load(Ordering::Acquire) {
            inner.listener.lock().await.take();
            return Ok(None);
        }

        let mut guard = inner.listener.lock().await;
        let Some(listener) = guard.as_ref() else {
            return Ok(None);
        };
        let accepted = tokio::select! {
            biased;
            () = &mut cancelled => None,
            accepted = listener.accept() => Some(accepted),
        };
        match accepted {
            None => {
                guard.take();
                Ok(None)
            }
            Some(accepted) => {
                let (stream, _) = accepted?;
                let (reader, writer) = stream.into_split();
                let inner = Arc::new(IpcConnectionInner {
                    reader: tokio::sync::Mutex::new(Some(tokio::io::BufReader::new(reader))),
                    writer: tokio::sync::Mutex::new(Some(writer)),
                    cancel: tokio::sync::Notify::new(),
                    closed: AtomicBool::new(false),
                });
                IPC_CONNECTIONS.with(|cell| cell.borrow_mut().push(Arc::downgrade(&inner)));
                Ok(Some(ShojiIpcConnection { inner }))
            }
        }
    }

    #[fast]
    fn close(&self) {
        close_ipc_listener(&self.inner);
    }
}

#[op2]
impl ShojiIpcConnection {
    // One NDJSON line including its trailing newline. Empty string means EOF,
    // which is unambiguous because a real line always carries the delimiter.
    #[async_method]
    #[string]
    async fn read_line(&self) -> Result<String, std::io::Error> {
        let inner = Arc::clone(&self.inner);
        let cancelled = inner.cancel.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if inner.closed.load(Ordering::Acquire) {
            return Ok(String::new());
        }

        let mut guard = inner.reader.lock().await;
        let Some(reader) = guard.as_mut() else {
            return Ok(String::new());
        };
        let mut line = Vec::new();
        let read = tokio::select! {
            biased;
            () = &mut cancelled => None,
            read = reader.read_until(b'\n', &mut line) => Some(read),
        };
        match read {
            None => {
                guard.take();
                Ok(String::new())
            }
            Some(read) => {
                read?;
                Ok(String::from_utf8_lossy(&line).into_owned())
            }
        }
    }

    #[async_method]
    async fn write(&self, #[string] data: String) -> Result<(), std::io::Error> {
        let inner = Arc::clone(&self.inner);
        if inner.closed.load(Ordering::Acquire) {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        let mut guard = inner.writer.lock().await;
        let Some(writer) = guard.as_mut() else {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        };
        writer.write_all(data.as_bytes()).await
    }

    #[fast]
    fn close(&self) {
        close_ipc_connection(&self.inner);
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
    fn effect(&self) -> Option<NativeEffectRequest> {
        let mut request = self.request.lock().ok()?;
        if !matches!(request.as_ref(), Some(BridgeRequest::Effect(_))) {
            return None;
        }
        match request.take() {
            Some(BridgeRequest::Effect(request)) => Some(request),
            _ => None,
        }
    }

    #[serde]
    fn interaction(&self) -> Option<NativeInteractionRequest> {
        let mut request = self.request.lock().ok()?;
        if !matches!(request.as_ref(), Some(BridgeRequest::Interaction(_))) {
            return None;
        }
        match request.take() {
            Some(BridgeRequest::Interaction(request)) => Some(request),
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
            effect_updates: registration.effect_updates,
            composition_uniform_slots: Mutex::new(HashMap::new()),
            effect_uniform_slots: Mutex::new(HashMap::new()),
            effect_uniform_patch_count: registration.effect_uniform_patch_count,
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

    fn write_interaction_response(
        &self,
        #[serde] response: WireNativeInteractionResponse,
    ) -> Result<(), std::io::Error> {
        let kind = native_interaction_kind(&response.kind)?;
        self.responses
            .send(EmbeddedRuntimeResponse::Interaction(
                NativeInteractionResponse {
                    request_id: response.request_id,
                    kind,
                    invoked: response.invoked,
                    dirty: response.dirty,
                    dirty_window_ids: response.dirty_window_ids,
                    dirty_managed_window_ids: response.dirty_managed_window_ids,
                    dirty_window_node_ids: response.dirty_window_node_ids,
                    dirty_layer_node_ids: response.dirty_layer_node_ids,
                    actions: response.actions,
                    next_poll_in_ms: response.next_poll_in_ms,
                },
            ))
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
                surface_policy: (flags & (1 << 12) != 0).then_some(crate::ssd::SurfacePolicy {
                    opaque_region: crate::ssd::OpaqueRegionPolicy::Ignore,
                }),
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

    fn write_effect_update(
        &self,
        request_id: f64,
        #[serde] update: WireNativeEffectUpdate,
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native effect decode");
        let request_id = checked_request_id(request_id)?;
        let bridge_error = |error: super::DecorationBridgeError| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
        };
        let update = match update {
            WireNativeEffectUpdate::Background { effect } => NativeEffectUpdate::Background(
                effect
                    .map(TryInto::try_into)
                    .transpose()
                    .map_err(bridge_error)?,
            ),
            WireNativeEffectUpdate::Window { window_id, effects } => NativeEffectUpdate::Window {
                window_id,
                effects: effects
                    .map(TryInto::try_into)
                    .transpose()
                    .map_err(bridge_error)?,
            },
            WireNativeEffectUpdate::Layers { assignments } => NativeEffectUpdate::Layers(
                assignments
                    .into_iter()
                    .map(|assignment| {
                        Ok(NativeLayerEffectAssignment {
                            layer_id: assignment.layer_id,
                            effects: assignment
                                .effects
                                .map(TryInto::try_into)
                                .transpose()
                                .map_err(bridge_error)?,
                        })
                    })
                    .collect::<Result<Vec<_>, std::io::Error>>()?,
            ),
            WireNativeEffectUpdate::Popups { assignments } => NativeEffectUpdate::Popups(
                assignments
                    .into_iter()
                    .map(|assignment| {
                        Ok(NativePopupEffectAssignment {
                            popup_id: assignment.popup_id,
                            effects: assignment
                                .effects
                                .map(TryInto::try_into)
                                .transpose()
                                .map_err(bridge_error)?,
                            surface_policy: assignment.surface_policy.map(|policy| SurfacePolicy {
                                opaque_region: match policy.opaque_region.as_deref() {
                                    Some("ignore") => OpaqueRegionPolicy::Ignore,
                                    _ => OpaqueRegionPolicy::Trust,
                                },
                            }),
                        })
                    })
                    .collect::<Result<Vec<_>, std::io::Error>>()?,
            ),
        };
        let mut updates = self
            .effect_updates
            .lock()
            .map_err(|_| std::io::Error::other("effect update store is poisoned"))?;
        match updates.entry(request_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(update);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "effect update was already written for this request",
                ));
            }
        }
        Ok(())
    }

    #[fast]
    fn begin_effect_shader_uniform_slot_patches(
        &self,
        request_id: f64,
        update_kind: u32,
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let kind = native_effect_update_kind(update_kind)?;
        let mut updates = self
            .effect_updates
            .lock()
            .map_err(|_| std::io::Error::other("effect update store is poisoned"))?;
        match updates.entry(request_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(NativeEffectUpdate::UniformPatches(
                    NativeEffectUniformPatchBatch {
                        kind,
                        target_ids: Vec::new(),
                        patches: Vec::new(),
                    },
                ));
                Ok(())
            }
            std::collections::hash_map::Entry::Occupied(_) => Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "effect update was already written for this request",
            )),
        }
    }

    #[fast]
    fn add_effect_shader_uniform_patch_target(
        &self,
        request_id: f64,
        #[string] target_id: &str,
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let mut updates = self
            .effect_updates
            .lock()
            .map_err(|_| std::io::Error::other("effect update store is poisoned"))?;
        let Some(NativeEffectUpdate::UniformPatches(batch)) = updates.get_mut(&request_id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "effect uniform patch batch was not started",
            ));
        };
        batch.target_ids.push(target_id.to_owned());
        Ok(())
    }

    #[fast]
    fn register_effect_shader_uniform_slot(
        &self,
        slot_id: u32,
        target_kind: u32,
        #[string] target_id: &str,
        effect_slot: u32,
        #[string] shader_path_json: &str,
        #[string] name: &str,
    ) -> Result<(), std::io::Error> {
        let shader_path = serde_json::from_str(shader_path_json).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid effect shader path: {error}"),
            )
        })?;
        self.effect_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("effect uniform slot store is poisoned"))?
            .insert(
                slot_id,
                NativeEffectUniformSlot {
                    target_kind: native_effect_target_kind(target_kind)?,
                    target_id: target_id.to_owned(),
                    effect_slot: native_effect_slot_kind(effect_slot)?,
                    shader_path,
                    name: name.to_owned(),
                },
            );
        Ok(())
    }

    #[fast]
    fn clear_effect_shader_uniform_slots(
        &self,
        target_kind: u32,
        #[string] target_id: &str,
    ) -> Result<(), std::io::Error> {
        let target_kind = native_effect_target_kind(target_kind)?;
        self.effect_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("effect uniform slot store is poisoned"))?
            .retain(|_, slot| {
                slot.target_kind != target_kind || slot.target_id.as_str() != target_id
            });
        Ok(())
    }

    #[fast]
    fn write_effect_shader_uniform_slot_patch(
        &self,
        request_id: f64,
        slot_id: u32,
        value_len: u32,
        x: f64,
        y: f64,
        z: f64,
        w: f64,
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native effect uniform slot patch");
        let request_id = checked_request_id(request_id)?;
        let value = native_shader_uniform_value(value_len, x, y, z, w)?;
        let slot = self
            .effect_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("effect uniform slot store is poisoned"))?
            .get(&slot_id)
            .cloned()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "effect shader uniform slot is not registered",
                )
            })?;
        let mut updates = self
            .effect_updates
            .lock()
            .map_err(|_| std::io::Error::other("effect update store is poisoned"))?;
        let Some(NativeEffectUpdate::UniformPatches(batch)) = updates.get_mut(&request_id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "effect uniform patch batch was not started",
            ));
        };
        if !native_effect_update_accepts_target(batch.kind, slot.target_kind) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "effect uniform slot target does not match the update kind",
            ));
        }
        batch.patches.push(NativeEffectUniformPatch {
            target_kind: slot.target_kind,
            target_id: slot.target_id,
            effect_slot: slot.effect_slot,
            shader_path: slot.shader_path,
            name: slot.name,
            value,
        });
        self.effect_uniform_patch_count
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[fast]
    fn write_effect_shader_uniform_array_slot_patch(
        &self,
        request_id: f64,
        slot_id: u32,
        element_width: u32,
        #[buffer] values: &[f32],
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native effect uniform array slot patch");
        let request_id = checked_request_id(request_id)?;
        let value = native_shader_uniform_array_value(element_width, values)?;
        let slot = self
            .effect_uniform_slots
            .lock()
            .map_err(|_| std::io::Error::other("effect uniform slot store is poisoned"))?
            .get(&slot_id)
            .cloned()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "effect shader uniform slot is not registered",
                )
            })?;
        let mut updates = self
            .effect_updates
            .lock()
            .map_err(|_| std::io::Error::other("effect update store is poisoned"))?;
        let Some(NativeEffectUpdate::UniformPatches(batch)) = updates.get_mut(&request_id) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "effect uniform patch batch was not started",
            ));
        };
        if !native_effect_update_accepts_target(batch.kind, slot.target_kind) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "effect uniform slot target does not match the update kind",
            ));
        }
        batch.patches.push(NativeEffectUniformPatch {
            target_kind: slot.target_kind,
            target_id: slot.target_id,
            effect_slot: slot.effect_slot,
            shader_path: slot.shader_path,
            name: slot.name,
            value,
        });
        self.effect_uniform_patch_count
            .fetch_add(1, Ordering::Relaxed);
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
    fn write_composition_shader_uniform_array_patch(
        &self,
        request_id: f64,
        #[string] node_id: &str,
        stage_index: u32,
        #[string] name: &str,
        element_width: u32,
        #[buffer] values: &[f32],
    ) -> Result<(), std::io::Error> {
        let request_id = checked_request_id(request_id)?;
        let value = native_shader_uniform_array_value(element_width, values)?;
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
    fn write_composition_shader_uniform_array_slot_patch(
        &self,
        request_id: f64,
        slot_id: u32,
        element_width: u32,
        #[buffer] values: &[f32],
    ) -> Result<(), std::io::Error> {
        timescope::scope!("runtime native uniform array slot patch");
        let request_id = checked_request_id(request_id)?;
        let value = native_shader_uniform_array_value(element_width, values)?;
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

fn native_effect_update_kind(value: u32) -> Result<NativeEffectUpdateKind, std::io::Error> {
    match value {
        0 => Ok(NativeEffectUpdateKind::Background),
        1 => Ok(NativeEffectUpdateKind::Window),
        2 => Ok(NativeEffectUpdateKind::Layers),
        3 => Ok(NativeEffectUpdateKind::Popups),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unknown native effect update kind",
        )),
    }
}

fn native_effect_target_kind(value: u32) -> Result<NativeEffectTargetKind, std::io::Error> {
    match value {
        0 => Ok(NativeEffectTargetKind::Background),
        1 => Ok(NativeEffectTargetKind::Window),
        2 => Ok(NativeEffectTargetKind::Layer),
        3 => Ok(NativeEffectTargetKind::Popup),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unknown native effect target kind",
        )),
    }
}

fn native_interaction_kind(value: &str) -> Result<NativeInteractionKind, std::io::Error> {
    match value {
        "pointerMove" => Ok(NativeInteractionKind::PointerMove),
        "pointerMoveAsync" => Ok(NativeInteractionKind::PointerMoveAsync),
        "gestureSwipe" => Ok(NativeInteractionKind::GestureSwipe),
        "gestureSwipeAsync" => Ok(NativeInteractionKind::GestureSwipeAsync),
        "windowMove" => Ok(NativeInteractionKind::WindowMove),
        "windowResize" => Ok(NativeInteractionKind::WindowResize),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown native interaction response kind: {value}"),
        )),
    }
}

fn native_effect_slot_kind(value: u32) -> Result<NativeEffectSlotKind, std::io::Error> {
    match value {
        0 => Ok(NativeEffectSlotKind::Background),
        1 => Ok(NativeEffectSlotKind::Behind),
        2 => Ok(NativeEffectSlotKind::BehindRootSurface),
        3 => Ok(NativeEffectSlotKind::InFront),
        4 => Ok(NativeEffectSlotKind::Replace),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unknown native effect slot kind",
        )),
    }
}

fn native_effect_update_accepts_target(
    update: NativeEffectUpdateKind,
    target: NativeEffectTargetKind,
) -> bool {
    matches!(
        (update, target),
        (
            NativeEffectUpdateKind::Background,
            NativeEffectTargetKind::Background
        ) | (
            NativeEffectUpdateKind::Window,
            NativeEffectTargetKind::Window
        ) | (
            NativeEffectUpdateKind::Layers,
            NativeEffectTargetKind::Layer
        ) | (
            NativeEffectUpdateKind::Popups,
            NativeEffectTargetKind::Popup
        )
    )
}

fn native_shader_uniform_value(
    value_len: u32,
    x: f64,
    y: f64,
    z: f64,
    w: f64,
) -> Result<super::ShaderUniformValue, std::io::Error> {
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
    match value_len {
        1 => Ok(super::ShaderUniformValue::Float(values[0])),
        2 => Ok(super::ShaderUniformValue::Vec2([values[0], values[1]])),
        3 => Ok(super::ShaderUniformValue::Vec3([
            values[0], values[1], values[2],
        ])),
        4 => Ok(super::ShaderUniformValue::Vec4(values)),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shader uniform length must be between 1 and 4",
        )),
    }
}

fn native_shader_uniform_array_value(
    element_width: u32,
    values: &[f32],
) -> Result<super::ShaderUniformValue, std::io::Error> {
    let width = element_width as usize;
    if !(1..=4).contains(&width) || values.is_empty() || !values.len().is_multiple_of(width) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shader uniform array must be non-empty and divisible by its 1-4 component width",
        ));
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shader uniform array values must be finite",
        ));
    }
    Ok(match width {
        1 => super::ShaderUniformValue::FloatArray(values.to_vec()),
        2 => super::ShaderUniformValue::Vec2Array(
            values.chunks_exact(2).map(|v| [v[0], v[1]]).collect(),
        ),
        3 => super::ShaderUniformValue::Vec3Array(
            values.chunks_exact(3).map(|v| [v[0], v[1], v[2]]).collect(),
        ),
        4 => super::ShaderUniformValue::Vec4Array(
            values
                .chunks_exact(4)
                .map(|v| [v[0], v[1], v[2], v[3]])
                .collect(),
        ),
        _ => unreachable!(),
    })
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
        op_shoji_ipc_listen,
        op_shoji_process_id,
        op_shoji_wake_compositor,
    ],
    objects = [
        ShojiRuntimeBridge,
        RuntimeRequestEnvelope,
        ShojiIpcListener,
        ShojiIpcConnection,
    ],
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
    effect_updates: Arc<Mutex<HashMap<u64, NativeEffectUpdate>>>,
    effect_state_cache: Mutex<NativeEffectStateCache>,
    // read only via the #[cfg(test)] accessor below
    #[cfg_attr(not(test), allow(dead_code))]
    effect_uniform_patch_count: Arc<AtomicU32>,
    worker: Option<JoinHandle<()>>,
    worker_error: Arc<Mutex<Option<String>>>,
}

#[derive(Debug, Clone)]
struct CachedPopupEffect {
    effects: Option<WindowEffectConfig>,
    surface_policy: Option<SurfacePolicy>,
}

#[derive(Debug, Default)]
struct NativeEffectStateCache {
    background: Option<BackgroundEffectConfig>,
    windows: HashMap<String, Option<WindowEffectConfig>>,
    layers: HashMap<String, Option<WindowEffectConfig>>,
    popups: HashMap<String, CachedPopupEffect>,
}

fn resolve_native_effect_update(
    cache: &mut NativeEffectStateCache,
    update: NativeEffectUpdate,
) -> Result<ResolvedNativeEffectUpdate, String> {
    match update {
        NativeEffectUpdate::Background(effect) => {
            cache.background = effect.clone();
            Ok(ResolvedNativeEffectUpdate {
                update: NativeEffectUpdate::Background(effect),
                uniform_only: false,
            })
        }
        NativeEffectUpdate::Window { window_id, effects } => {
            cache.windows.insert(window_id.clone(), effects.clone());
            Ok(ResolvedNativeEffectUpdate {
                update: NativeEffectUpdate::Window { window_id, effects },
                uniform_only: false,
            })
        }
        NativeEffectUpdate::Layers(assignments) => {
            for assignment in &assignments {
                cache
                    .layers
                    .insert(assignment.layer_id.clone(), assignment.effects.clone());
            }
            Ok(ResolvedNativeEffectUpdate {
                update: NativeEffectUpdate::Layers(assignments),
                uniform_only: false,
            })
        }
        NativeEffectUpdate::Popups(assignments) => {
            for assignment in &assignments {
                cache.popups.insert(
                    assignment.popup_id.clone(),
                    CachedPopupEffect {
                        effects: assignment.effects.clone(),
                        surface_policy: assignment.surface_policy,
                    },
                );
            }
            Ok(ResolvedNativeEffectUpdate {
                update: NativeEffectUpdate::Popups(assignments),
                uniform_only: false,
            })
        }
        NativeEffectUpdate::UniformPatches(batch) => {
            for patch in &batch.patches {
                apply_native_effect_uniform_patch(cache, patch)?;
            }
            Ok(ResolvedNativeEffectUpdate {
                update: materialize_native_effect_patch_batch(cache, batch)?,
                uniform_only: true,
            })
        }
    }
}

fn materialize_native_effect_patch_batch(
    cache: &NativeEffectStateCache,
    batch: NativeEffectUniformPatchBatch,
) -> Result<NativeEffectUpdate, String> {
    match batch.kind {
        NativeEffectUpdateKind::Background => {
            Ok(NativeEffectUpdate::Background(cache.background.clone()))
        }
        NativeEffectUpdateKind::Window => {
            let window_id = batch
                .target_ids
                .first()
                .ok_or_else(|| "window effect patch batch has no target".to_owned())?
                .clone();
            let effects = cache
                .windows
                .get(&window_id)
                .cloned()
                .ok_or_else(|| format!("window effect cache is missing: {window_id}"))?;
            Ok(NativeEffectUpdate::Window { window_id, effects })
        }
        NativeEffectUpdateKind::Layers => batch
            .target_ids
            .into_iter()
            .map(|layer_id| {
                let effects = cache
                    .layers
                    .get(&layer_id)
                    .cloned()
                    .ok_or_else(|| format!("layer effect cache is missing: {layer_id}"))?;
                Ok(NativeLayerEffectAssignment { layer_id, effects })
            })
            .collect::<Result<Vec<_>, String>>()
            .map(NativeEffectUpdate::Layers),
        NativeEffectUpdateKind::Popups => batch
            .target_ids
            .into_iter()
            .map(|popup_id| {
                let cached = cache
                    .popups
                    .get(&popup_id)
                    .cloned()
                    .ok_or_else(|| format!("popup effect cache is missing: {popup_id}"))?;
                Ok(NativePopupEffectAssignment {
                    popup_id,
                    effects: cached.effects,
                    surface_policy: cached.surface_policy,
                })
            })
            .collect::<Result<Vec<_>, String>>()
            .map(NativeEffectUpdate::Popups),
    }
}

fn apply_native_effect_uniform_patch(
    cache: &mut NativeEffectStateCache,
    patch: &NativeEffectUniformPatch,
) -> Result<(), String> {
    let effect = match patch.target_kind {
        NativeEffectTargetKind::Background => {
            if patch.effect_slot != NativeEffectSlotKind::Background {
                return Err("background effect patch has a window effect slot".to_owned());
            }
            &mut cache
                .background
                .as_mut()
                .ok_or_else(|| "background effect cache is missing".to_owned())?
                .effect
        }
        NativeEffectTargetKind::Window => {
            let effects = cache
                .windows
                .get_mut(&patch.target_id)
                .and_then(Option::as_mut)
                .ok_or_else(|| format!("window effect cache is missing: {}", patch.target_id))?;
            effect_for_window_slot_mut(effects, patch.effect_slot)?
        }
        NativeEffectTargetKind::Layer => {
            let effects = cache
                .layers
                .get_mut(&patch.target_id)
                .and_then(Option::as_mut)
                .ok_or_else(|| format!("layer effect cache is missing: {}", patch.target_id))?;
            effect_for_window_slot_mut(effects, patch.effect_slot)?
        }
        NativeEffectTargetKind::Popup => {
            let effects = cache
                .popups
                .get_mut(&patch.target_id)
                .and_then(|cached| cached.effects.as_mut())
                .ok_or_else(|| format!("popup effect cache is missing: {}", patch.target_id))?;
            effect_for_window_slot_mut(effects, patch.effect_slot)?
        }
    };
    let shader = effect_shader_stage_mut(effect, &patch.shader_path)
        .ok_or_else(|| "effect shader uniform path no longer exists".to_owned())?;
    let current = shader
        .uniforms
        .get_mut(&patch.name)
        .ok_or_else(|| format!("effect shader uniform is missing: {}", patch.name))?;
    if !current.shape_matches(&patch.value) {
        return Err(format!(
            "effect shader uniform shape changed without a structural update: {}",
            patch.name
        ));
    }
    *current = patch.value.clone();
    Ok(())
}

fn effect_for_window_slot_mut(
    effects: &mut WindowEffectConfig,
    slot: NativeEffectSlotKind,
) -> Result<&mut CompiledEffect, String> {
    let slot = match slot {
        NativeEffectSlotKind::Behind => effects.behind.as_mut(),
        NativeEffectSlotKind::BehindRootSurface => effects.behind_root_surface.as_mut(),
        NativeEffectSlotKind::InFront => effects.in_front.as_mut(),
        NativeEffectSlotKind::Replace => effects.replace.as_mut(),
        NativeEffectSlotKind::Background => {
            return Err("window effect patch has a background effect slot".to_owned());
        }
    }
    .ok_or_else(|| "effect assignment slot no longer exists".to_owned())?;
    Ok(&mut slot.effect)
}

fn effect_shader_stage_mut<'a>(
    effect: &'a mut CompiledEffect,
    path: &[NativeEffectShaderPathSegment],
) -> Option<&'a mut ShaderStage> {
    let (head, tail) = path.split_first()?;
    match head {
        NativeEffectShaderPathSegment::Input => {
            effect_input_shader_stage_mut(&mut effect.input, tail)
        }
        NativeEffectShaderPathSegment::Pipeline { index } => {
            effect_stage_shader_stage_mut(effect.pipeline.get_mut(*index)?, tail)
        }
        NativeEffectShaderPathSegment::Texture { .. } => None,
    }
}

fn effect_stage_shader_stage_mut<'a>(
    stage: &'a mut EffectStage,
    path: &[NativeEffectShaderPathSegment],
) -> Option<&'a mut ShaderStage> {
    match stage {
        EffectStage::Shader(shader) => shader_stage_shader_stage_mut(shader, path),
        EffectStage::Blend { input, .. } => effect_input_shader_stage_mut(input, path),
        EffectStage::Unit(effect) => effect_shader_stage_mut(effect, path),
        EffectStage::RenderTo { effect, .. } => effect_shader_stage_mut(effect, path),
        _ => None,
    }
}

fn effect_input_shader_stage_mut<'a>(
    input: &'a mut EffectInput,
    path: &[NativeEffectShaderPathSegment],
) -> Option<&'a mut ShaderStage> {
    match input {
        EffectInput::Shader(shader) => shader_stage_shader_stage_mut(shader, path),
        _ => None,
    }
}

fn shader_stage_shader_stage_mut<'a>(
    shader: &'a mut ShaderStage,
    path: &[NativeEffectShaderPathSegment],
) -> Option<&'a mut ShaderStage> {
    let Some((head, tail)) = path.split_first() else {
        return Some(shader);
    };
    let NativeEffectShaderPathSegment::Texture { name } = head else {
        return None;
    };
    effect_input_shader_stage_mut(shader.textures.get_mut(name)?, tail)
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
        let effect_updates = Arc::new(Mutex::new(HashMap::new()));
        let effect_uniform_patch_count = Arc::new(AtomicU32::new(0));

        bridge_registrations()
            .lock()
            .map_err(|_| "runtime bridge registry is poisoned".to_owned())?
            .insert(
                bridge_id,
                BridgeRegistration {
                    requests: request_rx,
                    responses: response_tx,
                    composition_updates: Arc::clone(&composition_updates),
                    effect_updates: Arc::clone(&effect_updates),
                    effect_uniform_patch_count: Arc::clone(&effect_uniform_patch_count),
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
                effect_updates,
                effect_state_cache: Mutex::new(NativeEffectStateCache::default()),
                effect_uniform_patch_count,
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

    pub fn write_effect_request(&self, request: NativeEffectRequest) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::Effect(request))
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn write_interaction_request(
        &self,
        request: NativeInteractionRequest,
    ) -> Result<(), String> {
        self.requests
            .as_ref()
            .ok_or_else(|| "embedded runtime is closed".to_owned())?
            .send(BridgeRequest::Interaction(request))
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

    pub fn take_effect_update(
        &self,
        request_id: u64,
    ) -> Result<Option<ResolvedNativeEffectUpdate>, String> {
        let update = self
            .effect_updates
            .lock()
            .map_err(|_| "effect update store is poisoned".to_owned())
            .map(|mut updates| updates.remove(&request_id))?;
        let Some(update) = update else {
            return Ok(None);
        };
        let mut cache = self
            .effect_state_cache
            .lock()
            .map_err(|_| "effect state cache is poisoned".to_owned())?;
        resolve_native_effect_update(&mut cache, update).map(Some)
    }

    #[cfg(test)]
    pub fn effect_uniform_patch_count(&self) -> u32 {
        self.effect_uniform_patch_count.load(Ordering::Relaxed)
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
    config_path: &std::path::Path,
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

    let result = runtime
        .call_function::<()>(
            Some(&handle),
            "runEmbeddedRuntime",
            json_args!(config_path.to_string_lossy(), bridge_id),
        )
        .map_err(|error| format!("embedded TypeScript runtime failed: {error}"));

    // Same reasoning for the IPC sockets: a cppgc finalizer may never run, so
    // the listener fd would survive the isolate that opened it.
    close_ipc_sockets();

    // Hot-reload spins up a brand-new isolate per reload rather than
    // resetting this one in place, and a plain Drop leaves freed heap pages
    // in V8's own arena instead of returning them to the OS. Hint V8 to
    // shrink its heap before `runtime` drops here, so this isolate's peak
    // doesn't linger past its own teardown.
    runtime.deno_runtime().v8_isolate().low_memory_notification();

    result
}
