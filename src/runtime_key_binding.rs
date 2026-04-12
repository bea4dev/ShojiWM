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
        Ok(CompiledRuntimeKeyBinding {
            id: self.id.clone(),
            phase: self.on,
            shortcut,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledRuntimeKeyBinding {
    pub id: String,
    pub phase: RuntimeKeyBindingPhase,
    pub shortcut: RuntimeKeyShortcut,
}

impl CompiledRuntimeKeyBinding {
    pub fn matches(&self, phase: RuntimeKeyBindingPhase, modifiers: &ModifiersState, handle: &KeysymHandle<'_>) -> bool {
        self.phase == phase && self.shortcut.matches(modifiers, handle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeKeyShortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub keysym: xkb::Keysym,
}

impl RuntimeKeyShortcut {
    pub fn matches(&self, modifiers: &ModifiersState, handle: &KeysymHandle<'_>) -> bool {
        let Some(raw_keysym) = handle.raw_latin_sym_or_raw_current_sym() else {
            return false;
        };

        self.ctrl == modifiers.ctrl
            && self.alt == modifiers.alt
            && self.shift == modifiers.shift
            && self.logo == modifiers.logo
            && self.keysym == raw_keysym
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeKeyBindingParseError {
    #[error("shortcut must not be empty")]
    EmptyShortcut,
    #[error("shortcut `{0}` must include exactly one non-modifier key")]
    MissingKey(String),
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
                tracing::warn!(binding_id = entry.id, ?error, "ignoring invalid runtime key binding");
                None
            }
        })
        .collect()
}

fn parse_runtime_key_shortcut(shortcut: &str) -> Result<RuntimeKeyShortcut, RuntimeKeyBindingParseError> {
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

        if keysym.is_some() {
            return Err(RuntimeKeyBindingParseError::MultipleKeys {
                shortcut: shortcut.to_string(),
            });
        }

        let parsed_keysym = parse_keysym_name(part).ok_or_else(|| {
            RuntimeKeyBindingParseError::UnknownKey {
                shortcut: shortcut.to_string(),
                key: part.to_string(),
            }
        })?;
        keysym = Some(parsed_keysym);
    }

    let Some(keysym) = keysym else {
        return Err(RuntimeKeyBindingParseError::MissingKey(shortcut.to_string()));
    };

    Ok(RuntimeKeyShortcut {
        ctrl,
        alt,
        shift,
        logo,
        keysym,
    })
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
