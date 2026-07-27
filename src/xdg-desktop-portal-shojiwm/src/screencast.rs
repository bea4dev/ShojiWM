//! org.freedesktop.impl.portal.ScreenCast backend implementation.
//!
//! See: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.ScreenCast.html

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::picker::{PickResult, PickerHandle};
use crate::pipewire_stream::{self, StreamHandle, StreamSpec};
use crate::sources::{self, OutputInfo, SourceInfo, SourceKind, ThumbnailUpdate, ToplevelInfo};
use crate::toplevel_stream::{
    self, StreamHandle as ToplevelStreamHandle, StreamSpec as ToplevelStreamSpec,
};

/// SourceTypes bitmask values from the portal spec.
#[allow(dead_code)]
mod source_types {
    pub const MONITOR: u32 = 1 << 0;
    pub const WINDOW: u32 = 1 << 1;
    pub const VIRTUAL: u32 = 1 << 2;
}

/// CursorMode bitmask values from the portal spec.
#[allow(dead_code)]
mod cursor_modes {
    pub const HIDDEN: u32 = 1 << 0;
    pub const EMBEDDED: u32 = 1 << 1;
    pub const METADATA: u32 = 1 << 2;
}

/// What the user picked at SelectSources time. Reused by Start to set up the
/// actual PipeWire stream.
#[derive(Debug, Clone)]
#[allow(dead_code)] // ToplevelInfo fields are read in Phase 5d.
pub enum Selection {
    Output(OutputInfo),
    Toplevel(ToplevelInfo),
}

/// Per-session handle to keep the streaming pipeline alive. Either branch of
/// `Start` may stash one here; dropping it stops the corresponding thread.
#[allow(dead_code)]
enum AnyStreamHandle {
    Output(StreamHandle),
    Toplevel(ToplevelStreamHandle),
}

/// Shared portal state. Held by the ScreenCast interface and by every
/// per-session [`SessionImpl`] object, so a session Close can drop exactly
/// its own stream and bookkeeping.
struct Inner {
    picker: PickerHandle,
    sessions: Mutex<HashMap<String, Selection>>,
    streams: Mutex<HashMap<String, AnyStreamHandle>>,
    /// Per-session `cursor_mode & EMBEDDED != 0`. Filled by SelectSources,
    /// consumed by Start to configure the streaming pipeline.
    cursor_visibility: Mutex<HashMap<String, bool>>,
    /// Per-session `persist_mode` requested at SelectSources. When non-zero,
    /// Start returns `restore_data` so the portal frontend can hand the app a
    /// restore token — that's what lets Chromium's repeated Go Live sessions
    /// (and OBS on relaunch) skip the picker after the first approval.
    persist_modes: Mutex<HashMap<String, u32>>,
    thumbnail_tx: tokio::sync::mpsc::UnboundedSender<ThumbnailUpdate>,
}

impl Inner {
    /// Tear down everything a session owns. Dropping the stream handle stops
    /// the capture thread and removes the PipeWire node.
    fn cleanup_session(
        &self,
        session_key: &str,
    ) {
        // Remove the session FIRST, the stream second. Start() does the
        // opposite (insert stream, then verify the session still exists), so
        // every interleaving of a Close racing a Start ends with the stream
        // dropped exactly once — either here or by Start's verify step.
        // Chromium's Go Live churns sessions fast enough to Close one while
        // its Start is still spawning the capture thread; before this
        // ordering the freshly started stream could land in the map after
        // cleanup had already run, leaving an unowned PipeWire node that
        // delivered zero frames to the app forever.
        self.sessions
            .lock()
            .unwrap()
            .remove(
                session_key,
            );
        let stream = self.streams
            .lock()
            .unwrap()
            .remove(
                session_key,
            );
        let had_stream = stream
            .is_some();
        drop(
            stream,
        );
        self.cursor_visibility
            .lock()
            .unwrap()
            .remove(
                session_key,
            );
        self.persist_modes
            .lock()
            .unwrap()
            .remove(
                session_key,
            );
        tracing::info!(session_key, had_stream, "session closed and cleaned up");
    }
}

pub struct ScreenCast {
    inner: Arc<Inner>,
}

/// Per-session `org.freedesktop.impl.portal.Session` object, exported at the
/// session handle path by CreateSession. Without it the portal frontend's
/// Close calls fail with UnknownObject and our streams outlive their
/// sessions — Chromium's Go Live opens and closes several sessions
/// back-to-back, so the stale driver streams pile up on the output.
struct SessionImpl {
    session_key: String,
    inner: Arc<Inner>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionImpl {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
    ) -> zbus::fdo::Result<()> {
        self.inner.cleanup_session(&self.session_key);
        if let Ok(path) = ObjectPath::try_from(self.session_key.clone()) {
            let _ = server.remove::<Self, _>(&path).await;
        }
        Ok(())
    }
}

/// Vendor tag inside the `(suv)` restore_data blob. The frontend gives the
/// blob back verbatim on later sessions; the tag + version gate decoding.
const RESTORE_VENDOR: &str = "shojiwm";
const RESTORE_VERSION: u32 = 1;

/// Encode a selection as the `v` payload of restore_data. Flat `(ssss)`:
/// ("output", connector_name, "", "") or
/// ("toplevel", identifier, app_id, title). Toplevel identifiers are not
/// stable across compositor restarts, so app_id+title are carried as a
/// fallback match key.
fn encode_restore_data(selection: &Selection) -> Value<'static> {
    let data = match selection {
        Selection::Output(out) => Value::from((
            "output".to_string(),
            out.name.clone(),
            String::new(),
            String::new(),
        )),
        Selection::Toplevel(top) => Value::from((
            "toplevel".to_string(),
            top.identifier.clone(),
            top.app_id.clone(),
            top.title.clone(),
        )),
    };
    Value::from((
        RESTORE_VENDOR.to_string(),
        RESTORE_VERSION,
        Value::new(data),
    ))
}

/// Attach `restore_data` + granted `persist_mode` to Start results when the
/// app asked for persistence. The portal frontend stores the blob in its
/// permission store and hands the app a restore token in our stead.
fn append_restore_results(
    results: &mut HashMap<String, OwnedValue>,
    persist_mode: u32,
    selection: &Selection,
) {
    if persist_mode == 0 {
        return;
    }
    match OwnedValue::try_from(encode_restore_data(selection)) {
        Ok(blob) => {
            results.insert("persist_mode".to_string(), OwnedValue::from(persist_mode));
            results.insert("restore_data".to_string(), blob);
        }
        Err(e) => tracing::warn!("failed to encode restore_data: {e}"),
    }
}

/// Decoded restore request from a `(suv)` restore_data option.
struct RestoreRequest {
    kind: String,
    key: String,
    app_id: String,
    title: String,
}

fn decode_restore_data(value: &OwnedValue) -> Option<RestoreRequest> {
    let structure = match &**value {
        Value::Structure(s) => s,
        _ => return None,
    };
    let fields = structure.fields();
    let vendor = match fields.first()? {
        Value::Str(s) => s.as_str(),
        _ => return None,
    };
    let version = match fields.get(1)? {
        Value::U32(v) => *v,
        _ => return None,
    };
    if vendor != RESTORE_VENDOR || version != RESTORE_VERSION {
        return None;
    }
    let mut inner = fields.get(2)?;
    while let Value::Value(boxed) = inner {
        inner = boxed;
    }
    let data = match inner {
        Value::Structure(s) => s,
        _ => return None,
    };
    let field_str = |i: usize| -> Option<String> {
        match data.fields().get(i)? {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }
    };
    Some(RestoreRequest {
        kind: field_str(0)?,
        key: field_str(1)?,
        app_id: field_str(2)?,
        title: field_str(3)?,
    })
}

/// Match a restore request against the currently existing sources. Only a
/// live match may skip the picker — a vanished output or closed window
/// falls through to the normal dialog.
fn match_restore(request: &RestoreRequest, sources: &[SourceInfo]) -> Option<Selection> {
    match request.kind.as_str() {
        "output" => sources.iter().find_map(|s| match &s.kind {
            SourceKind::Output(out) if out.name == request.key => {
                Some(Selection::Output(out.clone()))
            }
            _ => None,
        }),
        "toplevel" => {
            let toplevels: Vec<&ToplevelInfo> = sources
                .iter()
                .filter_map(|s| match &s.kind {
                    SourceKind::Toplevel(top) => Some(top),
                    _ => None,
                })
                .collect();
            // Identifier is exact within a compositor run; app_id+title is
            // the cross-restart fallback; a unique app_id match handles
            // title drift (documents, web pages).
            if let Some(top) = toplevels.iter().find(|t| t.identifier == request.key) {
                return Some(Selection::Toplevel((*top).clone()));
            }
            if !request.app_id.is_empty()
                && let Some(top) = toplevels
                    .iter()
                    .find(|t| t.app_id == request.app_id && t.title == request.title)
            {
                return Some(Selection::Toplevel((*top).clone()));
            }
            if !request.app_id.is_empty() {
                let mut by_app = toplevels.iter().filter(|t| t.app_id == request.app_id);
                if let (Some(top), None) = (by_app.next(), by_app.next()) {
                    return Some(Selection::Toplevel((*top).clone()));
                }
            }
            None
        }
        _ => None,
    }
}

impl ScreenCast {
    pub fn new(
        picker: PickerHandle,
        thumbnail_tx: tokio::sync::mpsc::UnboundedSender<ThumbnailUpdate>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                picker,
                sessions: Mutex::new(HashMap::new()),
                streams: Mutex::new(HashMap::new()),
                cursor_visibility: Mutex::new(HashMap::new()),
                persist_modes: Mutex::new(HashMap::new()),
                thumbnail_tx,
            }),
        }
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCast {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        4
    }

    /// We now advertise both MONITOR and WINDOW. WINDOW capture is wired
    /// through the picker but Start for WINDOW currently fails (Phase 5d
    /// migrates the streaming pipeline to ext-image-copy-capture-v1).
    #[zbus(property, name = "AvailableSourceTypes")]
    fn available_source_types(&self) -> u32 {
        source_types::MONITOR | source_types::WINDOW
    }

    /// Both HIDDEN and EMBEDDED — the OBS "Show cursor" checkbox toggles
    /// between these. Compositor honours the choice per session.
    #[zbus(property, name = "AvailableCursorModes")]
    fn available_cursor_modes(&self) -> u32 {
        cursor_modes::HIDDEN | cursor_modes::EMBEDDED
    }

    async fn create_session(
        &self,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(signal_emitter)] _emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        tracing::info!(
            %handle, %session_handle, %app_id, option_keys = ?options.keys().collect::<Vec<_>>(),
            "CreateSession"
        );
        // Export the impl Session object at the session handle so the portal
        // frontend can Close() us — that's how a session's stream gets torn
        // down when the app is done with it.
        let session = SessionImpl {
            session_key: session_handle.to_string(),
            inner: self.inner.clone(),
        };
        server
            .at(&session_handle, session)
            .await
            .map_err(|e| zbus::fdo::Error::Failed(format!("export session object: {e}",)))?;
        Ok((0, HashMap::new()))
    }

    async fn select_sources(
        &self,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let requested = options
            .get("types")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(source_types::MONITOR);
        // OBS sends `cursor_mode` per its "Show cursor" checkbox. EMBEDDED =
        // cursor in the stream, HIDDEN = no cursor. We default to EMBEDDED.
        let cursor_mode = options
            .get("cursor_mode")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(cursor_modes::EMBEDDED);
        let cursor_visible = cursor_mode & cursor_modes::EMBEDDED != 0;
        self.inner
            .cursor_visibility
            .lock()
            .unwrap()
            .insert(session_handle.to_string(), cursor_visible);
        let persist_mode = options
            .get("persist_mode")
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        self.inner
            .persist_modes
            .lock()
            .unwrap()
            .insert(session_handle.to_string(), persist_mode);
        let restore_request = options.get("restore_data").and_then(decode_restore_data);
        tracing::info!(
            %handle, %session_handle, %app_id, requested_types = requested, cursor_mode, cursor_visible,
            "SelectSources: enumerating sources and prompting picker"
        );

        // Spawn the long-lived thumbnail refresh thread; it does initial
        // discovery + first thumbnails, then keeps refreshing each source in
        // round-robin until the returned handle is dropped.
        let (init_tx, init_rx) = tokio::sync::oneshot::channel();
        let thumbnail_tx = self.inner.thumbnail_tx.clone();
        let _stream_guard = tokio::task::spawn_blocking(move || {
            sources::start_thumbnail_stream(init_tx, thumbnail_tx)
        })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("thumbnail thread join: {e}")))?;

        let sources_initial = init_rx.await.unwrap_or_default();
        let filtered: Vec<SourceInfo> = sources_initial
            .into_iter()
            .filter(|s| match &s.kind {
                SourceKind::Output(_) => requested & source_types::MONITOR != 0,
                SourceKind::Toplevel(_) => requested & source_types::WINDOW != 0,
            })
            .collect();
        tracing::info!(count = filtered.len(), "enumerated sources");

        // A valid restore_data blob that still matches a live source skips
        // the picker entirely — this is what keeps Chromium's Go Live (which
        // opens several sessions back-to-back) down to a single dialog.
        if let Some(request) = restore_request {
            if let Some(selection) = match_restore(&request, &filtered) {
                tracing::info!(
                    %session_handle,
                    ?selection,
                    "restore_data matched a live source; skipping picker",
                );
                drop(_stream_guard);
                self.inner
                    .sessions
                    .lock()
                    .unwrap()
                    .insert(session_handle.to_string(), selection);
                return Ok((0, HashMap::new()));
            }
            tracing::info!(
                %session_handle,
                "restore_data no longer matches any source; prompting picker",
            );
        }

        let pick = self.inner.picker.pick(filtered).await;
        drop(_stream_guard);

        match pick {
            PickResult::Source(src) => match src.kind {
                SourceKind::Output(out) => {
                    tracing::info!(?out, %session_handle, "picker: selected output");
                    self.inner
                        .sessions
                        .lock()
                        .unwrap()
                        .insert(session_handle.to_string(), Selection::Output(out));
                    Ok((0, HashMap::new()))
                }
                SourceKind::Toplevel(top) => {
                    tracing::info!(?top, %session_handle, "picker: selected toplevel");
                    self.inner
                        .sessions
                        .lock()
                        .unwrap()
                        .insert(session_handle.to_string(), Selection::Toplevel(top));
                    Ok((0, HashMap::new()))
                }
            },
            PickResult::Cancelled => {
                tracing::info!(%session_handle, "picker: cancelled");
                Ok((1, HashMap::new()))
            }
        }
    }

    async fn start(
        &self,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<(u32, HashMap<String, OwnedValue>)> {
        let selection = self
            .inner
            .sessions
            .lock()
            .unwrap()
            .get(&session_handle.to_string())
            .cloned();
        tracing::info!(%handle, %session_handle, %app_id, %parent_window, ?selection, "Start");

        let session_key = session_handle.to_string();
        let cursor_visible = self
            .inner
            .cursor_visibility
            .lock()
            .unwrap()
            .get(&session_key)
            .copied()
            .unwrap_or(true);
        let persist_mode = self
            .inner
            .persist_modes
            .lock()
            .unwrap()
            .get(&session_key)
            .copied()
            .unwrap_or(0);
        match selection {
            Some(Selection::Output(out)) => {
                let framerate = {
                    let hz = (out.refresh_mhz as f32 / 1000.0).round() as u32;
                    hz.max(30)
                };
                let spec = StreamSpec {
                    output_name: out.name.clone(),
                    width: out.width.max(1) as u32,
                    height: out.height.max(1) as u32,
                    framerate,
                    cursor_visible,
                };
                let spec_for_task = spec.clone();
                let stream_result =
                    tokio::task::spawn_blocking(move || pipewire_stream::start(spec_for_task))
                        .await
                        .map_err(|e| zbus::fdo::Error::Failed(format!("stream task panic: {e}")))?;
                let (node_id, handle_owned) = match stream_result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("pipewire stream failed: {e}");
                        return Ok((2, HashMap::new()));
                    }
                };
                self.inner
                    .streams
                    .lock()
                    .unwrap()
                    .insert(
                        session_key.clone(),
                        AnyStreamHandle::Output(
                            handle_owned,
                        ),
                    );
                // In testing the stream took hundreds of ms to start; the frontend may
                // have Closed the session in that window (Chromium's Go Live
                // does). Returning success would hand the app a node backed
                // by a stream nothing owns — it sits suspended and the app's
                // capture shows 0x0 forever. Verify AFTER inserting: if the
                // session is gone, either cleanup_session already dropped our
                // insert or we drop it here, and the app gets "cancelled".
                if !self.inner.sessions.lock().unwrap().contains_key(&session_key) {
                    drop(
                        self.inner.streams
                            .lock()
                            .unwrap()
                            .remove(
                                &session_key,
                            ),
                    );
                    tracing::warn!(
                        session_key,
                        "session closed during Start; dropped fresh stream and cancelled",
                    );
                    return Ok(
                        (
                            1, 
                            HashMap::new(),
                        ),
                    );
                }

                let mut stream_props: HashMap<String, Value> = HashMap::new();
                stream_props.insert(
                    "size".to_string(),
                    Value::from((spec.width as i32, spec.height as i32)),
                );
                stream_props.insert(
                    "source_type".to_string(),
                    Value::from(source_types::MONITOR),
                );
                let streams: Vec<(u32, HashMap<String, Value>)> = vec![(node_id, stream_props)];
                let mut results = HashMap::new();
                results.insert(
                    "streams".to_string(),
                    OwnedValue::try_from(Value::from(streams)).unwrap(),
                );
                append_restore_results(&mut results, persist_mode, &Selection::Output(out));
                Ok((0, results))
            }
            Some(Selection::Toplevel(top)) => {
                // Compositor's session events advertise the actual buffer
                // dims; we just pass identifier + a target framerate. 60Hz is
                // a reasonable cap regardless of which output the window is
                // currently visible on.
                let spec = ToplevelStreamSpec {
                    toplevel_identifier: top.identifier.clone(),
                    framerate: 60,
                    cursor_visible,
                };
                let spec_for_task = spec.clone();
                let stream_result =
                    tokio::task::spawn_blocking(move || toplevel_stream::start(spec_for_task))
                        .await
                        .map_err(|e| {
                            zbus::fdo::Error::Failed(format!("toplevel stream task panic: {e}"))
                        })?;
                let (node_id, handle_owned) = match stream_result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("toplevel stream failed: {e}");
                        return Ok((2, HashMap::new()));
                    }
                };
                self.inner
                    .streams
                    .lock()
                    .unwrap()
                    .insert(
                        session_key.
                            clone(), 
                        AnyStreamHandle::Toplevel(
                            handle_owned,
                        ),
                    );
                // Same closed-during-Start guard as the Output arm above.
                if !self.inner.sessions.lock().unwrap().contains_key(&session_key) {
                    drop(
                        self.inner.streams
                            .lock()
                            .unwrap()
                            .remove(
                                &session_key,
                            ),
                    );
                    tracing::warn!(
                        session_key,
                        "session closed during Start; dropped fresh stream and cancelled",
                    );
                    return Ok(
                        (
                            1,
                            HashMap::new(),
                        ),
                    );
                }

                let mut stream_props: HashMap<String, Value> = HashMap::new();
                stream_props.insert("source_type".to_string(), Value::from(source_types::WINDOW));
                let streams: Vec<(u32, HashMap<String, Value>)> = vec![(node_id, stream_props)];
                let mut results = HashMap::new();
                results.insert(
                    "streams".to_string(),
                    OwnedValue::try_from(Value::from(streams)).unwrap(),
                );
                append_restore_results(&mut results, persist_mode, &Selection::Toplevel(top));
                Ok((0, results))
            }
            None => {
                tracing::warn!(%session_handle, "Start with no selection — cancelling");
                Ok((1, HashMap::new()))
            }
        }
    }
}
