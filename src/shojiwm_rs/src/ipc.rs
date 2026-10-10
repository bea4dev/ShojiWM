//! A Unix-socket IPC server for external clients (bars, launchers), speaking
//! the same newline-delimited JSON as `shoji_wm/ipc`:
//!
//! ```text
//! client -> server   { "id"?: number, "method": string, "params"?: any }
//! server -> client   { "id": number, "result": any }        (response)
//!                    { "id": number, "error": string }      (error)
//!                    { "event": string, "payload": any }    (broadcast)
//! ```
//!
//! Sockets are served on background threads; handlers run on the compositor
//! thread like every other config callback.
//!
//! ```no_run
//! use shojiwm_rs::{ipc::IpcServer, prelude::*};
//!
//! let ipc = IpcServer::new().expect("socket");
//! ipc.handle("ping", |_params| Ok(serde_json::json!("pong")));
//! ipc.broadcast("hello", serde_json::json!({ "from": "config" }));
//! ```

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};

use crate::compositor::COMPOSITOR;

type Handler = Rc<dyn Fn(&Value) -> Result<Value, String>>;

struct Request {
    client: Arc<Client>,
    id: Option<Value>,
    method: String,
    params: Value,
}

struct Client {
    stream: Mutex<UnixStream>,
    alive: AtomicBool,
}

impl Client {
    fn write(&self, message: &Value) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let mut frame = message.to_string();
        frame.push('\n');
        let ok = self
            .stream
            .lock()
            .map(|mut stream| stream.write_all(frame.as_bytes()).is_ok())
            .unwrap_or(false);
        if !ok {
            self.alive.store(false, Ordering::Relaxed);
        }
    }
}

/// The socket path `shoji_wm/ipc` uses: `$XDG_RUNTIME_DIR/shojiwm-$WAYLAND_DISPLAY.sock`.
///
/// Inside the compositor the display is its own socket name, not
/// `WAYLAND_DISPLAY`: the config runs before the compositor points that
/// variable at itself, and nested in another compositor it would name the
/// parent, whose IPC socket this would then replace.
pub fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let display = crate::runtime::wayland_display()
        .or_else(|| std::env::var("WAYLAND_DISPLAY").ok())
        .unwrap_or_else(|| "wayland-0".into());
    PathBuf::from(runtime_dir).join(format!("shojiwm-{display}.sock"))
}

/// A running IPC server. Clone it freely; it stops when [`close`](Self::close)d.
#[derive(Clone)]
pub struct IpcServer {
    handlers: Rc<RefCell<HashMap<String, Handler>>>,
    clients: Arc<Mutex<Vec<Arc<Client>>>>,
    closed: Arc<AtomicBool>,
    path: PathBuf,
}

impl IpcServer {
    pub fn new() -> std::io::Result<Self> {
        Self::bind(default_socket_path())
    }

    pub fn bind(path: PathBuf) -> std::io::Result<Self> {
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let handlers: Rc<RefCell<HashMap<String, Handler>>> = Rc::default();
        let clients: Arc<Mutex<Vec<Arc<Client>>>> = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));

        let sender = {
            let handlers = handlers.clone();
            COMPOSITOR.channel(move |request: Request| {
                let handler = handlers.borrow().get(&request.method).cloned();
                let response = match handler {
                    Some(handler) => match handler(&request.params) {
                        Ok(result) => json!({ "result": result }),
                        Err(error) => json!({ "error": error }),
                    },
                    None => json!({ "error": format!("unknown method: {}", request.method) }),
                };
                if let Some(id) = request.id {
                    let mut response = response;
                    response["id"] = id;
                    request.client.write(&response);
                }
            })
        };

        {
            let clients = clients.clone();
            let closed = closed.clone();
            std::thread::Builder::new()
                .name("shoji-ipc-accept".into())
                .spawn(move || {
                    for stream in listener.incoming() {
                        if closed.load(Ordering::Relaxed) {
                            break;
                        }
                        let Ok(stream) = stream else {
                            continue;
                        };
                        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                        let Ok(reader) = stream.try_clone() else {
                            continue;
                        };
                        let client = Arc::new(Client {
                            stream: Mutex::new(stream),
                            alive: AtomicBool::new(true),
                        });
                        if let Ok(mut clients) = clients.lock() {
                            clients.push(client.clone());
                        }
                        let sender = sender.clone();
                        let _ = std::thread::Builder::new()
                            .name("shoji-ipc-client".into())
                            .spawn(move || {
                                for line in BufReader::new(reader).lines() {
                                    let Ok(line) = line else {
                                        break;
                                    };
                                    let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                                        continue;
                                    };
                                    let Some(method) = message.get("method").and_then(Value::as_str) else {
                                        continue;
                                    };
                                    sender.send(Request {
                                        client: client.clone(),
                                        id: message.get("id").filter(|id| !id.is_null()).cloned(),
                                        method: method.to_owned(),
                                        params: message.get("params").cloned().unwrap_or(Value::Null),
                                    });
                                }
                                client.alive.store(false, Ordering::Relaxed);
                            });
                    }
                })?;
        }

        Ok(Self {
            handlers,
            clients,
            closed,
            path,
        })
    }

    /// Answer `method`; the result (or error) goes back to the caller when
    /// it sent an `id`.
    pub fn handle(&self, method: &str, handler: impl Fn(&Value) -> Result<Value, String> + 'static) {
        self.handlers
            .borrow_mut()
            .insert(method.to_owned(), Rc::new(handler));
    }

    /// Push `{ event, payload }` to every connected client.
    pub fn broadcast(&self, event: &str, payload: Value) {
        let message = json!({ "event": event, "payload": payload });
        let clients: Vec<Arc<Client>> = match self.clients.lock() {
            Ok(mut clients) => {
                clients.retain(|client| client.alive.load(Ordering::Relaxed));
                clients.clone()
            }
            Err(_) => return,
        };
        for client in clients {
            client.write(&message);
        }
    }

    pub fn client_count(&self) -> usize {
        self.clients
            .lock()
            .map(|clients| clients.iter().filter(|client| client.alive.load(Ordering::Relaxed)).count())
            .unwrap_or(0)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        // Unblock the accept loop.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
        if let Ok(mut clients) = self.clients.lock() {
            for client in clients.drain(..) {
                client.alive.store(false, Ordering::Relaxed);
                if let Ok(stream) = client.stream.lock() {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
}
