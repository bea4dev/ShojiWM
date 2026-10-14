//! Drag and drop as the config sees it (`COMPOSITOR.dnd`).
//!
//! A client starts a drag with `wl_data_device.start_drag`; smithay's
//! `DnDGrab` runs the protocol. This module keeps what the compositor draws
//! for it: the drag icon the client hands over (captured into a texture, so
//! it stays drawable after the client destroys it at the drop) and the
//! *views* the config shows with `drag.show()` — the icon, or a decoration
//! tree around it, anchored to the pointer or to a point on the screen, with
//! an opacity and a scale the config animates. A view lives until the config
//! disposes of it, so a drop or cancel can animate; a deadline reclaims views
//! a config forgot.
//!
//! The config hears about a drag through `InputRequest::Dnd` (start, changes
//! of the negotiated action, the target window or the icon, end). A config
//! that does not listen gets the built-in view: the icon at the pointer,
//! gone when the drag ends.

use std::{
    any::Any,
    collections::HashMap,
    os::fd::OwnedFd,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use smithay::{
    input::{
        Seat,
        dnd::{DnDGrab, DndAction, GrabType, Source, SourceMetadata},
        pointer::Focus,
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Point, Rectangle, Serial},
};
use tracing::{debug, warn};

use crate::{
    ssd::{DecorationNode, DecorationTree, StandaloneDecoration, window_model::WindowIconSnapshot},
    state::ShojiWM,
};

/// How long a view may stay after its drag ended without the config
/// disposing of it, unless `show()` asked for another limit.
pub const DEFAULT_MAX_LINGER: Duration = Duration::from_secs(5);

/// The id of the built-in view; views the config shows count from 1.
const BUILTIN_VIEW_ID: u64 = 0;

/// What a drop would do, as negotiated between source and target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DragAction {
    #[default]
    None,
    Copy,
    Move,
    Ask,
}

impl DragAction {
    fn encode(action: DndAction) -> u8 {
        match action {
            DndAction::None => 0,
            DndAction::Copy => 1,
            DndAction::Move => 2,
            DndAction::Ask => 3,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            1 => Self::Copy,
            2 => Self::Move,
            3 => Self::Ask,
            _ => Self::None,
        }
    }
}

/// The actions the source offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct DragActions {
    pub copy: bool,
    #[serde(rename = "move")]
    pub move_: bool,
    pub ask: bool,
}

impl DragActions {
    fn from_metadata(metadata: Option<&SourceMetadata>) -> Self {
        let mut actions = Self::default();
        for action in metadata.map(|metadata| metadata.dnd_actions.as_slice()).unwrap_or_default() {
            match action {
                DndAction::Copy => actions.copy = true,
                DndAction::Move => actions.move_ = true,
                DndAction::Ask => actions.ask = true,
                DndAction::None => {}
            }
        }
        actions
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DragDevice {
    Pointer,
    Touch,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct DragPoint {
    pub x: f64,
    pub y: f64,
}

impl From<Point<f64, Logical>> for DragPoint {
    fn from(point: Point<f64, Logical>) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

/// The client's drag icon: its size, and where its top-left corner sits
/// relative to the pointer (the client's `wl_surface.offset`s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DragIconSnapshot {
    pub width: i32,
    pub height: i32,
    pub offset_x: i32,
    pub offset_y: i32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DragSnapshot {
    pub id: u64,
    pub mime_types: Vec<String>,
    pub actions: DragActions,
    pub action: DragAction,
    pub source_window_id: Option<String>,
    pub target_window_id: Option<String>,
    pub device: DragDevice,
    /// The pointer when the drag started (global logical coordinates).
    pub origin: DragPoint,
    pub icon: Option<DragIconSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DragEndKind {
    /// The button (or touch point) was released, over a target or not.
    Dropped,
    /// The drag ended some other way (the source went away, another grab
    /// took over).
    Cancelled,
}

/// What `InputRequest::Dnd` tells the config.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "phase", rename_all = "camelCase")]
pub enum DndEventSnapshot {
    Start { drag: DragSnapshot },
    /// The negotiated action, the target window or the icon changed.
    #[serde(rename_all = "camelCase")]
    Update {
        drag_id: u64,
        action: DragAction,
        target_window_id: Option<String>,
        icon: Option<DragIconSnapshot>,
    },
    #[serde(rename_all = "camelCase")]
    End {
        drag_id: u64,
        kind: DragEndKind,
        /// The target accepted the data (the drop goes through).
        accepted: bool,
        target_window_id: Option<String>,
        /// The pointer when the drag ended (global logical coordinates).
        position: DragPoint,
    },
}

/// What a view's `x` / `y` are relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DragViewAnchor {
    /// An offset from the pointer, which the compositor follows itself.
    #[default]
    Pointer,
    /// Global logical coordinates.
    Global,
}

/// Where a view's anchor point goes, and how it is drawn. The anchor point
/// is the pointer's spot in the view: the icon keeps the place it has under
/// the pointer, and `scale` grows or shrinks the view around that point.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DragViewProps {
    pub anchor: DragViewAnchor,
    pub x: f64,
    pub y: f64,
    pub opacity: f64,
    pub scale: f64,
}

impl Default for DragViewProps {
    fn default() -> Self {
        Self {
            anchor: DragViewAnchor::Pointer,
            x: 0.0,
            y: 0.0,
            opacity: 1.0,
            scale: 1.0,
        }
    }
}

impl DragViewProps {
    fn sanitized(self) -> Self {
        let finite = |value: f64, fallback: f64| if value.is_finite() { value } else { fallback };
        Self {
            anchor: self.anchor,
            x: finite(self.x, 0.0),
            y: finite(self.y, 0.0),
            opacity: finite(self.opacity, 1.0).clamp(0.0, 1.0),
            scale: finite(self.scale, 1.0).max(0.0),
        }
    }
}

/// `drag.show()` and friends, sent by the config (`HostMessage::DragView`).
#[derive(Debug, Clone)]
pub enum DragViewCommand {
    /// Create view `view_id` of drag `drag_id` (or keep it, when it exists).
    Show {
        view_id: u64,
        drag_id: u64,
        max_linger_ms: Option<u64>,
    },
    Props {
        view_id: u64,
        props: DragViewProps,
    },
    /// The decoration tree drawn around the icon; `None` draws the icon alone.
    Content {
        view_id: u64,
        content: Option<DecorationNode>,
    },
    Dispose {
        view_id: u64,
    },
}

/// A `Source` that records the action the target settles on, so the config
/// can show it.
struct TrackedSource<S> {
    inner: S,
    chosen: Arc<AtomicU8>,
}

impl<S: Source> IsAlive for TrackedSource<S> {
    fn alive(&self) -> bool {
        self.inner.alive()
    }
}

impl<S: Source> Source for TrackedSource<S> {
    fn is_client_local(&self, target: &dyn Any) -> bool {
        self.inner.is_client_local(target)
    }

    fn metadata(&self) -> Option<SourceMetadata> {
        self.inner.metadata()
    }

    fn choose_action(&self, action: DndAction) {
        self.chosen.store(DragAction::encode(action), Ordering::Release);
        self.inner.choose_action(action);
    }

    fn send(&self, mime_type: &str, fd: OwnedFd) {
        self.inner.send(mime_type, fd);
    }

    fn drop_performed(&self) {
        self.inner.drop_performed();
    }

    fn cancel(&self) {
        self.inner.cancel();
    }

    fn finished(&self) {
        self.inner.finished();
    }
}

/// The drag in progress.
#[derive(Debug)]
pub(crate) struct ActiveDrag {
    pub id: u64,
    pub device: DragDevice,
    pub origin: Point<f64, Logical>,
    pub mime_types: Vec<String>,
    pub actions: DragActions,
    chosen: Arc<AtomicU8>,
    pub source_window_id: Option<String>,
    /// The last state the config was told about.
    pub action: DragAction,
    pub target_window_id: Option<String>,
    pub icon: Option<DragIconSnapshot>,
    /// The config listened when the drag started, so it gets the updates and
    /// the end too (a config that starts listening mid-drag does not).
    pub reported: bool,
}

impl ActiveDrag {
    fn snapshot(&self) -> DragSnapshot {
        DragSnapshot {
            id: self.id,
            mime_types: self.mime_types.clone(),
            actions: self.actions,
            action: self.action,
            source_window_id: self.source_window_id.clone(),
            target_window_id: self.target_window_id.clone(),
            device: self.device,
            origin: self.origin.into(),
            icon: self.icon,
        }
    }
}

/// The client's icon of one drag.
#[derive(Debug)]
pub(crate) struct DragIcon {
    pub surface: WlSurface,
    /// Where the surface's top-left corner sits relative to the pointer.
    pub offset: Point<i32, Logical>,
    /// The surface drawn into textures, one per renderer. They keep the last
    /// picture after the client destroys the surface.
    pub captures: Vec<crate::backend::dnd::IconCapture>,
}

impl DragIcon {
    /// The surface tree's bounds relative to its own origin, while the
    /// client keeps it.
    pub fn live_bounds(&self) -> Option<Rectangle<i32, Logical>> {
        self.surface
            .alive()
            .then(|| smithay::desktop::utils::bbox_from_surface_tree(&self.surface, (0, 0)))
    }
}

/// What outlives the drag for its views: the icon and the source window's
/// app icon (for `<AppIcon>`).
#[derive(Debug, Default)]
pub(crate) struct DragRecord {
    pub icon: Option<DragIcon>,
    pub app_icon: Option<WindowIconSnapshot>,
    pub app_id: Option<String>,
}

/// One `drag.show()`.
#[derive(Debug)]
pub(crate) struct DragView {
    pub id: u64,
    pub drag_id: u64,
    pub props: DragViewProps,
    pub content: Option<DragViewContent>,
    pub max_linger: Duration,
    /// Disposed of at this point, once the drag has ended.
    pub deadline: Option<Instant>,
    /// The view drawn into textures, one per renderer and scale.
    pub captures: Vec<crate::backend::dnd::ViewCapture>,
}

#[derive(Debug)]
pub(crate) struct DragViewContent {
    pub tree: DecorationTree,
    /// Bumped with every new tree.
    pub generation: u64,
    /// The tree laid out at each scale it is drawn at.
    pub layouts: Vec<ScaledDragViewLayout>,
}

#[derive(Debug)]
pub(crate) struct ScaledDragViewLayout {
    pub scale: f64,
    pub slot_size: Option<(i32, i32)>,
    pub generation: u64,
    /// Unique per build: also bumped when rasterized labels or images
    /// arrive for the same tree, so captures of the old build are redrawn.
    pub revision: u64,
    pub decoration: StandaloneDecoration,
}

#[derive(Debug, Default)]
pub struct DndState {
    next_drag_id: u64,
    next_generation: u64,
    pub(crate) active: Option<ActiveDrag>,
    pub(crate) records: HashMap<u64, DragRecord>,
    pub(crate) views: Vec<DragView>,
    /// Rasterized labels or images arrived since the views were laid out.
    pub(crate) assets_dirty: bool,
}

impl DndState {
    pub(crate) fn has_views(&self) -> bool {
        !self.views.is_empty()
    }

    /// A view follows the pointer: moving it means redrawing.
    pub(crate) fn has_pointer_anchored_views(&self) -> bool {
        self.views
            .iter()
            .any(|view| view.props.anchor == DragViewAnchor::Pointer)
    }

    /// The live icon surface of the drag in progress.
    pub(crate) fn active_icon_surface(&self) -> Option<&WlSurface> {
        let active = self.active.as_ref()?;
        let icon = self.records.get(&active.id)?.icon.as_ref()?;
        icon.surface.alive().then_some(&icon.surface)
    }

    /// The icon record whose surface is `surface`.
    pub(crate) fn icon_for_surface_mut(&mut self, surface: &WlSurface) -> Option<&mut DragIcon> {
        self.records
            .values_mut()
            .filter_map(|record| record.icon.as_mut())
            .find(|icon| &icon.surface == surface)
    }

    /// Where a view's anchor point is (global logical coordinates).
    pub(crate) fn anchor_point(props: &DragViewProps, pointer: Point<f64, Logical>) -> Point<f64, Logical> {
        match props.anchor {
            DragViewAnchor::Pointer => pointer + Point::from((props.x, props.y)),
            DragViewAnchor::Global => Point::from((props.x, props.y)),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    fn insert_view(&mut self, view_id: u64, drag_id: u64, max_linger: Duration, now: Instant) {
        if self.views.iter().any(|view| view.id == view_id) {
            return;
        }
        // A view shown for a drag that already ended starts its linger now.
        let ended = self.active.as_ref().is_none_or(|active| active.id != drag_id);
        self.views.push(DragView {
            id: view_id,
            drag_id,
            props: DragViewProps::default(),
            content: None,
            max_linger,
            deadline: ended.then(|| now + max_linger),
            captures: Vec::new(),
        });
    }

    /// Apply a command of the config. Returns whether anything visible
    /// changed.
    pub(crate) fn apply(&mut self, command: DragViewCommand, now: Instant) -> bool {
        match command {
            DragViewCommand::Show {
                view_id,
                drag_id,
                max_linger_ms,
            } => {
                if !self.records.contains_key(&drag_id) {
                    debug!(drag_id, view_id, "drag view shown for a drag that is gone");
                    return false;
                }
                let max_linger = max_linger_ms
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_MAX_LINGER);
                self.insert_view(view_id, drag_id, max_linger, now);
                true
            }
            DragViewCommand::Props { view_id, props } => {
                let Some(view) = self.views.iter_mut().find(|view| view.id == view_id) else {
                    return false;
                };
                let props = props.sanitized();
                let changed = view.props != props;
                view.props = props;
                changed
            }
            DragViewCommand::Content { view_id, content } => {
                let generation = self.next_generation();
                let Some(view) = self.views.iter_mut().find(|view| view.id == view_id) else {
                    return false;
                };
                view.content = content.map(|node| DragViewContent {
                    tree: DecorationTree::new(node),
                    generation,
                    layouts: view
                        .content
                        .take()
                        .map(|content| content.layouts)
                        .unwrap_or_default(),
                });
                true
            }
            DragViewCommand::Dispose { view_id } => {
                let before = self.views.len();
                self.views.retain(|view| view.id != view_id);
                let removed = self.views.len() != before;
                self.collect_records();
                removed
            }
        }
    }

    /// Start a drag; returns its id.
    fn start(&mut self, mut drag: ActiveDrag, record: DragRecord) -> u64 {
        self.next_drag_id += 1;
        drag.id = self.next_drag_id;
        let id = drag.id;
        self.records.insert(id, record);
        self.active = Some(drag);
        id
    }

    fn show_builtin(&mut self, drag_id: u64, now: Instant) {
        self.insert_view(BUILTIN_VIEW_ID, drag_id, Duration::ZERO, now);
    }

    /// End the drag in progress: the built-in view goes at once, the
    /// config's views start their linger.
    fn end(&mut self, now: Instant) -> Option<ActiveDrag> {
        let active = self.active.take()?;
        self.views
            .retain(|view| !(view.drag_id == active.id && view.id == BUILTIN_VIEW_ID));
        for view in self.views.iter_mut().filter(|view| view.drag_id == active.id) {
            view.deadline.get_or_insert(now + view.max_linger);
        }
        self.collect_records();
        Some(active)
    }

    /// Drop views past their deadline. Returns whether any went.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let before = self.views.len();
        self.views.retain(|view| {
            let keep = view.deadline.is_none_or(|deadline| now < deadline);
            if !keep {
                warn!(
                    view_id = view.id,
                    drag_id = view.drag_id,
                    "drag view was not disposed of after its drag ended; removing it"
                );
            }
            keep
        });
        let expired = self.views.len() != before;
        if expired {
            self.collect_records();
        }
        expired
    }

    /// The earliest view deadline, to wake up for.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.views.iter().filter_map(|view| view.deadline).min()
    }

    /// Forget the config's views (a reload). The drag in progress keeps the
    /// built-in view, since the new config never heard of it.
    pub(crate) fn reset_views(&mut self, now: Instant) {
        self.views.clear();
        if let Some(active) = self.active.as_mut() {
            active.reported = false;
            let id = active.id;
            self.show_builtin(id, now);
        }
        self.collect_records();
    }

    /// Drop the records no drag or view needs any more.
    fn collect_records(&mut self) {
        let active = self.active.as_ref().map(|active| active.id);
        let views = &self.views;
        self.records.retain(|id, _| {
            Some(*id) == active || views.iter().any(|view| view.drag_id == *id)
        });
    }
}

impl ShojiWM {
    /// `wl_data_device.start_drag` (see `WaylandDndGrabHandler`).
    pub(crate) fn start_drag<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        grab_type: GrabType,
    ) {
        let GrabType::Pointer = grab_type else {
            // The seat has no touch capability, so this does not happen.
            source.cancel();
            return;
        };
        let Some(pointer) = seat.get_pointer() else {
            source.cancel();
            return;
        };
        let Some(start_data) = pointer.grab_start_data() else {
            source.cancel();
            return;
        };

        let metadata = source.metadata();
        let source_window = start_data
            .focus
            .as_ref()
            .and_then(|(surface, _)| self.window_owning_surface(surface));
        let source_snapshot = source_window
            .as_ref()
            .map(|window| self.snapshot_window(window));
        let chosen = Arc::new(AtomicU8::new(DragAction::encode(DndAction::None)));
        let tracked = TrackedSource {
            inner: source,
            chosen: chosen.clone(),
        };

        // A drag still running is cancelled by the new grab, but its end
        // comes later (`queue_end_drag`): end it here, before the new one is
        // registered.
        if let Some(previous) = self.dnd.active.as_ref().map(|active| active.id) {
            let position = pointer.current_location();
            self.end_drag(previous, DragEndKind::Cancelled, false, position);
        }
        let grab = DnDGrab::new_pointer(&self.display_handle, start_data, tracked, seat);
        pointer.set_grab(self, grab, serial, Focus::Keep);

        let now = Instant::now();
        let drag = ActiveDrag {
            id: 0,
            device: DragDevice::Pointer,
            origin: pointer.current_location(),
            mime_types: metadata
                .as_ref()
                .map(|metadata| metadata.mime_types.clone())
                .unwrap_or_default(),
            actions: DragActions::from_metadata(metadata.as_ref()),
            chosen,
            source_window_id: source_snapshot.as_ref().map(|snapshot| snapshot.id.clone()),
            action: DragAction::None,
            target_window_id: None,
            icon: None,
            reported: self.runtime_dnd_enabled,
        };
        let record = DragRecord {
            icon: icon.map(|surface| DragIcon {
                surface,
                offset: Point::default(),
                captures: Vec::new(),
            }),
            app_icon: source_snapshot.as_ref().and_then(|snapshot| snapshot.icon.clone()),
            app_id: source_snapshot.and_then(|snapshot| snapshot.app_id),
        };
        let drag_id = self.dnd.start(drag, record);
        self.refresh_active_drag_fields();
        debug!(drag_id, "drag started");

        if self.runtime_dnd_enabled {
            let drag = self.dnd.active.as_ref().map(ActiveDrag::snapshot);
            if let Some(drag) = drag {
                self.send_dnd_event(DndEventSnapshot::Start { drag });
            }
        } else {
            self.dnd.show_builtin(drag_id, now);
        }
        self.schedule_redraw();
    }

    /// The drag ended (`DndGrabHandler::dropped` / `cancelled`).
    ///
    /// smithay calls these from inside the grab, with the pointer's lock
    /// held: touching the pointer there (its location, a focus change the
    /// config asks for at the end) deadlocks. The drag therefore ends from
    /// the next idle callback.
    pub(crate) fn queue_end_drag(
        &mut self,
        kind: DragEndKind,
        accepted: bool,
        position: Point<f64, Logical>,
    ) {
        let Some(drag_id) = self.dnd.active.as_ref().map(|active| active.id) else {
            return;
        };
        self.loop_handle.insert_idle(move |state| {
            state.end_drag(drag_id, kind, accepted, position);
        });
    }

    /// End drag `drag_id`, unless another drag took its place already.
    fn end_drag(
        &mut self,
        drag_id: u64,
        kind: DragEndKind,
        accepted: bool,
        position: Point<f64, Logical>,
    ) {
        if self.dnd.active.as_ref().is_none_or(|active| active.id != drag_id) {
            return;
        }
        self.sync_drag_state();
        let Some(active) = self.dnd.end(Instant::now()) else {
            return;
        };
        debug!(drag_id = active.id, ?kind, accepted, "drag ended");
        if active.reported {
            self.send_dnd_event(DndEventSnapshot::End {
                drag_id: active.id,
                kind,
                accepted,
                target_window_id: active.target_window_id,
                position: position.into(),
            });
        }
        self.schedule_drag_view_expiry();
        self.schedule_redraw();
    }

    /// Tell the config what changed in the drag in progress (the negotiated
    /// action, the window under the pointer, the icon). Cheap when nothing
    /// did; called after pointer motion and before each frame.
    pub(crate) fn sync_drag_state(&mut self) {
        let Some(active) = self.dnd.active.as_ref() else {
            return;
        };
        let before = (
            active.action,
            active.target_window_id.clone(),
            active.icon,
        );
        self.refresh_active_drag_fields();
        let Some(active) = self.dnd.active.as_ref() else {
            return;
        };
        let after = (
            active.action,
            active.target_window_id.clone(),
            active.icon,
        );
        if before != after && active.reported {
            let event = DndEventSnapshot::Update {
                drag_id: active.id,
                action: active.action,
                target_window_id: active.target_window_id.clone(),
                icon: active.icon,
            };
            self.send_dnd_event(event);
        }
    }

    fn refresh_active_drag_fields(&mut self) {
        let target_window_id = self
            .pointer_contents
            .surface
            .as_ref()
            .and_then(|(surface, _)| self.window_owning_surface(surface))
            .map(|window| self.window_runtime_id(&window));
        let Some(active) = self.dnd.active.as_mut() else {
            return;
        };
        let icon = self
            .dnd
            .records
            .get(&active.id)
            .and_then(|record| record.icon.as_ref())
            .and_then(|icon| {
                let bounds = icon.live_bounds()?;
                Some(DragIconSnapshot {
                    width: bounds.size.w,
                    height: bounds.size.h,
                    offset_x: icon.offset.x,
                    offset_y: icon.offset.y,
                })
            });
        // Keep the last icon the config saw once the client let go of it.
        if icon.is_some() {
            active.icon = icon;
        }
        active.action = DragAction::decode(active.chosen.load(Ordering::Acquire));
        active.target_window_id = target_window_id;
    }

    fn send_dnd_event(&mut self, event: DndEventSnapshot) {
        let now_ms = std::time::Duration::from(self.clock.now()).as_millis() as u64;
        match self.config_runtime.dnd_event(&event, now_ms) {
            Ok(invocation) => self.handle_runtime_pointer_move_invocation(invocation),
            Err(error) => warn!(?error, "runtime drag and drop event failed"),
        }
    }

    /// `HostMessage::DragView`.
    pub(crate) fn apply_drag_view_command(&mut self, command: DragViewCommand) {
        let shown = matches!(command, DragViewCommand::Show { .. });
        if self.dnd.apply(command, Instant::now()) {
            self.schedule_redraw();
            if shown {
                self.schedule_drag_view_expiry();
            }
        }
    }

    /// Wake up when the next lingering view is due, so it goes even when
    /// nothing else redraws.
    fn schedule_drag_view_expiry(&mut self) {
        let Some(deadline) = self.dnd.next_deadline() else {
            return;
        };
        let delay = deadline.saturating_duration_since(Instant::now());
        let timer = smithay::reexports::calloop::timer::Timer::from_duration(delay);
        let inserted = self.loop_handle.insert_source(timer, |_, _, state| {
            if state.dnd.expire(Instant::now()) {
                state.schedule_redraw();
            }
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        });
        if let Err(error) = inserted {
            warn!(?error, "failed to schedule drag view expiry");
        }
    }

    /// The window a surface (or a popup of it) belongs to.
    fn window_owning_surface(&self, surface: &WlSurface) -> Option<smithay::desktop::Window> {
        self.space
            .elements()
            .find(|window| self.surface_belongs_to_window(window, surface))
            .cloned()
    }

    fn window_runtime_id(&self, window: &smithay::desktop::Window) -> String {
        self.window_decorations
            .get(window)
            .map(|decoration| decoration.snapshot.id.clone())
            .unwrap_or_else(|| self.snapshot_window(window).id)
    }

    /// Before each frame: drop expired views, report drag changes and lay
    /// out view contents at the scales they are drawn at.
    pub(crate) fn refresh_drag_views(&mut self) {
        self.sync_drag_state();
        if self.dnd.expire(Instant::now()) {
            self.schedule_redraw();
        }
        if !self.dnd.has_views() {
            self.dnd.assets_dirty = false;
            return;
        }

        let mut scales: Vec<f64> = self
            .space
            .outputs()
            .map(|output| output.current_scale().fractional_scale())
            .collect();
        scales.sort_by(f64::total_cmp);
        scales.dedup();
        let assets_dirty = std::mem::take(&mut self.dnd.assets_dirty);

        let ShojiWM {
            dnd,
            text_rasterizer,
            icon_rasterizer,
            ..
        } = self;
        let DndState {
            records,
            views,
            next_generation,
            ..
        } = dnd;
        let mut rebuilt = false;
        for view in views.iter_mut() {
            let Some(content) = view.content.as_mut() else {
                continue;
            };
            let record = records.get(&view.drag_id);
            let slot_size = record
                .and_then(|record| record.icon.as_ref())
                .and_then(crate::backend::dnd::icon_size);
            let source = crate::ssd::IconSource {
                icon: record.and_then(|record| record.app_icon.as_ref()),
                app_id: record.and_then(|record| record.app_id.as_deref()),
            };
            content
                .layouts
                .retain(|layout| scales.iter().any(|scale| *scale == layout.scale));
            for &scale in &scales {
                let index = content.layouts.iter().position(|layout| layout.scale == scale);
                let current = index.map(|index| &content.layouts[index]);
                if current.is_some_and(|layout| {
                    layout.generation == content.generation
                        && layout.slot_size == slot_size
                        && !assets_dirty
                }) {
                    continue;
                }
                let previous = index.map(|index| content.layouts.remove(index).decoration);
                match StandaloneDecoration::build(
                    &content.tree,
                    slot_size,
                    scale,
                    source,
                    text_rasterizer,
                    icon_rasterizer,
                    previous,
                ) {
                    Ok(decoration) => {
                        *next_generation += 1;
                        content.layouts.push(ScaledDragViewLayout {
                            scale,
                            slot_size,
                            generation: content.generation,
                            revision: *next_generation,
                            decoration,
                        });
                        rebuilt = true;
                    }
                    Err(error) => {
                        warn!(view_id = view.id, ?error, "drag view content cannot be laid out");
                    }
                }
            }
        }
        if rebuilt {
            self.schedule_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(id_hint: u64) -> ActiveDrag {
        ActiveDrag {
            id: id_hint,
            device: DragDevice::Pointer,
            origin: Point::from((10.0, 20.0)),
            mime_types: vec!["text/plain".into()],
            actions: DragActions {
                copy: true,
                move_: true,
                ask: false,
            },
            chosen: Arc::new(AtomicU8::new(0)),
            source_window_id: None,
            action: DragAction::None,
            target_window_id: None,
            icon: None,
            reported: true,
        }
    }

    #[test]
    fn views_linger_after_the_drag_until_disposed_or_expired() {
        let now = Instant::now();
        let mut state = DndState::default();
        let id = state.start(drag(0), DragRecord::default());
        assert!(state.apply(
            DragViewCommand::Show {
                view_id: 1,
                drag_id: id,
                max_linger_ms: Some(100),
            },
            now,
        ));
        assert!(state.views[0].deadline.is_none());

        state.end(now);
        assert_eq!(state.views.len(), 1, "the view outlives its drag");
        assert!(state.records.contains_key(&id), "and keeps its icon");
        assert!(!state.expire(now + Duration::from_millis(50)));
        assert!(state.expire(now + Duration::from_millis(100)));
        assert!(state.views.is_empty());
        assert!(state.records.is_empty());
    }

    #[test]
    fn the_builtin_view_goes_with_its_drag() {
        let now = Instant::now();
        let mut state = DndState::default();
        let id = state.start(drag(0), DragRecord::default());
        state.show_builtin(id, now);
        assert!(state.has_pointer_anchored_views());
        state.end(now);
        assert!(!state.has_views());
    }

    #[test]
    fn dispose_frees_the_record_of_an_ended_drag() {
        let now = Instant::now();
        let mut state = DndState::default();
        let id = state.start(drag(0), DragRecord::default());
        state.apply(
            DragViewCommand::Show {
                view_id: 7,
                drag_id: id,
                max_linger_ms: None,
            },
            now,
        );
        state.end(now);
        assert!(state.apply(DragViewCommand::Dispose { view_id: 7 }, now));
        assert!(state.records.is_empty());
        // Showing a view for a drag that is gone does nothing.
        assert!(!state.apply(
            DragViewCommand::Show {
                view_id: 8,
                drag_id: id,
                max_linger_ms: None,
            },
            now,
        ));
    }

    #[test]
    fn a_reload_keeps_the_running_drag_drawn() {
        let now = Instant::now();
        let mut state = DndState::default();
        let id = state.start(drag(0), DragRecord::default());
        state.apply(
            DragViewCommand::Show {
                view_id: 3,
                drag_id: id,
                max_linger_ms: None,
            },
            now,
        );
        state.reset_views(now);
        assert_eq!(state.views.len(), 1);
        assert_eq!(state.views[0].id, BUILTIN_VIEW_ID);
        assert!(!state.active.as_ref().unwrap().reported);
    }

    #[test]
    fn props_are_sanitized_and_anchor_the_view() {
        let now = Instant::now();
        let mut state = DndState::default();
        let id = state.start(drag(0), DragRecord::default());
        state.apply(
            DragViewCommand::Show {
                view_id: 1,
                drag_id: id,
                max_linger_ms: None,
            },
            now,
        );
        state.apply(
            DragViewCommand::Props {
                view_id: 1,
                props: DragViewProps {
                    anchor: DragViewAnchor::Pointer,
                    x: 4.0,
                    y: f64::NAN,
                    opacity: 2.0,
                    scale: -1.0,
                },
            },
            now,
        );
        let props = state.views[0].props;
        assert_eq!((props.y, props.opacity, props.scale), (0.0, 1.0, 0.0));
        let pointer = Point::from((100.0, 50.0));
        assert_eq!(DndState::anchor_point(&props, pointer), Point::from((104.0, 50.0)));
        let global = DragViewProps {
            anchor: DragViewAnchor::Global,
            x: 7.0,
            y: 8.0,
            ..props
        };
        assert_eq!(DndState::anchor_point(&global, pointer), Point::from((7.0, 8.0)));
    }

    #[test]
    fn events_serialize_for_the_config() {
        let event = DndEventSnapshot::End {
            drag_id: 2,
            kind: DragEndKind::Dropped,
            accepted: false,
            target_window_id: None,
            position: DragPoint { x: 1.0, y: 2.0 },
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["phase"], "end");
        assert_eq!(json["dragId"], 2);
        assert_eq!(json["kind"], "dropped");
        let start = DndEventSnapshot::Start {
            drag: drag(5).snapshot(),
        };
        let json = serde_json::to_value(&start).unwrap();
        assert_eq!(json["drag"]["actions"]["move"], true);
        assert_eq!(json["drag"]["mimeTypes"][0], "text/plain");
        assert_eq!(json["drag"]["device"], "pointer");
    }
}
