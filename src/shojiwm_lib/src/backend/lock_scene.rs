//! Explicit, runtime-owned backgrounds for transparent ext-session-lock surfaces.
use std::{
    collections::HashMap,
    io,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;
use smithay::{
    backend::renderer::utils::with_renderer_surface_state,
    backend::renderer::{ImportAll, Renderer, element::surface::WaylandSurfaceRenderElement},
    desktop::{Space, Window},
    output::Output,
    utils::{Logical, Point, Scale},
    wayland::session_lock::LockSurface,
};

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundSelection {
    pub window_ids: Vec<String>,
    pub layer_ids: Vec<String>,
}

impl BackgroundSelection {
    fn validate(&self) -> io::Result<()> {
        if self.window_ids.len() + self.layer_ids.len() > 64
            || self
                .window_ids
                .iter()
                .chain(&self.layer_ids)
                .any(|id| id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        {
            return Err(io::Error::other(
                "Lock backgrounds require at most 64 nonempty runtime IDs, each <= 512 bytes",
            ));
        }
        Ok(())
    }
}

struct OwnedSelection {
    owner: Weak<AtomicBool>,
    background: BackgroundSelection,
}

impl OwnedSelection {
    fn current(&self) -> BackgroundSelection {
        if self
            .owner
            .upgrade()
            .is_some_and(|owner| owner.load(Ordering::Acquire))
        {
            self.background.clone()
        } else {
            BackgroundSelection::default()
        }
    }
}

/// `ShojiWM::session_lock_active` for the runtime threads; written only by
/// `ShojiWM::set_session_lock_active` (and the constructor).
static LOCKED: AtomicBool = AtomicBool::new(false);
#[derive(Default)]
struct Selections {
    current: Option<OwnedSelection>,
    pending: Option<OwnedSelection>,
}
static BACKGROUND: OnceLock<Mutex<Selections>> = OnceLock::new();
static CHANGED: AtomicBool = AtomicBool::new(false);

fn selections() -> &'static Mutex<Selections> {
    BACKGROUND.get_or_init(|| Mutex::new(Selections::default()))
}

/// Apply only at a frame boundary, before collecting GPU buffer holds.
pub(crate) fn apply_pending() {
    let mut slot = selections().lock().unwrap();
    if let Some(next) = slot.pending.take() {
        slot.current = Some(next);
    }
}

pub fn is_locked() -> bool {
    LOCKED.load(Ordering::Acquire)
}

pub(crate) fn set_locked(locked: bool) {
    LOCKED.store(locked, Ordering::Release);
}

pub(crate) fn locker_ready(surface: &LockSurface) -> bool {
    surface.alive()
        && with_renderer_surface_state(surface.wl_surface(), |state| state.buffer().is_some())
            .unwrap_or(false)
}

pub(crate) fn window_selected(window: &Window) -> bool {
    selections()
        .lock()
        .unwrap()
        .current
        .as_ref()
        .is_some_and(|selection| {
            selection
                .owner
                .upgrade()
                .is_some_and(|owner| owner.load(Ordering::Acquire))
                && selection
                    .background
                    .window_ids
                    .contains(&format!("0x{:x}", window.id()))
        })
}

/// Returns whether the selection changed; this never locks or unlocks.
pub fn configure(owner: &Arc<AtomicBool>, background: BackgroundSelection) -> io::Result<bool> {
    background.validate()?;
    if !owner.load(Ordering::Acquire) {
        return Err(io::Error::other("Lock background runtime is inactive"));
    }
    let mut slot = selections().lock().unwrap();
    let owner = Arc::downgrade(owner);
    if slot
        .pending
        .as_ref()
        .or(slot.current.as_ref())
        .is_some_and(|current| {
            Weak::ptr_eq(&current.owner, &owner) && current.background == background
        })
    {
        return Ok(false);
    }
    slot.pending = Some(OwnedSelection { owner, background });
    CHANGED.store(true, Ordering::Release);
    Ok(true)
}

/// A selection changed since the last call. The compositor then redraws: on
/// TTY nothing else may (a static lock screen and wallpaper do not commit), so
/// the pending selection would otherwise wait for unrelated damage.
pub(crate) fn take_changed() -> bool {
    CHANGED.swap(false, Ordering::AcqRel)
}

/// Root surface trees only: no separately managed popups, SSDs or desktop scene.
/// Call only when a live locker surface exists on this output.
pub(crate) fn background_elements<R>(
    renderer: &mut R,
    space: &Space<Window>,
    decorations: &HashMap<Window, crate::ssd::WindowDecorationState>,
    output: &Output,
    scale: Scale<f64>,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let background = selections()
        .lock()
        .unwrap()
        .current
        .as_ref()
        .map(OwnedSelection::current)
        .unwrap_or_default();
    let mut elements = Vec::new();
    let Some(output_geo) = space.output_geometry(output) else {
        return elements;
    };
    // Keep front-to-back space order for multiple selected windows.
    for window in space.elements().rev() {
        if !background
            .window_ids
            .contains(&format!("0x{:x}", window.id()))
        {
            continue;
        }
        let location = decorations
            .get(window)
            .map(|decoration| {
                Point::<i32, Logical>::from((decoration.client_rect.x, decoration.client_rect.y))
            })
            .or_else(|| space.element_location(window));
        if let Some(location) = location {
            elements.extend(super::window::surface_elements(
                window,
                renderer,
                (location - output_geo.loc).to_physical_precise_round(scale),
                scale,
                1.0,
            ));
        }
    }
    // The helper releases LayerMap's non-reentrant mutex before root rendering.
    let (upper, lower) = super::window::layer_surfaces_for_output(output);
    for layer in upper.into_iter().chain(lower) {
        if background
            .layer_ids
            .contains(&crate::ssd::layer_runtime_id(&layer))
        {
            elements.extend(super::window::layer_surface_root_elements(
                renderer, output, &layer, scale, 1.0,
            ));
        }
    }
    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ids_and_drops_background_when_runtime_stops() {
        let owner = Arc::new(AtomicBool::new(true));
        let background = BackgroundSelection {
            window_ids: vec!["0x1".into()],
            layer_ids: vec![],
        };
        assert!(background.validate().is_ok());
        let selection = OwnedSelection {
            owner: Arc::downgrade(&owner),
            background: background.clone(),
        };
        assert_eq!(selection.current(), background);
        owner.store(false, Ordering::Release);
        assert_eq!(selection.current(), BackgroundSelection::default());
        drop(owner);
        assert_eq!(selection.current(), BackgroundSelection::default());
        for id in ["", "bad\nid"] {
            assert!(
                BackgroundSelection {
                    window_ids: vec![id.into()],
                    layer_ids: vec![]
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            BackgroundSelection {
                window_ids: vec!["x".repeat(513)],
                layer_ids: vec![]
            }
            .validate()
            .is_err()
        );
        assert!(
            BackgroundSelection {
                window_ids: vec!["x".into(); 65],
                layer_ids: vec![]
            }
            .validate()
            .is_err()
        );
        assert!(serde_json::from_str::<BackgroundSelection>(r#"{"appId":"anything"}"#).is_err());
    }

    // The only test touching the process-wide selection.
    #[test]
    fn changed_selections_request_one_redraw_and_apply_at_the_frame_boundary() {
        let owner = Arc::new(AtomicBool::new(true));
        let background = BackgroundSelection {
            window_ids: vec!["0x1".into()],
            layer_ids: vec![],
        };
        take_changed();
        assert!(configure(&owner, background.clone()).unwrap());
        assert!(take_changed());
        assert!(!take_changed());
        // Unchanged (still pending, then applied): no new redraw.
        assert!(!configure(&owner, background.clone()).unwrap());
        apply_pending();
        assert!(!configure(&owner, background.clone()).unwrap());
        assert!(!take_changed());
        assert_eq!(
            selections()
                .lock()
                .unwrap()
                .current
                .as_ref()
                .map(OwnedSelection::current),
            Some(background)
        );
        owner.store(false, Ordering::Release);
        assert!(configure(&owner, BackgroundSelection::default()).is_err());
        assert!(!take_changed());
    }
}
