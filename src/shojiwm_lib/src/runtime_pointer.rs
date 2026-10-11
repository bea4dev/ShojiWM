use smithay::input::keyboard::ModifiersState;

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePointerConfigUpdate {
    pub window_move_modifier: Option<String>,
    pub window_resize_modifier: Option<String>,
    /// `COMPOSITOR.pointer.defineRegion`. The whole set, replacing the previous one.
    #[serde(default)]
    pub regions: Vec<crate::runtime_pointer_region::PointerRegionSpec>,
    /// `COMPOSITOR.cursor.setAppLockIgnored`.
    #[serde(default)]
    pub app_lock_ignored: bool,
    /// `COMPOSITOR.cursor.bindAppLockIgnoreModifier`.
    #[serde(default)]
    pub app_lock_ignore_modifier: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimePointerModifier {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl RuntimePointerModifier {
    pub fn matches(&self, modifiers: &ModifiersState) -> bool {
        self.ctrl == modifiers.ctrl
            && self.alt == modifiers.alt
            && self.shift == modifiers.shift
            && self.logo == modifiers.logo
    }

    /// Every modifier of this combination is held (others may be too).
    pub fn held(&self, modifiers: &ModifiersState) -> bool {
        (!self.ctrl || modifiers.ctrl)
            && (!self.alt || modifiers.alt)
            && (!self.shift || modifiers.shift)
            && (!self.logo || modifiers.logo)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimePointerModifierParseError {
    #[error("modifier must not be empty")]
    Empty,
    #[error("modifier shortcut `{shortcut}` contains unknown modifier `{modifier}`")]
    UnknownModifier { shortcut: String, modifier: String },
    #[error("modifier shortcut `{shortcut}` contains duplicate modifier `{modifier}`")]
    DuplicateModifier { shortcut: String, modifier: String },
}

pub fn parse_runtime_pointer_modifier(
    shortcut: &str,
) -> Result<RuntimePointerModifier, RuntimePointerModifierParseError> {
    let parts = shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return Err(RuntimePointerModifierParseError::Empty);
    }

    let mut modifier = RuntimePointerModifier {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
    };

    for part in parts {
        let slot = modifier_slot(part, &mut modifier).ok_or_else(|| {
            RuntimePointerModifierParseError::UnknownModifier {
                shortcut: shortcut.to_string(),
                modifier: part.to_string(),
            }
        })?;

        if *slot {
            return Err(RuntimePointerModifierParseError::DuplicateModifier {
                shortcut: shortcut.to_string(),
                modifier: part.to_string(),
            });
        }
        *slot = true;
    }

    Ok(modifier)
}

fn modifier_slot<'a>(part: &str, modifier: &'a mut RuntimePointerModifier) -> Option<&'a mut bool> {
    match part.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(&mut modifier.ctrl),
        "alt" | "mod1" => Some(&mut modifier.alt),
        "shift" => Some(&mut modifier.shift),
        "super" | "logo" | "meta" | "win" | "mod4" => Some(&mut modifier.logo),
        _ => None,
    }
}
