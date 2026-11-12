use std::collections::BTreeMap;

use smithay::input::keyboard::{KeysymHandle, ModifiersState};
use xkbcommon::xkb;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RuntimeKeyBindingConfigUpdate {
    pub entries: Vec<RuntimeKeyBindingEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct RuntimeKeyBindingEntry {
    pub id: String,
    pub shortcut: String,
    #[serde(default)]
    pub on: RuntimeKeyBindingPhase,
    /// Tap bindings only: fire only if the modifier was released within this
    /// many milliseconds of being pressed.
    #[serde(default)]
    pub within: Option<u32>,
    /// Also fire while the session is locked (volume, brightness keys).
    #[serde(default)]
    pub locked: bool,
    /// Fire again at the keyboard's repeat delay and rate while the key is
    /// held.
    #[serde(default)]
    pub repeat: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKeyBindingPhase {
    #[default]
    Press,
    Release,
}

impl RuntimeKeyBindingEntry {
    pub fn compile(&self) -> Result<CompiledRuntimeKeyBinding, RuntimeKeyBindingParseError> {
        let shortcut = parse_runtime_key_shortcut(&self.shortcut)?;
        // A modifier-only shortcut (e.g. "Super") fires as a "tap": it is only
        // meaningful on release, since on press we cannot yet tell a tap from a
        // modifier held for a combo. Reject press to surface the mistake.
        if shortcut.is_modifier_only() && self.on != RuntimeKeyBindingPhase::Release {
            return Err(RuntimeKeyBindingParseError::ModifierOnlyRequiresRelease(
                self.shortcut.clone(),
            ));
        }
        if self.within.is_some() && !shortcut.is_modifier_only() {
            return Err(RuntimeKeyBindingParseError::WithinRequiresTap(
                self.shortcut.clone(),
            ));
        }
        if shortcut.wheel_direction.is_some() && self.on != RuntimeKeyBindingPhase::Press {
            return Err(RuntimeKeyBindingParseError::WheelRequiresPress(
                self.shortcut.clone(),
            ));
        }
        if shortcut.mouse_button.is_some() && self.on != RuntimeKeyBindingPhase::Press {
            return Err(RuntimeKeyBindingParseError::MouseButtonRequiresPress(
                self.shortcut.clone(),
            ));
        }
        let is_key = !shortcut.is_modifier_only()
            && shortcut.wheel_direction.is_none()
            && shortcut.mouse_button.is_none();
        if self.repeat && !(is_key && self.on == RuntimeKeyBindingPhase::Press) {
            return Err(RuntimeKeyBindingParseError::RepeatRequiresKeyPress(
                self.shortcut.clone(),
            ));
        }
        // While locked the pointer belongs to the lock screen, and a tap of a
        // bare modifier is how a locker is usually typed into.
        if self.locked && !is_key {
            return Err(RuntimeKeyBindingParseError::LockedRequiresKey(
                self.shortcut.clone(),
            ));
        }
        Ok(CompiledRuntimeKeyBinding {
            id: self.id.clone(),
            phase: self.on,
            shortcut,
            within_ms: self.within,
            locked: self.locked,
            repeat: self.repeat,
        })
    }
}

/// A keyboard modifier class, independent of the left/right physical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierClass {
    Ctrl,
    Alt,
    Shift,
    Logo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeWheelDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRuntimeKeyBinding {
    pub id: String,
    pub phase: RuntimeKeyBindingPhase,
    pub shortcut: RuntimeKeyShortcut,
    /// Tap bindings: the longest press, in ms, that still counts as a tap.
    pub within_ms: Option<u32>,
    pub locked: bool,
    pub repeat: bool,
}

impl CompiledRuntimeKeyBinding {
    pub fn matches(
        &self,
        phase: RuntimeKeyBindingPhase,
        modifiers: &ModifiersState,
        handle: &KeysymHandle<'_>,
        keycode: Option<u32>,
    ) -> bool {
        self.phase == phase && self.shortcut.matches(modifiers, handle, keycode)
    }

    pub fn matches_wheel(
        &self,
        direction: RuntimeWheelDirection,
        modifiers: &ModifiersState,
    ) -> bool {
        self.phase == RuntimeKeyBindingPhase::Press
            && self.shortcut.matches_wheel(direction, modifiers)
    }

    pub fn matches_mouse_button(&self, button: u32, modifiers: &ModifiersState) -> bool {
        self.phase == RuntimeKeyBindingPhase::Press
            && self.shortcut.mouse_button == Some(button)
            && self.shortcut.modifiers_match(modifiers)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeKeyShortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    /// `None` for a modifier-only shortcut (a "tap" binding).
    pub keysym: Option<xkb::Keysym>,
    /// `code:<xkb keycode>`: the physical key, whatever the layout puts on it.
    pub keycode: Option<u32>,
    pub wheel_direction: Option<RuntimeWheelDirection>,
    pub mouse_button: Option<u32>,
}

impl RuntimeKeyShortcut {
    /// `keycode` is the key a `code:N` shortcut compares N with
    /// (`ShojiWM::bind_keycode`).
    pub fn matches(
        &self,
        modifiers: &ModifiersState,
        handle: &KeysymHandle<'_>,
        keycode: Option<u32>,
    ) -> bool {
        // Modifier-only shortcuts are not matched here; they are dispatched as a
        // release "tap" by the input layer (see process_input_event).
        if let Some(code) = self.keycode {
            return self.modifiers_match(modifiers) && keycode == Some(code);
        }
        let Some(keysym) = self.keysym else {
            return false;
        };
        let Some(raw_keysym) = handle.raw_latin_sym_or_raw_current_sym() else {
            return false;
        };

        self.modifiers_match(modifiers) && keysym == raw_keysym
    }

    pub fn matches_wheel(
        &self,
        direction: RuntimeWheelDirection,
        modifiers: &ModifiersState,
    ) -> bool {
        self.wheel_direction == Some(direction) && self.modifiers_match(modifiers)
    }

    pub fn is_modifier_only(&self) -> bool {
        self.keysym.is_none()
            && self.keycode.is_none()
            && self.wheel_direction.is_none()
            && self.mouse_button.is_none()
    }

    /// The single modifier class for a modifier-only shortcut, else `None`.
    pub fn modifier_class(&self) -> Option<ModifierClass> {
        if !self.is_modifier_only() {
            return None;
        }
        match (self.ctrl, self.alt, self.shift, self.logo) {
            (true, false, false, false) => Some(ModifierClass::Ctrl),
            (false, true, false, false) => Some(ModifierClass::Alt),
            (false, false, true, false) => Some(ModifierClass::Shift),
            (false, false, false, true) => Some(ModifierClass::Logo),
            _ => None,
        }
    }

    fn modifiers_match(&self, modifiers: &ModifiersState) -> bool {
        self.ctrl == modifiers.ctrl
            && self.alt == modifiers.alt
            && self.shift == modifiers.shift
            && self.logo == modifiers.logo
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeKeyBindingParseError {
    #[error("shortcut must not be empty")]
    EmptyShortcut,
    #[error("shortcut `{0}` must include exactly one non-modifier key")]
    MissingKey(String),
    #[error("modifier-only shortcut `{0}` must use exactly one modifier")]
    ModifierOnlyMultipleModifiers(String),
    #[error("modifier-only shortcut `{0}` is only valid with `on: \"release\"`")]
    ModifierOnlyRequiresRelease(String),
    #[error("mouse-wheel shortcut `{0}` is only valid with `on: \"press\"`")]
    WheelRequiresPress(String),
    #[error("mouse-button shortcut `{0}` is only valid with `on: \"press\"`")]
    MouseButtonRequiresPress(String),
    #[error("`within` on `{0}` only applies to modifier-only tap bindings")]
    WithinRequiresTap(String),
    #[error("`repeat` on `{0}` only applies to a key with `on: \"press\"`")]
    RepeatRequiresKeyPress(String),
    #[error("`locked` on `{0}` only applies to a key, not a tap, wheel or mouse button")]
    LockedRequiresKey(String),
    #[error("shortcut `{shortcut}` contains unknown modifier `{modifier}`")]
    UnknownModifier { shortcut: String, modifier: String },
    #[error("shortcut `{shortcut}` contains duplicate modifier `{modifier}`")]
    DuplicateModifier { shortcut: String, modifier: String },
    #[error("shortcut `{shortcut}` contains multiple non-modifier keys")]
    MultipleKeys { shortcut: String },
    #[error("shortcut `{shortcut}` contains unknown key `{key}`")]
    UnknownKey { shortcut: String, key: String },
    #[error("shortcut `{shortcut}` has `code:{code}`, which is not an xkb keycode (8 or more)")]
    BadKeycode { shortcut: String, code: String },
}

pub fn compile_runtime_key_bindings(
    entries: &BTreeMap<String, RuntimeKeyBindingEntry>,
) -> Vec<CompiledRuntimeKeyBinding> {
    let compiled = entries
        .values()
        .filter_map(|entry| match entry.compile() {
            Ok(binding) => Some(binding),
            Err(error) => {
                tracing::warn!(
                    binding_id = entry.id,
                    ?error,
                    "ignoring invalid runtime key binding"
                );
                None
            }
        })
        .collect::<Vec<_>>();

    // Dispatch picks the first match in id order, so a binding that reuses an
    // existing shortcut silently never fires. Surface that so "my new binding
    // does nothing" is diagnosable from the log. Modifier-only taps are
    // exempt: the tap dispatcher fires every matching binding.
    for (index, binding) in compiled.iter().enumerate() {
        if binding.shortcut.is_modifier_only() {
            continue;
        }
        if let Some(winner) = compiled[..index]
            .iter()
            .find(|other| other.phase == binding.phase && other.shortcut == binding.shortcut)
        {
            tracing::warn!(
                binding_id = binding.id,
                shadowed_by = winner.id,
                shortcut = entries
                    .get(&binding.id)
                    .map(|entry| entry.shortcut.as_str())
                    .unwrap_or_default(),
                "key binding shares a shortcut with an earlier binding and will never fire \
                 (dispatch picks the alphabetically first id)"
            );
        }
    }

    compiled
}

fn parse_runtime_key_shortcut(
    shortcut: &str,
) -> Result<RuntimeKeyShortcut, RuntimeKeyBindingParseError> {
    let parts = shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return Err(RuntimeKeyBindingParseError::EmptyShortcut);
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut logo = false;
    let mut keysym = None;
    let mut keycode = None;
    let mut wheel_direction = None;
    let mut mouse_button = None;

    for part in parts {
        if let Some(target) = modifier_slot(part, &mut ctrl, &mut alt, &mut shift, &mut logo) {
            if *target {
                return Err(RuntimeKeyBindingParseError::DuplicateModifier {
                    shortcut: shortcut.to_string(),
                    modifier: part.to_string(),
                });
            }
            *target = true;
            continue;
        }

        if keysym.is_some()
            || keycode.is_some()
            || wheel_direction.is_some()
            || mouse_button.is_some()
        {
            return Err(RuntimeKeyBindingParseError::MultipleKeys {
                shortcut: shortcut.to_string(),
            });
        }

        if let Some(direction) = parse_wheel_direction(part) {
            wheel_direction = Some(direction);
            continue;
        }

        if let Some(button) = parse_mouse_button(part) {
            mouse_button = Some(button);
            continue;
        }

        if let Some(code) = part
            .get(..5)
            .filter(|prefix| prefix.eq_ignore_ascii_case("code:"))
            .map(|_| &part[5..])
        {
            let parsed = code
                .parse::<u32>()
                .ok()
                .filter(|code| *code >= 8)
                .ok_or_else(|| RuntimeKeyBindingParseError::BadKeycode {
                    shortcut: shortcut.to_string(),
                    code: code.to_string(),
                })?;
            keycode = Some(parsed);
            continue;
        }

        let parsed_keysym =
            parse_keysym_name(part).ok_or_else(|| RuntimeKeyBindingParseError::UnknownKey {
                shortcut: shortcut.to_string(),
                key: part.to_string(),
            })?;
        keysym = Some(parsed_keysym);
    }

    if keysym.is_none() && keycode.is_none() && wheel_direction.is_none() && mouse_button.is_none()
    {
        // Modifier-only shortcut ("tap"): require exactly one modifier.
        let modifier_count = [ctrl, alt, shift, logo]
            .into_iter()
            .filter(|set| *set)
            .count();
        if modifier_count != 1 {
            return Err(RuntimeKeyBindingParseError::ModifierOnlyMultipleModifiers(
                shortcut.to_string(),
            ));
        }
        return Ok(RuntimeKeyShortcut {
            ctrl,
            alt,
            shift,
            logo,
            keysym: None,
            keycode: None,
            wheel_direction: None,
            mouse_button: None,
        });
    }

    Ok(RuntimeKeyShortcut {
        ctrl,
        alt,
        shift,
        logo,
        keysym,
        keycode,
        wheel_direction,
        mouse_button,
    })
}

fn parse_mouse_button(name: &str) -> Option<u32> {
    match name.to_ascii_lowercase().as_str() {
        "mouseback" => Some(0x113),
        "mouseforward" => Some(0x114),
        _ => None,
    }
}

fn parse_wheel_direction(name: &str) -> Option<RuntimeWheelDirection> {
    match name.to_ascii_lowercase().as_str() {
        "wheelscrollup" => Some(RuntimeWheelDirection::Up),
        "wheelscrolldown" => Some(RuntimeWheelDirection::Down),
        "wheelscrollleft" => Some(RuntimeWheelDirection::Left),
        "wheelscrollright" => Some(RuntimeWheelDirection::Right),
        _ => None,
    }
}

fn modifier_slot<'a>(
    part: &str,
    ctrl: &'a mut bool,
    alt: &'a mut bool,
    shift: &'a mut bool,
    logo: &'a mut bool,
) -> Option<&'a mut bool> {
    match part.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(ctrl),
        "alt" | "mod1" => Some(alt),
        "shift" => Some(shift),
        "super" | "logo" | "meta" | "win" | "mod4" => Some(logo),
        _ => None,
    }
}

fn parse_keysym_name(name: &str) -> Option<xkb::Keysym> {
    let normalized = if name.chars().count() == 1 {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    };
    let keysym = xkb::keysym_from_name(&normalized, xkb::KEYSYM_CASE_INSENSITIVE);
    (keysym != xkb::keysyms::KEY_NoSymbol.into()).then_some(keysym)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(shortcut: &str, on: RuntimeKeyBindingPhase) -> RuntimeKeyBindingEntry {
        RuntimeKeyBindingEntry {
            id: "test".to_string(),
            shortcut: shortcut.to_string(),
            on,
            within: None,
            locked: false,
            repeat: false,
        }
    }

    #[test]
    fn repeat_needs_a_key_on_press() {
        let repeat = |shortcut: &str, on| RuntimeKeyBindingEntry {
            repeat: true,
            ..entry(shortcut, on)
        };
        let binding = repeat("XF86AudioRaiseVolume", RuntimeKeyBindingPhase::Press)
            .compile()
            .unwrap();
        assert!(binding.repeat && !binding.locked);
        for (shortcut, on) in [
            ("XF86AudioRaiseVolume", RuntimeKeyBindingPhase::Release),
            ("Super", RuntimeKeyBindingPhase::Release),
            ("Super+WheelScrollUp", RuntimeKeyBindingPhase::Press),
            ("Super+MouseBack", RuntimeKeyBindingPhase::Press),
        ] {
            assert!(matches!(
                repeat(shortcut, on).compile(),
                Err(RuntimeKeyBindingParseError::RepeatRequiresKeyPress(_)),
            ));
        }
    }

    #[test]
    fn keycode_shortcuts_name_the_physical_key() {
        let shortcut = parse_runtime_key_shortcut("Super+Shift+code:10").unwrap();
        assert_eq!(shortcut.keycode, Some(10));
        assert!(shortcut.keysym.is_none());
        assert!(shortcut.logo && shortcut.shift);
        assert!(!shortcut.is_modifier_only());
        assert_eq!(
            parse_runtime_key_shortcut("CODE:19").unwrap().keycode,
            Some(19)
        );
        assert!(
            entry("Super+code:10", RuntimeKeyBindingPhase::Press)
                .compile()
                .is_ok()
        );
    }

    #[test]
    fn keycodes_below_eight_and_extra_keys_are_rejected() {
        for shortcut in ["Super+code:7", "Super+code:x", "Super+code:", "code:10+a"] {
            assert!(
                parse_runtime_key_shortcut(shortcut).is_err(),
                "{shortcut} should not parse"
            );
        }
    }

    #[test]
    fn locked_needs_a_key() {
        let locked = |shortcut: &str, on| RuntimeKeyBindingEntry {
            locked: true,
            ..entry(shortcut, on)
        };
        for on in [
            RuntimeKeyBindingPhase::Press,
            RuntimeKeyBindingPhase::Release,
        ] {
            assert!(locked("XF86MonBrightnessUp", on).compile().unwrap().locked);
        }
        for (shortcut, on) in [
            ("Super", RuntimeKeyBindingPhase::Release),
            ("Super+WheelScrollUp", RuntimeKeyBindingPhase::Press),
            ("Super+MouseForward", RuntimeKeyBindingPhase::Press),
        ] {
            assert!(matches!(
                locked(shortcut, on).compile(),
                Err(RuntimeKeyBindingParseError::LockedRequiresKey(_)),
            ));
        }
    }

    #[test]
    fn options_default_off_on_the_wire() {
        let entry: RuntimeKeyBindingEntry =
            serde_json::from_str(r#"{"id":"a","shortcut":"Super+T","on":"press"}"#).unwrap();
        assert!(!entry.locked && !entry.repeat);
        let entry: RuntimeKeyBindingEntry = serde_json::from_str(
            r#"{"id":"a","shortcut":"XF86AudioMute","locked":true,"repeat":true}"#,
        )
        .unwrap();
        assert!(entry.locked && entry.repeat);
    }

    #[test]
    fn combo_shortcut_has_keysym() {
        let shortcut = parse_runtime_key_shortcut("Super+A").unwrap();
        assert!(!shortcut.is_modifier_only());
        assert!(shortcut.keysym.is_some());
        assert!(shortcut.wheel_direction.is_none());
        assert!(shortcut.logo);
        assert_eq!(shortcut.modifier_class(), None);
    }

    #[test]
    fn wheel_shortcuts_parse_case_insensitively() {
        for (name, expected) in [
            ("WheelScrollUp", RuntimeWheelDirection::Up),
            ("wheelscrolldown", RuntimeWheelDirection::Down),
            ("WHEELSCROLLLEFT", RuntimeWheelDirection::Left),
            ("WheelScrollRight", RuntimeWheelDirection::Right),
        ] {
            let shortcut = parse_runtime_key_shortcut(&format!("Super+{name}")).unwrap();
            assert_eq!(shortcut.wheel_direction, Some(expected));
            assert!(shortcut.keysym.is_none());
            assert!(shortcut.logo);
            assert!(!shortcut.is_modifier_only());
        }
    }

    #[test]
    fn wheel_shortcut_requires_press_phase() {
        assert!(
            entry("Super+WheelScrollUp", RuntimeKeyBindingPhase::Press)
                .compile()
                .is_ok()
        );
        assert!(matches!(
            entry("Super+WheelScrollUp", RuntimeKeyBindingPhase::Release).compile(),
            Err(RuntimeKeyBindingParseError::WheelRequiresPress(_)),
        ));
    }

    #[test]
    fn side_buttons_require_press_and_exact_modifiers() {
        let modifiers = ModifiersState {
            logo: true,
            ..Default::default()
        };
        for (name, code) in [("MouseBack", 275), ("mouseforward", 276)] {
            let shortcut = format!("Super+{name}");
            let binding = entry(&shortcut, RuntimeKeyBindingPhase::Press)
                .compile()
                .unwrap();
            assert!(!binding.shortcut.is_modifier_only());
            assert!(binding.matches_mouse_button(code, &modifiers));
            assert!(!binding.matches_mouse_button(272, &modifiers));
            assert!(!binding.matches_mouse_button(code, &ModifiersState::default()));
            assert!(!binding.matches_mouse_button(
                code,
                &ModifiersState {
                    shift: true,
                    ..modifiers
                },
            ));
            assert!(matches!(
                entry(&shortcut, RuntimeKeyBindingPhase::Release).compile(),
                Err(RuntimeKeyBindingParseError::MouseButtonRequiresPress(_)),
            ));
        }
        for shortcut in [
            "MouseBack+MouseForward",
            "A+MouseBack",
            "MouseForward+WheelScrollUp",
        ] {
            assert!(matches!(
                parse_runtime_key_shortcut(shortcut),
                Err(RuntimeKeyBindingParseError::MultipleKeys { .. })
            ));
        }
    }

    #[test]
    fn wheel_shortcut_rejects_another_trigger() {
        assert!(matches!(
            parse_runtime_key_shortcut("Super+A+WheelScrollUp"),
            Err(RuntimeKeyBindingParseError::MultipleKeys { .. }),
        ));
    }

    #[test]
    fn modifier_only_shortcut_parses() {
        let shortcut = parse_runtime_key_shortcut("Super").unwrap();
        assert!(shortcut.is_modifier_only());
        assert!(shortcut.keysym.is_none());
        assert!(shortcut.wheel_direction.is_none());
        assert_eq!(shortcut.modifier_class(), Some(ModifierClass::Logo));
    }

    #[test]
    fn modifier_only_other_modifiers() {
        assert_eq!(
            parse_runtime_key_shortcut("Ctrl").unwrap().modifier_class(),
            Some(ModifierClass::Ctrl),
        );
        assert_eq!(
            parse_runtime_key_shortcut("Alt").unwrap().modifier_class(),
            Some(ModifierClass::Alt),
        );
        assert_eq!(
            parse_runtime_key_shortcut("Shift")
                .unwrap()
                .modifier_class(),
            Some(ModifierClass::Shift),
        );
    }

    #[test]
    fn modifier_only_requires_single_modifier() {
        assert!(matches!(
            parse_runtime_key_shortcut("Super+Shift"),
            Err(RuntimeKeyBindingParseError::ModifierOnlyMultipleModifiers(
                _
            )),
        ));
    }

    #[test]
    fn within_only_applies_to_tap_bindings() {
        let mut tap = entry("Super", RuntimeKeyBindingPhase::Release);
        tap.within = Some(300);
        assert_eq!(tap.compile().unwrap().within_ms, Some(300));
        let mut combo = entry("Super+T", RuntimeKeyBindingPhase::Press);
        combo.within = Some(300);
        assert!(matches!(
            combo.compile(),
            Err(RuntimeKeyBindingParseError::WithinRequiresTap(_)),
        ));
    }

    #[test]
    fn modifier_only_requires_release_phase() {
        assert!(matches!(
            entry("Super", RuntimeKeyBindingPhase::Press).compile(),
            Err(RuntimeKeyBindingParseError::ModifierOnlyRequiresRelease(_)),
        ));
        assert!(
            entry("Super", RuntimeKeyBindingPhase::Release)
                .compile()
                .is_ok()
        );
    }

    #[test]
    fn combo_allows_press_phase() {
        assert!(
            entry("Super+A", RuntimeKeyBindingPhase::Press)
                .compile()
                .is_ok()
        );
    }
}
