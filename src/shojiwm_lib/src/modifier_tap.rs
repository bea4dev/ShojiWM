//! Detects a modifier tapped on its own (e.g. a bare `Super`).
//!
//! A modifier tap fires on release if nothing else was pressed in between: no
//! other key before or during it, no button, no wheel.
//!
//! That needs to know which keys are down, and a release can go missing. A
//! session lock takes the releases of the keys that triggered it, and a KVM
//! switch or an unplugged keyboard can take a release with it. Known cases call
//! [`ModifierTapTracker::reset`]. For the rest, a key that looks held for longer
//! than [`STALE_KEY_MS`] is forgotten. That cannot tell a lost release from a key
//! really held that long, so the limit is well above what a game asks of a key: a
//! key really held past it no longer blocks a tap.

use std::collections::HashMap;

use crate::runtime_key_binding::ModifierClass;

/// A key that looks held for longer than this (ms) no longer blocks a tap.
const STALE_KEY_MS: u32 = 60_000;

/// A completed tap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifierTap {
    pub class: ModifierClass,
    /// How long the modifier was held, for `within`.
    pub held_ms: u32,
}

#[derive(Debug, Default)]
pub struct ModifierTapTracker {
    /// The keys down now, with the event time (ms) of their press.
    pressed_keys: HashMap<u32, u32>,
    armed: Option<ModifierClass>,
    armed_at: u32,
    interrupted: bool,
}

impl ModifierTapTracker {
    /// `class` is the modifier class of the key, if it is a modifier.
    pub fn press(&mut self, key: u32, class: Option<ModifierClass>, time: u32) {
        self.pressed_keys.retain(|_, pressed_at| {
            // Signed: devices are read in turn, so an event can carry a time just
            // before that of a press already handled. That press is recent, not
            // 49 days old.
            (time.wrapping_sub(*pressed_at) as i32) <= STALE_KEY_MS as i32
        });
        let was_idle = self.pressed_keys.keys().all(|&down| down == key);
        self.pressed_keys.insert(key, time);
        match class {
            Some(class) if was_idle => {
                self.armed = Some(class);
                self.armed_at = time;
                self.interrupted = false;
            }
            _ => self.interrupted = true,
        }
    }

    /// Returns the tap this release completes, if any.
    pub fn release(
        &mut self,
        key: u32,
        class: Option<ModifierClass>,
        time: u32,
    ) -> Option<ModifierTap> {
        let tap = class
            .filter(|class| self.armed == Some(*class) && !self.interrupted)
            .map(|class| ModifierTap {
                class,
                held_ms: time.wrapping_sub(self.armed_at),
            });
        self.pressed_keys.remove(&key);
        if self.pressed_keys.is_empty() {
            self.armed = None;
            self.interrupted = false;
        }
        tap
    }

    /// Other input happened, so what is held now is not a tap.
    pub fn interrupt(&mut self) {
        self.interrupted = true;
    }

    /// Forget everything, for when releases are known to have gone elsewhere.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPER: u32 = 133;
    const KEY_A: u32 = 38;
    const KEY_W: u32 = 25;
    const LOGO: Option<ModifierClass> = Some(ModifierClass::Logo);

    fn logo_tap(held_ms: u32) -> Option<ModifierTap> {
        Some(ModifierTap {
            class: ModifierClass::Logo,
            held_ms,
        })
    }

    #[test]
    fn a_modifier_pressed_and_released_alone_is_a_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);
        assert_eq!(tracker.release(SUPER, LOGO, 1120), logo_tap(120));
    }

    #[test]
    fn a_key_other_than_a_modifier_is_never_a_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(KEY_A, None, 1000);
        assert_eq!(tracker.release(KEY_A, None, 1100), None);
    }

    #[test]
    fn a_key_pressed_during_the_modifier_cancels_the_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);
        tracker.press(KEY_A, None, 1050);
        assert_eq!(tracker.release(KEY_A, None, 1100), None);
        assert_eq!(tracker.release(SUPER, LOGO, 1150), None);
    }

    #[test]
    fn other_input_during_the_modifier_cancels_the_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);
        tracker.interrupt();
        assert_eq!(tracker.release(SUPER, LOGO, 1100), None);
    }

    #[test]
    fn a_cancelled_tap_does_not_affect_the_next_one() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);
        tracker.interrupt();
        tracker.release(SUPER, LOGO, 1100);

        tracker.press(SUPER, LOGO, 2000);
        assert_eq!(tracker.release(SUPER, LOGO, 2100), logo_tap(100));
    }

    #[test]
    fn a_key_held_for_a_long_time_still_blocks_a_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(KEY_W, None, 0);

        tracker.press(SUPER, LOGO, STALE_KEY_MS);
        assert_eq!(tracker.release(SUPER, LOGO, STALE_KEY_MS + 100), None);
    }

    #[test]
    fn a_key_that_lost_its_release_stops_blocking_a_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(KEY_W, None, 0);

        tracker.press(SUPER, LOGO, STALE_KEY_MS + 1);
        assert_eq!(
            tracker.release(SUPER, LOGO, STALE_KEY_MS + 101),
            logo_tap(100),
        );
    }

    #[test]
    fn pressing_a_key_again_replaces_its_lost_release() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);

        tracker.press(SUPER, LOGO, 2000);
        assert_eq!(tracker.release(SUPER, LOGO, 2100), logo_tap(100));
    }

    #[test]
    fn a_press_from_just_before_a_handled_one_does_not_forget_it() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(KEY_A, None, 1001);

        tracker.press(SUPER, LOGO, 1000);
        assert_eq!(tracker.release(SUPER, LOGO, 1100), None);
    }

    #[test]
    fn age_survives_the_event_clock_wrapping() {
        let mut tracker = ModifierTapTracker::default();
        let before_wrap = u32::MAX - 10;
        tracker.press(KEY_W, None, before_wrap);

        tracker.press(SUPER, LOGO, 100);
        assert_eq!(tracker.release(SUPER, LOGO, 200), None);

        tracker.press(SUPER, LOGO, before_wrap.wrapping_add(STALE_KEY_MS + 1));
        assert_eq!(
            tracker.release(SUPER, LOGO, before_wrap.wrapping_add(STALE_KEY_MS + 101)),
            logo_tap(100),
        );
    }

    #[test]
    fn reset_forgets_keys_whose_release_went_elsewhere() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);
        tracker.press(KEY_A, None, 1050);

        tracker.reset();

        tracker.press(SUPER, LOGO, 1200);
        assert_eq!(tracker.release(SUPER, LOGO, 1300), logo_tap(100));
    }

    #[test]
    fn a_release_after_reset_is_not_a_tap() {
        let mut tracker = ModifierTapTracker::default();
        tracker.press(SUPER, LOGO, 1000);

        tracker.reset();

        assert_eq!(tracker.release(SUPER, LOGO, 1100), None);
    }
}
