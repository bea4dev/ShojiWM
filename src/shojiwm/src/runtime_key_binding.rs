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
        if shortcut.wheel_direction.is_some() && self.on != RuntimeKeyBindingPhase::Press {
            return Err(RuntimeKeyBindingParseError::WheelRequiresPress(
                self.shortcut.clone(),
            ));
        }
        Ok(CompiledRuntimeKeyBinding {
            id: self.id.clone(),
            phase: self.on,
            shortcut,
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
}

impl CompiledRuntimeKeyBinding {
    pub fn matches(
        &self,
        phase: RuntimeKeyBindingPhase,
        modifiers: &ModifiersState,
        handle: &KeysymHandle<'_>,
    ) -> bool {
        self.phase == phase && self.shortcut.matches(modifiers, handle)
    }

    pub fn matches_wheel(
        &self,
        direction: RuntimeWheelDirection,
        modifiers: &ModifiersState,
    ) -> bool {
        self.phase == RuntimeKeyBindingPhase::Press
            && self.shortcut.matches_wheel(direction, modifiers)
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
    pub wheel_direction: Option<RuntimeWheelDirection>,
}

impl RuntimeKeyShortcut {
    pub fn matches(&self, modifiers: &ModifiersState, handle: &KeysymHandle<'_>) -> bool {
        // Modifier-only shortcuts are not matched here; they are dispatched as a
        // release "tap" by the input layer (see process_input_event).
        let Some(keysym) = self.keysym else {
            return false;
        };
        let Some(raw_keysym) = handle.raw_latin_sym_or_raw_current_sym() else {
            return false;
        };

        self.modifiers_match(modifiers)
            && keysym == raw_keysym
    }

    pub fn matches_wheel(
        &self,
        direction: RuntimeWheelDirection,
        modifiers: &ModifiersState,
    ) -> bool {
        self.wheel_direction == Some(direction) && self.modifiers_match(modifiers)
    }

    pub fn is_modifier_only(&self) -> bool {
        self.keysym.is_none() && self.wheel_direction.is_none()
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
    #[error("shortcut `{shortcut}` contains unknown modifier `{modifier}`")]
    UnknownModifier { shortcut: String, modifier: String },
    #[error("shortcut `{shortcut}` contains duplicate modifier `{modifier}`")]
    DuplicateModifier { shortcut: String, modifier: String },
    #[error("shortcut `{shortcut}` contains multiple non-modifier keys")]
    MultipleKeys { shortcut: String },
    #[error("shortcut `{shortcut}` contains unknown key `{key}`")]
    UnknownKey { shortcut: String, key: String },
}

pub fn compile_runtime_key_bindings(
    entries: &BTreeMap<String, RuntimeKeyBindingEntry>,
) -> Vec<CompiledRuntimeKeyBinding> {
    entries
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
        .collect()
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
    let mut wheel_direction = None;

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

        if keysym.is_some() || wheel_direction.is_some() {
            return Err(RuntimeKeyBindingParseError::MultipleKeys {
                shortcut: shortcut.to_string(),
            });
        }

        if let Some(direction) = parse_wheel_direction(part) {
            wheel_direction = Some(direction);
            continue;
        }

        let parsed_keysym =
            parse_keysym_name(part).ok_or_else(|| RuntimeKeyBindingParseError::UnknownKey {
                shortcut: shortcut.to_string(),
                key: part.to_string(),
            })?;
        keysym = Some(parsed_keysym);
    }

    if keysym.is_none() && wheel_direction.is_none() {
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
            wheel_direction: None,
        });
    }

    Ok(RuntimeKeyShortcut {
        ctrl,
        alt,
        shift,
        logo,
        keysym,
        wheel_direction,
    })
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
        }
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
