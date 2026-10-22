//! `COMPOSITOR.input.grab()`: take all keyboard, pointer button, scroll and
//! swipe input until [`InputGrab::release`]. Pointer motion still moves the
//! cursor and is reported too.
//!
//! ```no_run
//! use shojiwm_rs::prelude::*;
//!
//! let grab = COMPOSITOR.input.grab(
//!     InputGrabOptions::new()
//!         .on_key(|event| tracing::info!(key = %event.key, "grabbed key"))
//!         .on_cancel(|reason| tracing::info!(?reason, "grab cancelled")),
//! );
//! grab.release();
//! ```

use std::{cell::RefCell, rc::Rc};

use shojiwm_lib::{
    runtime_api::HostMessage,
    runtime_input_grab::{InputGrabEventSnapshot, RuntimeInputGrabUpdate},
    ssd::{GestureSwipeEventSnapshot, PointerModifierStateSnapshot, PointerMovePointSnapshot},
};

use crate::runtime;

pub use shojiwm_lib::runtime_input_grab::InputGrabStateSnapshot as InputGrabState;

/// A key pressed or released under the grab.
#[derive(Debug, Clone, PartialEq)]
pub struct InputGrabKeyEvent {
    /// xkb keysym name, unshifted Latin layout first, spelled like key
    /// binding shortcuts: `"Tab"`, `"Return"`, `"Escape"`, `"a"`, `"Super_L"`.
    pub key: String,
    pub keycode: u32,
    pub state: InputGrabState,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputGrabPointerMotionEvent {
    /// Global logical position of the cursor.
    pub position: PointerMovePointSnapshot,
    pub delta: PointerMovePointSnapshot,
    pub output_name: Option<String>,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputGrabPointerButtonEvent {
    /// Linux event code (`BTN_LEFT` = 272).
    pub button: u32,
    /// `"left"`, `"right"`, `"middle"`, `"back"` or `"forward"`.
    pub button_name: Option<String>,
    pub state: InputGrabState,
    pub position: PointerMovePointSnapshot,
    pub output_name: Option<String>,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputGrabScrollEvent {
    /// Logical pixels; positive is right / down.
    pub delta_x: f64,
    pub delta_y: f64,
    /// Wheel clicks × 120, when the device reports them.
    pub discrete_x: Option<f64>,
    pub discrete_y: Option<f64>,
    /// `"wheel"`, `"finger"`, `"continuous"` or `"wheelTilt"`.
    pub source: String,
    pub position: PointerMovePointSnapshot,
    pub output_name: Option<String>,
    pub modifiers: PointerModifierStateSnapshot,
    pub timestamp: u64,
}

/// Why a grab ended without [`InputGrab::release`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputGrabCancelReason {
    /// The screen locked.
    SessionLock,
    /// A handler panicked.
    Error,
    /// Another grab replaced it.
    Replaced,
    Other(String),
}

type Handler<E> = Option<Rc<dyn Fn(&E)>>;

/// The handlers of a grab (`COMPOSITOR.input.grab({ ... })`).
#[derive(Clone, Default)]
pub struct InputGrabOptions {
    on_key: Handler<InputGrabKeyEvent>,
    on_pointer_motion: Handler<InputGrabPointerMotionEvent>,
    on_pointer_button: Handler<InputGrabPointerButtonEvent>,
    on_scroll: Handler<InputGrabScrollEvent>,
    on_swipe: Handler<GestureSwipeEventSnapshot>,
    on_cancel: Handler<InputGrabCancelReason>,
}

impl InputGrabOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_key(mut self, f: impl Fn(&InputGrabKeyEvent) + 'static) -> Self {
        self.on_key = Some(Rc::new(f));
        self
    }

    pub fn on_pointer_motion(mut self, f: impl Fn(&InputGrabPointerMotionEvent) + 'static) -> Self {
        self.on_pointer_motion = Some(Rc::new(f));
        self
    }

    pub fn on_pointer_button(mut self, f: impl Fn(&InputGrabPointerButtonEvent) + 'static) -> Self {
        self.on_pointer_button = Some(Rc::new(f));
        self
    }

    pub fn on_scroll(mut self, f: impl Fn(&InputGrabScrollEvent) + 'static) -> Self {
        self.on_scroll = Some(Rc::new(f));
        self
    }

    pub fn on_swipe(mut self, f: impl Fn(&GestureSwipeEventSnapshot) + 'static) -> Self {
        self.on_swipe = Some(Rc::new(f));
        self
    }

    /// The grab ended without `release()`: the screen locked, a handler
    /// panicked, or another grab replaced it.
    pub fn on_cancel(mut self, f: impl Fn(&InputGrabCancelReason) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(f));
        self
    }
}

struct ActiveGrab {
    id: u64,
    options: InputGrabOptions,
}

thread_local! {
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static CURRENT: RefCell<Option<ActiveGrab>> = const { RefCell::new(None) };
}

/// A running grab. Dropping the handle does not release it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputGrab {
    id: u64,
}

impl InputGrab {
    /// Give input back to the clients.
    pub fn release(&self) {
        let released = CURRENT.with(|current| {
            let mut current = current.borrow_mut();
            if current.as_ref().is_some_and(|grab| grab.id == self.id) {
                *current = None;
                true
            } else {
                false
            }
        });
        if released {
            send(self.id, false);
        }
    }

    pub fn is_active(&self) -> bool {
        CURRENT.with(|current| {
            current
                .borrow()
                .as_ref()
                .is_some_and(|grab| grab.id == self.id)
        })
    }
}

fn send(id: u64, active: bool) {
    runtime::send(HostMessage::InputGrab(RuntimeInputGrabUpdate {
        id,
        active,
    }));
}

pub(crate) fn grab(options: InputGrabOptions) -> InputGrab {
    let id = NEXT_ID.with(|next| next.replace(next.get() + 1));
    let previous = CURRENT.with(|current| current.borrow_mut().replace(ActiveGrab { id, options }));
    send(id, true);
    if let Some(on_cancel) = previous.and_then(|previous| previous.options.on_cancel) {
        on_cancel(&InputGrabCancelReason::Replaced);
    }
    InputGrab { id }
}

/// Deliver a grabbed event. Returns whether a handler ran.
pub(crate) fn dispatch(grab_id: u64, event: &InputGrabEventSnapshot) -> bool {
    let options = CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .filter(|grab| grab.id == grab_id)
            .map(|grab| grab.options.clone())
    });
    let Some(options) = options else {
        return false;
    };
    fn call<E>(handler: &Handler<E>, event: E) -> bool {
        match handler {
            Some(handler) => {
                handler(&event);
                true
            }
            None => false,
        }
    }
    match event.clone() {
        InputGrabEventSnapshot::Key {
            key,
            keycode,
            state,
            modifiers,
            timestamp,
        } => call(
            &options.on_key,
            InputGrabKeyEvent {
                key,
                keycode,
                state,
                modifiers,
                timestamp,
            },
        ),
        InputGrabEventSnapshot::PointerMotion {
            position,
            delta,
            output_name,
            modifiers,
            timestamp,
        } => call(
            &options.on_pointer_motion,
            InputGrabPointerMotionEvent {
                position,
                delta,
                output_name,
                modifiers,
                timestamp,
            },
        ),
        InputGrabEventSnapshot::PointerButton {
            button,
            button_name,
            state,
            position,
            output_name,
            modifiers,
            timestamp,
        } => call(
            &options.on_pointer_button,
            InputGrabPointerButtonEvent {
                button,
                button_name,
                state,
                position,
                output_name,
                modifiers,
                timestamp,
            },
        ),
        InputGrabEventSnapshot::Scroll {
            delta_x,
            delta_y,
            discrete_x,
            discrete_y,
            source,
            position,
            output_name,
            modifiers,
            timestamp,
        } => call(
            &options.on_scroll,
            InputGrabScrollEvent {
                delta_x,
                delta_y,
                discrete_x,
                discrete_y,
                source,
                position,
                output_name,
                modifiers,
                timestamp,
            },
        ),
        InputGrabEventSnapshot::Swipe { event } => call(&options.on_swipe, event),
        InputGrabEventSnapshot::Cancel { reason } => {
            CURRENT.with(|current| *current.borrow_mut() = None);
            let reason = match reason.as_str() {
                "sessionLock" => InputGrabCancelReason::SessionLock,
                "error" => InputGrabCancelReason::Error,
                "replaced" => InputGrabCancelReason::Replaced,
                _ => InputGrabCancelReason::Other(reason),
            };
            call(&options.on_cancel, reason);
            true
        }
    }
}

/// Forget the grab (the compositor drops it when the runtime restarts).
pub(crate) fn reset() {
    CURRENT.with(|current| *current.borrow_mut() = None);
}
