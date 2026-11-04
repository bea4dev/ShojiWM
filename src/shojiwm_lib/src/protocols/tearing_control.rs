//! `wp_tearing_control_v1` (tearing-control-v1) global.
//!
//! Lets clients hint, per `wl_surface`, whether tearing (immediate/async page
//! flips) is acceptable for their content — games and similar latency-sensitive
//! apps set the `async` hint. The compositor stores the hint and consults it
//! when deciding whether to submit a tearing flip (see the fullscreen scanout
//! path in `backend::tty`).
//!
//! The global is passive: it only records hints, so there is no handler trait.
//! Read a surface's current preference with [`surface_prefers_tearing`].
//!
//! Presentation hints are double-buffered and take effect with the associated
//! surface commit, including synchronized subsurface transactions.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::{
    wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
    wp_tearing_control_v1::{self, PresentationHint, WpTearingControlV1},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
};
use smithay::wayland::compositor::{Cacheable, with_states};

const VERSION: u32 = 1;

#[cfg(test)]
mod tests;

/// Per-surface tearing-control state, stored in the surface's `data_map`.
#[derive(Debug, Default)]
struct TearingControlSurfaceData {
    /// A `wp_tearing_control_v1` already exists for this surface. The protocol
    /// requires erroring if the client requests a second one.
    has_control: AtomicBool,
}

#[derive(Debug, Default, Clone, Copy)]
struct TearingControlCachedState {
    prefer_tearing: bool,
}

impl Cacheable for TearingControlCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Manager state for the `wp_tearing_control_manager_v1` global.
#[derive(Debug)]
pub struct TearingControlManagerState;

impl TearingControlManagerState {
    /// Create and advertise the `wp_tearing_control_manager_v1` global.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpTearingControlManagerV1, ()>,
        D: Dispatch<WpTearingControlManagerV1, ()>,
        D: Dispatch<WpTearingControlV1, TearingControlData>,
        D: 'static,
    {
        display.create_global::<D, WpTearingControlManagerV1, _>(VERSION, ());
        Self
    }
}

/// Per-`wp_tearing_control_v1` object data: the surface it controls.
#[derive(Debug)]
pub struct TearingControlData {
    surface: WlSurface,
}

/// Returns whether the surface's client hinted that tearing (async page flips)
/// is acceptable for its content (`wp_tearing_control_v1` `async` hint).
///
/// Defaults to `false` (vsync). Hint changes and control destruction take effect
/// on the next applicable surface commit.
pub fn surface_prefers_tearing(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<TearingControlCachedState>()
            .current()
            .prefer_tearing
    })
}

fn with_surface_data<T>(surface: &WlSurface, f: impl FnOnce(&TearingControlSurfaceData) -> T) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(TearingControlSurfaceData::default);
        f(states.data_map.get::<TearingControlSurfaceData>().unwrap())
    })
}

fn reset_surface_hint(surface: &WlSurface) {
    set_pending_hint(surface, false);
    with_surface_data(surface, |data| {
        data.has_control.store(false, Ordering::Relaxed);
    });
}

fn set_pending_hint(surface: &WlSurface, prefer_tearing: bool) {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<TearingControlCachedState>()
            .pending()
            .prefer_tearing = prefer_tearing;
    });
}

impl<D> GlobalDispatch<WpTearingControlManagerV1, (), D> for TearingControlManagerState
where
    D: GlobalDispatch<WpTearingControlManagerV1, ()>,
    D: Dispatch<WpTearingControlManagerV1, ()>,
    D: Dispatch<WpTearingControlV1, TearingControlData>,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<WpTearingControlManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<WpTearingControlManagerV1, (), D> for TearingControlManagerState
where
    D: Dispatch<WpTearingControlManagerV1, ()>,
    D: Dispatch<WpTearingControlV1, TearingControlData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                // The new object must always be initialised to consume the id,
                // even on the error path (the offending client is then killed).
                let already_present = with_surface_data(&surface, |data| {
                    data.has_control.swap(true, Ordering::Relaxed)
                });
                data_init.init(
                    id,
                    TearingControlData {
                        surface: surface.clone(),
                    },
                );
                if already_present {
                    manager.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "wl_surface already has a tearing control",
                    );
                }
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpTearingControlV1, TearingControlData, D> for TearingControlManagerState
where
    D: Dispatch<WpTearingControlV1, TearingControlData>,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _control: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &TearingControlData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                let prefer_tearing = matches!(hint, WEnum::Value(PresentationHint::Async));
                set_pending_hint(&data.surface, prefer_tearing);
            }
            wp_tearing_control_v1::Request::Destroy => {
                reset_surface_hint(&data.surface);
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        _control: &WpTearingControlV1,
        data: &TearingControlData,
    ) {
        // Safety net for clients that drop the object without an explicit
        // destroy request (e.g. on disconnect).
        reset_surface_hint(&data.surface);
    }
}

/// Delegate the `wp_tearing_control` globals to [`TearingControlManagerState`].
#[macro_export]
macro_rules! delegate_tearing_control {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_manager_v1::WpTearingControlManagerV1: ()
        ] => $crate::protocols::tearing_control::TearingControlManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_manager_v1::WpTearingControlManagerV1: ()
        ] => $crate::protocols::tearing_control::TearingControlManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_v1::WpTearingControlV1: $crate::protocols::tearing_control::TearingControlData
        ] => $crate::protocols::tearing_control::TearingControlManagerState);
    };
}
