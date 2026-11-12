//! Input that does not come from a device: `zwlr_virtual_pointer_v1`
//! clients (`wlrctl pointer`, remote desktop servers) and the control
//! socket's `key` command.
//!
//! Both are fed to [`ShojiWM::process_input_event`] as an input backend of
//! their own, so they take the same path as libinput events: pointer
//! constraints, input grabs, the config's key binds and pointer handlers.
//! `zwp_virtual_keyboard_v1` clients come through here too, with a keymap
//! of their own (see [`crate::virtual_keyboard`]).

use std::time::Duration;

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
    DeviceCapability, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, Keycode,
    PointerAxisEvent, PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent,
    UnusedEvent,
};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::utils::{Logical, Rectangle};
use xkbcommon::xkb;

use crate::state::ShojiWM;

#[derive(Debug)]
pub struct VirtualInput;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VirtualDevice {
    pub id: String,
    pub name: String,
    pub keyboard: bool,
}

impl Device for VirtualDevice {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn has_capability(&self, capability: DeviceCapability) -> bool {
        match capability {
            DeviceCapability::Keyboard => self.keyboard,
            DeviceCapability::Pointer => !self.keyboard,
            _ => false,
        }
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }

    fn syspath(&self) -> Option<std::path::PathBuf> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct VirtualKey {
    pub device: VirtualDevice,
    pub time: u64,
    pub keycode: Keycode,
    pub state: KeyState,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct VirtualMotion {
    pub device: VirtualDevice,
    pub time: u64,
    pub dx: f64,
    pub dy: f64,
}

/// An absolute position as a fraction (0..=1) of the whole output layout,
/// which is the space `PointerMotionAbsolute` is mapped onto.
#[derive(Debug, Clone)]
pub struct VirtualMotionAbsolute {
    pub device: VirtualDevice,
    pub time: u64,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone)]
pub struct VirtualButton {
    pub device: VirtualDevice,
    pub time: u64,
    pub button: u32,
    pub state: ButtonState,
}

#[derive(Debug, Clone)]
pub struct VirtualAxis {
    pub device: VirtualDevice,
    pub time: u64,
    pub frame: AxisFrame,
}

macro_rules! virtual_event {
    ($($ty:ty),*) => {$(
        impl Event<VirtualInput> for $ty {
            fn time(&self) -> u64 {
                self.time
            }

            fn device(&self) -> VirtualDevice {
                self.device.clone()
            }
        }
    )*};
}

virtual_event!(
    VirtualKey,
    VirtualMotion,
    VirtualMotionAbsolute,
    VirtualButton,
    VirtualAxis
);

impl KeyboardKeyEvent<VirtualInput> for VirtualKey {
    fn key_code(&self) -> Keycode {
        self.keycode
    }

    fn state(&self) -> KeyState {
        self.state
    }

    fn count(&self) -> u32 {
        self.count
    }
}

impl PointerMotionEvent<VirtualInput> for VirtualMotion {
    fn delta_x(&self) -> f64 {
        self.dx
    }

    fn delta_y(&self) -> f64 {
        self.dy
    }

    fn delta_x_unaccel(&self) -> f64 {
        self.dx
    }

    fn delta_y_unaccel(&self) -> f64 {
        self.dy
    }
}

impl AbsolutePositionEvent<VirtualInput> for VirtualMotionAbsolute {
    fn x(&self) -> f64 {
        self.x
    }

    fn y(&self) -> f64 {
        self.y
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.x * width as f64
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.y * height as f64
    }
}

impl PointerMotionAbsoluteEvent<VirtualInput> for VirtualMotionAbsolute {}

impl PointerButtonEvent<VirtualInput> for VirtualButton {
    fn button_code(&self) -> u32 {
        self.button
    }

    fn state(&self) -> ButtonState {
        self.state
    }
}

impl PointerAxisEvent<VirtualInput> for VirtualAxis {
    fn amount(&self, axis: Axis) -> Option<f64> {
        self.frame.axis(axis).value
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        self.frame.v120(axis)
    }

    fn source(&self) -> AxisSource {
        self.frame.source.unwrap_or(AxisSource::Wheel)
    }

    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

impl InputBackend for VirtualInput {
    type Device = VirtualDevice;
    type KeyboardKeyEvent = VirtualKey;
    type PointerAxisEvent = VirtualAxis;
    type PointerButtonEvent = VirtualButton;
    type PointerMotionEvent = VirtualMotion;
    type PointerMotionAbsoluteEvent = VirtualMotionAbsolute;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AxisAmount {
    pub value: Option<f64>,
    pub v120: Option<f64>,
}

/// The axis requests of one `zwlr_virtual_pointer_v1` frame, which the
/// protocol delivers as separate requests and smithay takes as one event.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AxisFrame {
    pub time_msec: u32,
    pub source: Option<AxisSource>,
    pub horizontal: AxisAmount,
    pub vertical: AxisAmount,
}

impl AxisFrame {
    pub fn axis(&self, axis: Axis) -> AxisAmount {
        match axis {
            Axis::Horizontal => self.horizontal,
            Axis::Vertical => self.vertical,
        }
    }

    fn axis_mut(&mut self, axis: Axis) -> &mut AxisAmount {
        match axis {
            Axis::Horizontal => &mut self.horizontal,
            Axis::Vertical => &mut self.vertical,
        }
    }

    /// Wheel steps in 120ths. A frame with no `axis_source` and no
    /// `axis_discrete` is taken as a wheel at 15 per notch, so clients see the
    /// notches (`axis_value120`) a wheel would send rather than a smooth scroll.
    pub fn v120(&self, axis: Axis) -> Option<f64> {
        let amount = self.axis(axis);
        match (amount.v120, self.source) {
            (Some(v120), _) => Some(v120),
            (None, None) => amount.value.map(|value| value * 120.0 / 15.0),
            (None, Some(_)) => None,
        }
    }

    pub fn add(&mut self, axis: Axis, value: f64) {
        let amount = self.axis_mut(axis);
        amount.value = Some(amount.value.unwrap_or(0.0) + value);
    }

    /// `axis_discrete` carries both the pixel value and the wheel steps.
    pub fn add_discrete(&mut self, axis: Axis, value: f64, steps: i32) {
        self.add(axis, value);
        let amount = self.axis_mut(axis);
        amount.v120 = Some(amount.v120.unwrap_or(0.0) + f64::from(steps) * 120.0);
    }

    /// An `axis_stop` is a zero-length scroll on that axis, which smithay
    /// turns into `wl_pointer.axis_stop`.
    pub fn stop(&mut self, axis: Axis) {
        let amount = self.axis_mut(axis);
        amount.value.get_or_insert(0.0);
    }

    pub fn is_empty(&self) -> bool {
        self.horizontal == AxisAmount::default() && self.vertical == AxisAmount::default()
    }
}

/// Where `motion_absolute` lands, as a fraction of `layout`. A pointer
/// created for one output spans that output; otherwise it spans the layout.
pub fn absolute_fraction(
    x: u32,
    y: u32,
    x_extent: u32,
    y_extent: u32,
    output: Option<Rectangle<i32, Logical>>,
    layout: Rectangle<i32, Logical>,
) -> Option<(f64, f64)> {
    if x_extent == 0 || y_extent == 0 || layout.size.w <= 0 || layout.size.h <= 0 {
        return None;
    }
    let fx = f64::from(x.min(x_extent)) / f64::from(x_extent);
    let fy = f64::from(y.min(y_extent)) / f64::from(y_extent);
    let Some(output) = output else {
        return Some((fx, fy));
    };
    let px = f64::from(output.loc.x) + fx * f64::from(output.size.w);
    let py = f64::from(output.loc.y) + fy * f64::from(output.size.h);
    Some((
        (px - f64::from(layout.loc.x)) / f64::from(layout.size.w),
        (py - f64::from(layout.loc.y)) / f64::from(layout.size.h),
    ))
}

/// Evdev codes of the modifiers a chord can name.
const KEY_LEFTCTRL: u32 = 29;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFTMETA: u32 = 125;

fn modifier_code(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "super" | "mod4" | "logo" | "meta" | "win" => Some(KEY_LEFTMETA),
        "shift" => Some(KEY_LEFTSHIFT),
        "ctrl" | "control" => Some(KEY_LEFTCTRL),
        "alt" | "mod1" => Some(KEY_LEFTALT),
        _ => None,
    }
}

/// The key presses a chord such as `Super+Shift+Return`, `Ctrl+c` or
/// `Super+code:10` stands for, as xkb keycodes in press order. The key is a
/// keysym name looked up in `keymap`'s `layout`, or `code:<xkb keycode>` (the
/// evdev code plus 8, as `wev` prints it). A keysym only on the shifted level
/// adds Shift.
pub fn resolve_chord(
    chord: &str,
    keymap: &xkb::Keymap,
    layout: xkb::LayoutIndex,
) -> Result<Vec<Keycode>, String> {
    let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Err("empty chord".into());
    };
    if key.is_empty() {
        return Err(format!("no key in {chord:?}"));
    }
    let mut codes = Vec::new();
    for name in modifiers {
        let code = modifier_code(name).ok_or_else(|| format!("unknown modifier {name:?}"))?;
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    let key_code = if let Some(code) = key.strip_prefix("code:") {
        code.parse::<u32>()
            .ok()
            .and_then(|code| code.checked_sub(8))
            .ok_or_else(|| format!("bad xkb keycode {code:?}"))?
    } else if let Some(code) = modifier_code(key) {
        code
    } else {
        let keysym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
        let keysym = if keysym.raw() == xkb::keysyms::KEY_NoSymbol {
            xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE)
        } else {
            keysym
        };
        if keysym.raw() == xkb::keysyms::KEY_NoSymbol {
            return Err(format!("unknown key {key:?}"));
        }
        let (code, level) = keycode_for_keysym(keymap, layout, keysym)
            .ok_or_else(|| format!("{key:?} is not on the active layout"))?;
        if level > 0 && !codes.contains(&KEY_LEFTSHIFT) {
            codes.push(KEY_LEFTSHIFT);
        }
        code
    };
    codes.push(key_code);
    Ok(codes
        .into_iter()
        .map(|code| Keycode::new(code + 8))
        .collect())
}

/// The lowest evdev code producing `keysym` on `layout`, and its level
/// (0 plain, 1 shifted). Higher levels are not reachable with the modifiers
/// a chord names.
fn keycode_for_keysym(
    keymap: &xkb::Keymap,
    layout: xkb::LayoutIndex,
    keysym: xkb::Keysym,
) -> Option<(u32, u32)> {
    for level in 0..2 {
        let mut found = None;
        keymap.key_for_each(|keymap, keycode| {
            if found.is_some() || keycode.raw() < 8 {
                return;
            }
            if keymap
                .key_get_syms_by_level(keycode, layout, level)
                .contains(&keysym)
            {
                found = Some(keycode.raw() - 8);
            }
        });
        if let Some(code) = found {
            return Some((code, level));
        }
    }
    None
}

impl ShojiWM {
    /// Event time in microseconds on the compositor's monotonic clock, the
    /// clock libinput stamps its events with.
    pub fn virtual_input_time(&self) -> u64 {
        std::time::Duration::from(self.clock.now()).as_micros() as u64
    }

    /// Press then release `chord` on a virtual keyboard, through the same
    /// path as a real key press.
    pub fn inject_key_chord(&mut self, chord: &str, hold_ms: u32) -> Result<(), String> {
        // The chord names keys of the session's layout.
        self.restore_session_keymap();
        let keyboard = self
            .seat
            .get_keyboard()
            .ok_or_else(|| "the seat has no keyboard".to_string())?;
        let codes = keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().unwrap();
            let layout = xkb.active_layout();
            // Safety: the keymap is only read, and not kept past this call.
            let keymap = unsafe { xkb.keymap() };
            resolve_chord(chord, keymap, layout.0)
        })?;
        self.send_control_keys(&codes, KeyState::Pressed);
        if hold_ms == 0 {
            self.send_control_keys(&codes, KeyState::Released);
            return Ok(());
        }
        let timer = Timer::from_duration(Duration::from_millis(u64::from(hold_ms)));
        let held = codes.clone();
        let scheduled = self.loop_handle.insert_source(timer, move |_, _, state| {
            state.send_control_keys(&held, KeyState::Released);
            TimeoutAction::Drop
        });
        if let Err(error) = scheduled {
            self.send_control_keys(&codes, KeyState::Released);
            return Err(format!("could not schedule the release: {error}"));
        }
        Ok(())
    }

    /// Presses `codes` in order, or releases them in reverse, on the control
    /// socket's keyboard.
    fn send_control_keys(&mut self, codes: &[Keycode], state: KeyState) {
        let device = VirtualDevice {
            id: "shojiwm-control-keyboard".into(),
            name: "ShojiWM control socket keyboard".into(),
            keyboard: true,
        };
        let len = codes.len() as u32;
        let ordered: Vec<(u32, Keycode)> = match state {
            KeyState::Pressed => codes
                .iter()
                .copied()
                .zip(1..)
                .map(|(c, n)| (n, c))
                .collect(),
            KeyState::Released => codes
                .iter()
                .rev()
                .copied()
                .zip((0..len).rev())
                .map(|(c, n)| (n, c))
                .collect(),
        };
        for (count, keycode) in ordered {
            self.process_input_event(InputEvent::<VirtualInput>::Keyboard {
                event: VirtualKey {
                    device: device.clone(),
                    time: self.virtual_input_time(),
                    keycode,
                    state,
                    count,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us_keymap() -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the us keymap compiles")
    }

    fn evdev(codes: Vec<Keycode>) -> Vec<u32> {
        codes.into_iter().map(|code| code.raw() - 8).collect()
    }

    #[test]
    fn chord_presses_modifiers_then_the_key() {
        let keymap = us_keymap();
        assert_eq!(
            evdev(resolve_chord("Super+Shift+Return", &keymap, 0).unwrap()),
            vec![KEY_LEFTMETA, KEY_LEFTSHIFT, 28]
        );
        assert_eq!(
            evdev(resolve_chord("ctrl+c", &keymap, 0).unwrap()),
            vec![KEY_LEFTCTRL, 46]
        );
    }

    #[test]
    fn shifted_keysym_adds_shift_once() {
        let keymap = us_keymap();
        assert_eq!(
            evdev(resolve_chord("exclam", &keymap, 0).unwrap()),
            vec![KEY_LEFTSHIFT, 2]
        );
        assert_eq!(
            evdev(resolve_chord("Shift+exclam", &keymap, 0).unwrap()),
            vec![KEY_LEFTSHIFT, 2]
        );
    }

    #[test]
    fn key_codes_and_lone_modifiers() {
        let keymap = us_keymap();
        assert_eq!(
            evdev(resolve_chord("Super+code:11", &keymap, 0).unwrap()),
            vec![KEY_LEFTMETA, 3]
        );
        assert_eq!(
            evdev(resolve_chord("Super", &keymap, 0).unwrap()),
            vec![KEY_LEFTMETA]
        );
    }

    #[test]
    fn bad_chords_are_errors() {
        let keymap = us_keymap();
        assert!(resolve_chord("Hyper+a", &keymap, 0).is_err());
        assert!(resolve_chord("Super+", &keymap, 0).is_err());
        assert!(resolve_chord("Super+NotAKey", &keymap, 0).is_err());
        assert!(resolve_chord("code:x", &keymap, 0).is_err());
        assert!(resolve_chord("code:7", &keymap, 0).is_err());
    }

    #[test]
    fn axis_frame_sums_and_stops() {
        let mut frame = AxisFrame::default();
        assert!(frame.is_empty());
        frame.add_discrete(Axis::Vertical, 15.0, 1);
        frame.add_discrete(Axis::Vertical, 15.0, 1);
        frame.stop(Axis::Horizontal);
        assert_eq!(
            frame.vertical,
            AxisAmount {
                value: Some(30.0),
                v120: Some(240.0)
            }
        );
        assert_eq!(
            frame.horizontal,
            AxisAmount {
                value: Some(0.0),
                v120: None
            }
        );
        assert!(!frame.is_empty());
    }

    #[test]
    fn a_sourceless_axis_counts_wheel_notches() {
        let mut frame = AxisFrame::default();
        frame.add(Axis::Vertical, 15.0);
        assert_eq!(frame.v120(Axis::Vertical), Some(120.0));
        assert_eq!(frame.v120(Axis::Horizontal), None);

        frame.source = Some(AxisSource::Finger);
        assert_eq!(frame.v120(Axis::Vertical), None);

        let mut discrete = AxisFrame::default();
        discrete.add_discrete(Axis::Vertical, 10.0, 2);
        assert_eq!(discrete.v120(Axis::Vertical), Some(240.0));
    }

    #[test]
    fn absolute_motion_maps_onto_the_layout_or_one_output() {
        let layout = Rectangle::new((0, 0).into(), (3000, 1000).into());
        assert_eq!(
            absolute_fraction(50, 25, 100, 100, None, layout),
            Some((0.5, 0.25))
        );
        let right = Rectangle::new((1000, 0).into(), (2000, 1000).into());
        assert_eq!(
            absolute_fraction(50, 100, 100, 100, Some(right), layout),
            Some((2000.0 / 3000.0, 1.0))
        );
        assert_eq!(
            absolute_fraction(200, 0, 100, 100, None, layout),
            Some((1.0, 0.0))
        );
        assert_eq!(absolute_fraction(1, 1, 0, 100, None, layout), None);
    }
}
