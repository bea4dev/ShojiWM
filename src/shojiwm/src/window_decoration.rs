use std::collections::HashMap;

use smithay::{
    desktop::Window,
    reexports::{
        wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as XdgMode,
        wayland_protocols_misc::server_decoration::server::org_kde_kwin_server_decoration::{
            Mode as KdeMode, OrgKdeKwinServerDecoration,
        },
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
};
use tracing::{debug, warn};

use crate::{
    ssd::{
        DecorationEvaluator, WindowDecorationModeSnapshot, WindowDecorationPolicyContextSnapshot,
        WindowDecorationPolicyReasonSnapshot, WindowDecorationProtocolSnapshot,
        WindowDecorationStateSnapshot,
    },
    state::ShojiWM,
};

const LOOP_WARNING_THRESHOLD: u32 = 8;

#[derive(Debug, Clone)]
pub struct WindowDecorationNegotiation {
    protocol: WindowDecorationProtocolSnapshot,
    client_preference: Option<WindowDecorationModeSnapshot>,
    configured_mode: WindowDecorationModeSnapshot,
    effective_mode: WindowDecorationModeSnapshot,
    acked_mode: Option<WindowDecorationModeSnapshot>,
    reason: WindowDecorationPolicyReasonSnapshot,
    policy_dirty: bool,
    has_sent_configure: bool,
    duplicate_request_count: u32,
    loop_warning_logged: bool,
    kde_resource: Option<OrgKdeKwinServerDecoration>,
}

impl WindowDecorationNegotiation {
    fn new(protocol: WindowDecorationProtocolSnapshot) -> Self {
        Self {
            protocol,
            client_preference: None,
            configured_mode: WindowDecorationModeSnapshot::Server,
            effective_mode: WindowDecorationModeSnapshot::Server,
            acked_mode: None,
            reason: WindowDecorationPolicyReasonSnapshot::Initial,
            policy_dirty: true,
            has_sent_configure: false,
            duplicate_request_count: 0,
            loop_warning_logged: false,
            kde_resource: None,
        }
    }

    fn snapshot(&self) -> WindowDecorationStateSnapshot {
        WindowDecorationStateSnapshot {
            protocol: self.protocol,
            client_preference: self.client_preference,
            configured_mode: self.configured_mode,
            mode: self.effective_mode,
        }
    }

    fn context(&self) -> WindowDecorationPolicyContextSnapshot {
        WindowDecorationPolicyContextSnapshot {
            protocol: self.protocol,
            client_preference: self.client_preference,
            can_negotiate: matches!(
                self.protocol,
                WindowDecorationProtocolSnapshot::XdgDecorationV1
                    | WindowDecorationProtocolSnapshot::KdeServerDecoration
            ),
            reason: self.reason,
        }
    }
}

impl ShojiWM {
    pub fn window_decoration_snapshot(
        &self,
        surface: &WlSurface,
        is_xwayland: bool,
    ) -> WindowDecorationStateSnapshot {
        self.window_decoration_negotiations
            .get(surface)
            .map(WindowDecorationNegotiation::snapshot)
            .unwrap_or_else(|| WindowDecorationStateSnapshot {
                protocol: if is_xwayland {
                    WindowDecorationProtocolSnapshot::Xwayland
                } else {
                    WindowDecorationProtocolSnapshot::None
                },
                ..WindowDecorationStateSnapshot::default()
            })
    }

    pub fn note_xdg_decoration_created(&mut self, surface: &WlSurface) {
        self.note_window_decoration_request(
            surface,
            WindowDecorationProtocolSnapshot::XdgDecorationV1,
            None,
            WindowDecorationPolicyReasonSnapshot::Initial,
            None,
        );
    }

    pub fn ensure_wayland_window_decoration_policy(&mut self, surface: &WlSurface) {
        self.window_decoration_negotiations
            .entry(surface.clone())
            .or_insert_with(|| {
                WindowDecorationNegotiation::new(WindowDecorationProtocolSnapshot::None)
            });
        self.resolve_window_decoration_policy_for_surface(surface);
    }

    pub fn note_xdg_decoration_request(
        &mut self,
        surface: &WlSurface,
        preference: Option<WindowDecorationModeSnapshot>,
        reason: WindowDecorationPolicyReasonSnapshot,
    ) {
        self.note_window_decoration_request(
            surface,
            WindowDecorationProtocolSnapshot::XdgDecorationV1,
            preference,
            reason,
            None,
        );
    }

    pub fn note_kde_decoration_created(
        &mut self,
        surface: &WlSurface,
        resource: &OrgKdeKwinServerDecoration,
    ) {
        self.note_window_decoration_request(
            surface,
            WindowDecorationProtocolSnapshot::KdeServerDecoration,
            None,
            WindowDecorationPolicyReasonSnapshot::Initial,
            Some(resource.clone()),
        );
    }

    pub fn note_kde_decoration_request(
        &mut self,
        surface: &WlSurface,
        resource: &OrgKdeKwinServerDecoration,
        preference: WindowDecorationModeSnapshot,
    ) {
        self.note_window_decoration_request(
            surface,
            WindowDecorationProtocolSnapshot::KdeServerDecoration,
            Some(preference),
            WindowDecorationPolicyReasonSnapshot::ClientRequest,
            Some(resource.clone()),
        );
    }

    fn note_window_decoration_request(
        &mut self,
        surface: &WlSurface,
        protocol: WindowDecorationProtocolSnapshot,
        preference: Option<WindowDecorationModeSnapshot>,
        reason: WindowDecorationPolicyReasonSnapshot,
        kde_resource: Option<OrgKdeKwinServerDecoration>,
    ) {
        let entry = self
            .window_decoration_negotiations
            .entry(surface.clone())
            .or_insert_with(|| WindowDecorationNegotiation::new(protocol));

        if entry.protocol == protocol
            && entry.client_preference == preference
            && !entry.policy_dirty
            && entry.has_sent_configure
        {
            if protocol != WindowDecorationProtocolSnapshot::KdeServerDecoration {
                let mode = entry.configured_mode;
                self.send_window_decoration_mode(surface, protocol, mode, None);
                return;
            }

            entry.duplicate_request_count = entry.duplicate_request_count.saturating_add(1);
            if entry.duplicate_request_count >= LOOP_WARNING_THRESHOLD && !entry.loop_warning_logged
            {
                entry.loop_warning_logged = true;
                warn!(
                    surface = ?surface.id(),
                    ?protocol,
                    ?preference,
                    configured_mode = ?entry.configured_mode,
                    duplicate_requests = entry.duplicate_request_count,
                    "suppressed repeated window decoration request; possible client/compositor request loop"
                );
            }
            if entry.duplicate_request_count < LOOP_WARNING_THRESHOLD {
                let protocol = entry.protocol;
                let mode = entry.configured_mode;
                let kde_resource = entry.kde_resource.clone();
                self.send_window_decoration_mode(surface, protocol, mode, kde_resource);
            }
            return;
        }

        if entry.protocol != protocol {
            entry.protocol = protocol;
            entry.has_sent_configure = false;
            entry.acked_mode = None;
        }
        entry.client_preference = preference;
        entry.reason = reason;
        entry.policy_dirty = true;
        entry.duplicate_request_count = 0;
        entry.loop_warning_logged = false;
        if let Some(resource) = kde_resource {
            entry.kde_resource = Some(resource);
        }

        self.resolve_window_decoration_policy_for_surface(surface);
    }

    pub fn mark_window_decoration_metadata_changed(&mut self, surface: &WlSurface) {
        if let Some(entry) = self.window_decoration_negotiations.get_mut(surface) {
            entry.reason = WindowDecorationPolicyReasonSnapshot::MetadataChanged;
            entry.policy_dirty = true;
            entry.duplicate_request_count = 0;
            entry.loop_warning_logged = false;
            self.resolve_window_decoration_policy_for_surface(surface);
        }
    }

    pub fn mark_all_window_decoration_policies_reloaded(&mut self) {
        let surfaces = self
            .window_decoration_negotiations
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for surface in surfaces {
            if let Some(entry) = self.window_decoration_negotiations.get_mut(&surface) {
                entry.reason = WindowDecorationPolicyReasonSnapshot::Reload;
                entry.policy_dirty = true;
                entry.duplicate_request_count = 0;
                entry.loop_warning_logged = false;
            }
            self.resolve_window_decoration_policy_for_surface(&surface);
        }
    }

    pub fn resolve_window_decoration_policy_for_surface(&mut self, surface: &WlSurface) {
        let Some(window) = self
            .space
            .elements()
            .find(|window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == surface)
            })
            .cloned()
        else {
            return;
        };
        self.resolve_window_decoration_policy(&window);
    }

    pub fn resolve_window_decoration_policy(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let surface = toplevel.wl_surface().clone();
        let Some(context) = self
            .window_decoration_negotiations
            .get(&surface)
            .filter(|entry| entry.policy_dirty)
            .map(WindowDecorationNegotiation::context)
        else {
            return;
        };
        let snapshot = self.snapshot_window(window);
        let decision = match self
            .decoration_evaluator
            .window_decoration_policy(&snapshot, &context)
        {
            Ok(decision) => decision,
            Err(error) => {
                let mode = self
                    .window_decoration_negotiations
                    .get(&surface)
                    .map(|entry| entry.configured_mode)
                    .unwrap_or(WindowDecorationModeSnapshot::Server);
                warn!(
                    window_id = %snapshot.id,
                    title = %snapshot.title,
                    app_id = ?snapshot.app_id,
                    ?error,
                    fallback_mode = ?mode,
                    "window decoration policy failed; applying the previous mode"
                );
                crate::ssd::WindowDecorationDecisionSnapshot { mode }
            }
        };

        let (protocol, changed, first_configure, kde_resource) = {
            let entry = self
                .window_decoration_negotiations
                .get_mut(&surface)
                .expect("decoration negotiation disappeared during policy evaluation");
            let changed = entry.configured_mode != decision.mode;
            let first_configure = !entry.has_sent_configure;
            entry.configured_mode = decision.mode;
            entry.policy_dirty = false;
            entry.has_sent_configure = true;
            entry.duplicate_request_count = 0;
            entry.loop_warning_logged = false;
            (
                entry.protocol,
                changed,
                first_configure,
                entry.kde_resource.clone(),
            )
        };

        if !changed && !first_configure {
            // The effective mode may be unchanged while the client's request
            // changed. Publish that context to the reactive TS snapshot too.
            self.runtime_node_only_window_ids.remove(&snapshot.id);
            self.runtime_dirty_window_ids.insert(snapshot.id);
            self.runtime_poll_dirty = true;
            self.request_tty_maintenance("window-decoration-context");
            self.schedule_redraw();
            return;
        }

        self.send_window_decoration_mode(&surface, protocol, decision.mode, kde_resource);

        debug!(
            window_id = %snapshot.id,
            ?protocol,
            client_preference = ?context.client_preference,
            configured_mode = ?decision.mode,
            "applied window decoration policy"
        );
        self.runtime_node_only_window_ids.remove(&snapshot.id);
        self.runtime_dirty_window_ids.insert(snapshot.id);
        self.runtime_poll_dirty = true;
        self.request_tty_maintenance("window-decoration-policy");
        self.schedule_redraw();
    }

    fn send_window_decoration_mode(
        &mut self,
        surface: &WlSurface,
        protocol: WindowDecorationProtocolSnapshot,
        mode: WindowDecorationModeSnapshot,
        kde_resource: Option<OrgKdeKwinServerDecoration>,
    ) {
        match protocol {
            WindowDecorationProtocolSnapshot::XdgDecorationV1 => {
                let Some(toplevel) = self.space.elements().find_map(|window| {
                    window
                        .toplevel()
                        .filter(|toplevel| toplevel.wl_surface() == surface)
                        .cloned()
                }) else {
                    return;
                };
                toplevel.with_pending_state(|pending| {
                    pending.decoration_mode = Some(match mode {
                        WindowDecorationModeSnapshot::Client => XdgMode::ClientSide,
                        WindowDecorationModeSnapshot::Server => XdgMode::ServerSide,
                    });
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
            WindowDecorationProtocolSnapshot::KdeServerDecoration => {
                if let Some(resource) = kde_resource {
                    resource.mode(match mode {
                        WindowDecorationModeSnapshot::Client => KdeMode::Client,
                        WindowDecorationModeSnapshot::Server => KdeMode::Server,
                    });
                }
                if let Some(entry) = self.window_decoration_negotiations.get_mut(surface) {
                    entry.effective_mode = mode;
                }
            }
            WindowDecorationProtocolSnapshot::Xwayland | WindowDecorationProtocolSnapshot::None => {
                if let Some(entry) = self.window_decoration_negotiations.get_mut(surface) {
                    entry.effective_mode = mode;
                }
            }
        }
    }

    pub fn acknowledge_xdg_decoration_mode(&mut self, surface: &WlSurface, mode: Option<XdgMode>) {
        let Some(entry) = self.window_decoration_negotiations.get_mut(surface) else {
            return;
        };
        entry.acked_mode = mode.map(|mode| match mode {
            XdgMode::ClientSide => WindowDecorationModeSnapshot::Client,
            XdgMode::ServerSide => WindowDecorationModeSnapshot::Server,
            _ => entry.configured_mode,
        });
    }

    pub fn commit_xdg_decoration_mode(&mut self, surface: &WlSurface) {
        let Some(entry) = self.window_decoration_negotiations.get_mut(surface) else {
            return;
        };
        let Some(mode) = entry.acked_mode.take() else {
            return;
        };
        if entry.effective_mode == mode {
            return;
        }
        entry.effective_mode = mode;
        if let Some(window_id) = self.space.elements().find_map(|window| {
            window
                .toplevel()
                .filter(|toplevel| toplevel.wl_surface() == surface)
                .map(|_| self.snapshot_window(window).id)
        }) {
            self.runtime_node_only_window_ids.remove(&window_id);
            self.runtime_dirty_window_ids.insert(window_id);
        }
        self.runtime_poll_dirty = true;
        self.request_tty_maintenance("window-decoration-commit");
        self.schedule_redraw();
    }

    pub fn remove_window_decoration_negotiation(&mut self, surface: &WlSurface) {
        self.window_decoration_negotiations.remove(surface);
    }

    /// Removes all per-window state that would otherwise leak once a window
    /// closes: decoration state (including its cached backdrop textures,
    /// nested in `WindowDecorationState`), per-output tracking, and commit
    /// timing. None of these are pruned by unmapping the window from the
    /// `Space` alone. `closing_window_snapshots` is handled separately, via
    /// `WaylandWindowAction::FinalizeClose` once the close animation itself
    /// finishes.
    pub fn prune_window_state(&mut self, window: &Window) {
        let window_id = self
            .window_decorations
            .get(window)
            .map(|decoration| decoration.snapshot.id.clone());
        self.window_decorations.remove(window);
        self.window_primary_output_names.remove(window);
        self.window_commit_times.remove(window);
        if let Some(window_id) = window_id {
            self.prune_window_shader_pipeline_caches(&window_id);
        }
    }

    pub fn prune_window_shader_pipeline_caches(&mut self, window_id: &str) {
        crate::backend::shader_effect::purge_shared_effect_pipeline_caches_for_window(window_id);
    }

    /// Drops a captured `live_window_snapshots` entry (a real `GlesTexture`)
    /// left behind when a window closes without ever being promoted to
    /// `closing_window_snapshots` — either because no close animation was
    /// invoked for it, or because the caller (X11 unmap) never attempts a
    /// promotion at all. Once a window IS promoted, its snapshot lives on in
    /// `closing_window_snapshots` instead and is cleaned up separately via
    /// `WaylandWindowAction::FinalizeClose`, so this must only run when that
    /// promotion did not happen.
    pub fn prune_unpromoted_window_snapshot(&mut self, window_id: &str) {
        if self.closing_window_snapshots.contains_key(window_id) {
            return;
        }
        self.prune_window_shader_pipeline_caches(window_id);
        self.live_window_snapshots.remove(window_id);
        self.live_window_snapshot_trackers.remove(window_id);
        self.complete_window_snapshots.remove(window_id);
        self.complete_window_snapshot_trackers.remove(window_id);
    }
}

pub type WindowDecorationNegotiationMap = HashMap<WlSurface, WindowDecorationNegotiation>;
