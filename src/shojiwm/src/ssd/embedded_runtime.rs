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
    requests: tokio::sync::mpsc::UnboundedReceiver<String>,
    responses: Sender<String>,
}

static NEXT_BRIDGE_ID: AtomicU32 = AtomicU32::new(1);
static BRIDGE_REGISTRATIONS: OnceLock<Mutex<HashMap<u32, BridgeRegistration>>> = OnceLock::new();

fn bridge_registrations() -> &'static Mutex<HashMap<u32, BridgeRegistration>> {
    BRIDGE_REGISTRATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[repr(C)]
struct ShojiRuntimeBridge {
    requests: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>,
    responses: Sender<String>,
}

unsafe impl GarbageCollected for ShojiRuntimeBridge {
    fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static CStr {
        c"ShojiRuntimeBridge"
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
        })
    }

    #[async_method]
    #[string]
    async fn read_request(&self) -> Option<String> {
        self.requests.lock().await.recv().await
    }

    #[fast]
    fn write_response(&self, #[string] response: String) -> Result<(), std::io::Error> {
        self.responses
            .send(response)
            .map_err(|_| std::io::Error::other("runtime response receiver was dropped"))
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
    objects = [ShojiRuntimeBridge],
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
    requests: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    responses: Receiver<String>,
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

        bridge_registrations()
            .lock()
            .map_err(|_| "runtime bridge registry is poisoned".to_owned())?
            .insert(
                bridge_id,
                BridgeRegistration {
                    requests: request_rx,
                    responses: response_tx,
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
            .send(request.to_owned())
            .map_err(|_| self.failure_message("embedded runtime request channel closed"))
    }

    pub fn read_response(&self) -> Result<Option<Vec<u8>>, String> {
        match self.responses.recv() {
            Ok(response) => Ok(Some(response.into_bytes())),
            Err(_) => {
                let message = self.failure_message("embedded runtime response channel closed");
                if self
                    .worker_error
                    .lock()
                    .ok()
                    .and_then(|error| error.clone())
                    .is_some()
                {
                    Err(message)
                } else {
                    Ok(None)
                }
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
