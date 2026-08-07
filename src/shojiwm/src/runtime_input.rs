use std::collections::{BTreeMap, HashMap};

use input::{
    AccelProfile, ClickMethod, Device as LibinputDevice, DeviceCapability, ScrollMethod,
    TapButtonMap,
};
use serde::{Deserialize, Serialize};
use smithay::{
    backend::{
        input::{Device as SmithayInputDevice, InputEvent},
        libinput::LibinputInputBackend,
    },
    input::keyboard::XkbConfig,
};
use tracing::warn;

use crate::state::ShojiWM;

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputConfigUpdate {
    pub config: RuntimeInputConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputConfig {
    pub global: Option<RuntimeInputDeviceConfig>,
    #[serde(default)]
    pub device: BTreeMap<String, Option<RuntimeInputDeviceConfig>>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputDeviceConfig {
    pub keyboard: Option<RuntimeKeyboardInputConfig>,
    pub pointer: Option<RuntimePointerInputConfig>,
    pub touchpad: Option<RuntimeTouchpadInputConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeKeyboardInputConfig {
    pub repeat_rate: Option<i32>,
    pub repeat_delay: Option<i32>,
    pub rules: Option<String>,
    pub model: Option<String>,
    pub layout: Option<String>,
    pub variant: Option<String>,
    pub options: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePointerInputConfig {
    pub pointer_accel: Option<f64>,
    pub accel_profile: Option<RuntimeInputAccelProfile>,
    pub left_handed: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub middle_emulation: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTouchpadInputConfig {
    pub pointer_accel: Option<f64>,
    pub accel_profile: Option<RuntimeInputAccelProfile>,
    pub left_handed: Option<bool>,
    pub natural_scroll: Option<bool>,
    pub middle_emulation: Option<bool>,
    pub tap_to_click: Option<bool>,
    pub tap_button_map: Option<RuntimeInputTapButtonMap>,
    pub click_method: Option<RuntimeInputClickMethod>,
    pub scroll_method: Option<RuntimeInputScrollMethod>,
    pub scroll_factor: Option<f64>,
    pub disable_while_typing: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInputAccelProfile {
    Adaptive,
    Flat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInputClickMethod {
    ButtonAreas,
    Clickfinger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInputScrollMethod {
    #[serde(rename = "none")]
    None,
    TwoFinger,
    Edge,
    OnButtonDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeInputTapButtonMap {
    LeftRightMiddle,
    LeftMiddleRight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputDeviceSnapshot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sysname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<u32>,
    pub kind: RuntimeInputDeviceKindSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInputDeviceKindSnapshot {
    pub keyboard: bool,
    pub pointer: bool,
    pub touchpad: bool,
    pub touch: bool,
    pub tablet_tool: bool,
    pub tablet_pad: bool,
    pub gesture: bool,
    #[serde(rename = "switch")]
    pub switch_device: bool,
}

impl RuntimeInputDeviceConfig {
    fn merge_from(&mut self, other: &RuntimeInputDeviceConfig) {
        if let Some(keyboard) = &other.keyboard {
            self.keyboard
                .get_or_insert_with(RuntimeKeyboardInputConfig::default)
                .merge_from(keyboard);
        }
        if let Some(pointer) = &other.pointer {
            self.pointer
                .get_or_insert_with(RuntimePointerInputConfig::default)
                .merge_from(pointer);
        }
        if let Some(touchpad) = &other.touchpad {
            self.touchpad
                .get_or_insert_with(RuntimeTouchpadInputConfig::default)
                .merge_from(touchpad);
        }
    }
}

impl RuntimeKeyboardInputConfig {
    fn merge_from(&mut self, other: &RuntimeKeyboardInputConfig) {
        self.repeat_rate = other.repeat_rate.or(self.repeat_rate);
        self.repeat_delay = other.repeat_delay.or(self.repeat_delay);
        self.rules = other.rules.clone().or_else(|| self.rules.clone());
        self.model = other.model.clone().or_else(|| self.model.clone());
        self.layout = other.layout.clone().or_else(|| self.layout.clone());
        self.variant = other.variant.clone().or_else(|| self.variant.clone());
        self.options = other.options.clone().or_else(|| self.options.clone());
    }

    fn has_xkb_config(&self) -> bool {
        self.rules.is_some()
            || self.model.is_some()
            || self.layout.is_some()
            || self.variant.is_some()
            || self.options.is_some()
    }
}

impl RuntimePointerInputConfig {
    fn merge_from(&mut self, other: &RuntimePointerInputConfig) {
        self.pointer_accel = other.pointer_accel.or(self.pointer_accel);
        self.accel_profile = other.accel_profile.or(self.accel_profile);
        self.left_handed = other.left_handed.or(self.left_handed);
        self.natural_scroll = other.natural_scroll.or(self.natural_scroll);
        self.middle_emulation = other.middle_emulation.or(self.middle_emulation);
    }
}

impl RuntimeTouchpadInputConfig {
    fn merge_from(&mut self, other: &RuntimeTouchpadInputConfig) {
        self.pointer_accel = other.pointer_accel.or(self.pointer_accel);
        self.accel_profile = other.accel_profile.or(self.accel_profile);
        self.left_handed = other.left_handed.or(self.left_handed);
        self.natural_scroll = other.natural_scroll.or(self.natural_scroll);
        self.middle_emulation = other.middle_emulation.or(self.middle_emulation);
        self.tap_to_click = other.tap_to_click.or(self.tap_to_click);
        self.tap_button_map = other.tap_button_map.or(self.tap_button_map);
        self.click_method = other.click_method.or(self.click_method);
        self.scroll_method = other.scroll_method.or(self.scroll_method);
        self.scroll_factor = other.scroll_factor.or(self.scroll_factor);
        self.disable_while_typing = other.disable_while_typing.or(self.disable_while_typing);
    }

    fn as_pointer_config(&self) -> RuntimePointerInputConfig {
        RuntimePointerInputConfig {
            pointer_accel: self.pointer_accel,
            accel_profile: self.accel_profile,
            left_handed: self.left_handed,
            natural_scroll: self.natural_scroll,
            middle_emulation: self.middle_emulation,
        }
    }
}

pub fn snapshot_for_libinput_device(device: &mut LibinputDevice) -> RuntimeInputDeviceSnapshot {
    let keyboard = device.has_capability(DeviceCapability::Keyboard);
    let pointer = device.has_capability(DeviceCapability::Pointer);
    let touch = device.has_capability(DeviceCapability::Touch);
    let tablet_tool = device.has_capability(DeviceCapability::TabletTool);
    let tablet_pad = device.has_capability(DeviceCapability::TabletPad);
    let gesture = device.has_capability(DeviceCapability::Gesture);
    let switch_device = device.has_capability(DeviceCapability::Switch);
    let touchpad = pointer && gesture && device.config_tap_finger_count() > 0;

    RuntimeInputDeviceSnapshot {
        name: device.name().into_owned(),
        sysname: {
            let sysname = device.sysname().to_string();
            if sysname.is_empty() {
                None
            } else {
                Some(sysname)
            }
        },
        vendor: Some(device.id_vendor()),
        product: Some(device.id_product()),
        kind: RuntimeInputDeviceKindSnapshot {
            keyboard,
            pointer,
            touchpad,
            touch,
            tablet_tool,
            tablet_pad,
            gesture,
            switch_device,
        },
    }
}

pub fn libinput_device_key(device: &mut LibinputDevice) -> String {
    let sysname = device.sysname();
    if sysname.is_empty() {
        device.name().into_owned()
    } else {
        sysname.to_string()
    }
}

pub fn merged_config_for_device(
    config: &RuntimeInputConfig,
    snapshot: &RuntimeInputDeviceSnapshot,
) -> RuntimeInputDeviceConfig {
    let mut merged = RuntimeInputDeviceConfig::default();
    if let Some(global) = &config.global {
        merged.merge_from(global);
    }
    if let Some(Some(device_config)) = config.device.get(&snapshot.name) {
        merged.merge_from(device_config);
    }
    if let Some(sysname) = &snapshot.sysname
        && let Some(Some(device_config)) = config.device.get(sysname)
    {
        merged.merge_from(device_config);
    }
    merged
}

pub fn scroll_factor_for_backend_device<D: SmithayInputDevice>(
    config: &RuntimeInputConfig,
    devices: &BTreeMap<String, RuntimeInputDeviceSnapshot>,
    device: &D,
) -> f64 {
    let Some(snapshot) = snapshot_for_backend_device(devices, device) else {
        return 1.0;
    };
    if !snapshot.kind.touchpad {
        return 1.0;
    }
    let merged = merged_config_for_device(config, snapshot);
    let Some(factor) = merged.touchpad.and_then(|touchpad| touchpad.scroll_factor) else {
        return 1.0;
    };
    if factor.is_finite() {
        factor.clamp(0.0, 20.0)
    } else {
        1.0
    }
}

pub fn snapshot_for_backend_input_device<D: SmithayInputDevice>(
    devices: &BTreeMap<String, RuntimeInputDeviceSnapshot>,
    device: &D,
) -> Option<RuntimeInputDeviceSnapshot> {
    snapshot_for_backend_device(devices, device).cloned()
}

fn snapshot_for_backend_device<'a, D: SmithayInputDevice>(
    devices: &'a BTreeMap<String, RuntimeInputDeviceSnapshot>,
    device: &D,
) -> Option<&'a RuntimeInputDeviceSnapshot> {
    if let Some(syspath) = device.syspath()
        && let Some(file_name) = syspath.file_name().and_then(|name| name.to_str())
        && let Some(snapshot) = devices.get(file_name)
    {
        return Some(snapshot);
    }

    let name = device.name();
    if let Some(snapshot) = devices.values().find(|snapshot| snapshot.name == name) {
        return Some(snapshot);
    }

    let (product, vendor) = device.usb_id()?;
    devices
        .values()
        .find(|snapshot| snapshot.product == Some(product) && snapshot.vendor == Some(vendor))
}

pub fn apply_keyboard_config(state: &mut ShojiWM) {
    let config = &state.runtime_input_config;
    let devices = &state.runtime_input_devices;

    let mut keyboard = RuntimeKeyboardInputConfig::default();
    if let Some(global) = &config.global
        && let Some(global_keyboard) = &global.keyboard
    {
        keyboard.merge_from(global_keyboard);
    }
    for snapshot in devices.values().filter(|snapshot| snapshot.kind.keyboard) {
        let merged = merged_config_for_device(config, snapshot);
        if let Some(device_keyboard) = &merged.keyboard {
            keyboard.merge_from(device_keyboard);
        }
    }

    if let Some(rate) = keyboard.repeat_rate
        && let Some(handle) = state.seat.get_keyboard()
    {
        let delay = keyboard.repeat_delay.unwrap_or(200);
        handle.change_repeat_info(rate.max(0), delay.max(0));
    }

    let active_keyboard = state
        .runtime_active_keyboard_device
        .as_ref()
        .and_then(|snapshot| merged_config_for_device(config, snapshot).keyboard)
        .filter(|keyboard| keyboard.has_xkb_config());

    let global_keyboard = config
        .global
        .as_ref()
        .and_then(|global| global.keyboard.as_ref())
        .filter(|keyboard| keyboard.has_xkb_config())
        .cloned();
    let keyboard = active_keyboard.or(global_keyboard);

    apply_keyboard_xkb_config(state, keyboard, "runtime keyboard xkb config");
}

pub fn apply_keyboard_config_for_backend_device<D: SmithayInputDevice>(
    state: &mut ShojiWM,
    device: &D,
) {
    let snapshot = snapshot_for_backend_device(&state.runtime_input_devices, device).cloned();
    if let Some(snapshot) = &snapshot
        && snapshot.kind.keyboard
    {
        state.runtime_active_keyboard_device = Some(snapshot.clone());
    }
    let global_keyboard = state
        .runtime_input_config
        .global
        .as_ref()
        .and_then(|global| global.keyboard.as_ref())
        .filter(|keyboard| keyboard.has_xkb_config())
        .cloned();
    let keyboard = snapshot
        .as_ref()
        .and_then(|snapshot| keyboard_config_for_snapshot(&state.runtime_input_config, snapshot))
        .filter(|keyboard| keyboard.has_xkb_config())
        .or(global_keyboard);
    apply_keyboard_xkb_config(state, keyboard, "runtime keyboard device xkb config");
}

fn apply_keyboard_xkb_config(
    state: &mut ShojiWM,
    keyboard: Option<RuntimeKeyboardInputConfig>,
    log_label: &'static str,
) {
    if state.runtime_applied_xkb_config == keyboard {
        return;
    }
    let Some(handle) = state.seat.get_keyboard() else {
        return;
    };

    let xkb_config = keyboard
        .as_ref()
        .map_or_else(XkbConfig::default, |keyboard| XkbConfig {
            rules: keyboard.rules.as_deref().unwrap_or(""),
            model: keyboard.model.as_deref().unwrap_or(""),
            layout: keyboard.layout.as_deref().unwrap_or(""),
            variant: keyboard.variant.as_deref().unwrap_or(""),
            options: keyboard.options.clone(),
        });
    match handle.set_xkb_config(state, xkb_config) {
        Ok(()) => {
            state.runtime_applied_xkb_config = keyboard;
            state.refresh_keyboard_focus_for_keymap_change();
        }
        Err(error) => {
            warn!(?error, "{log_label}: failed to apply");
        }
    }
}

fn keyboard_config_for_snapshot(
    config: &RuntimeInputConfig,
    snapshot: &RuntimeInputDeviceSnapshot,
) -> Option<RuntimeKeyboardInputConfig> {
    merged_config_for_device(config, snapshot).keyboard
}

pub fn apply_config_to_libinput_devices(
    config: &RuntimeInputConfig,
    devices: &BTreeMap<String, RuntimeInputDeviceSnapshot>,
    libinput_devices: &mut HashMap<String, LibinputDevice>,
) {
    for (key, device) in libinput_devices {
        let Some(snapshot) = devices.get(key) else {
            continue;
        };
        let merged = merged_config_for_device(config, snapshot);
        if snapshot.kind.pointer
            && let Some(pointer) = &merged.pointer
        {
            apply_pointer_config(device, pointer);
        }
        if snapshot.kind.touchpad
            && let Some(touchpad) = &merged.touchpad
        {
            apply_pointer_config(device, &touchpad.as_pointer_config());
            apply_touchpad_config(device, touchpad);
        }
    }
}

fn apply_pointer_config(device: &mut LibinputDevice, config: &RuntimePointerInputConfig) {
    if let Some(speed) = config.pointer_accel
        && device.config_accel_is_available()
        && let Err(error) = device.config_accel_set_speed(speed.clamp(-1.0, 1.0))
    {
        warn!(?error, "failed to apply pointer acceleration");
    }
    if let Some(profile) = config.accel_profile {
        let profile = match profile {
            RuntimeInputAccelProfile::Adaptive => AccelProfile::Adaptive,
            RuntimeInputAccelProfile::Flat => AccelProfile::Flat,
        };
        if device.config_accel_profiles().contains(&profile)
            && let Err(error) = device.config_accel_set_profile(profile)
        {
            warn!(?error, "failed to apply pointer acceleration profile");
        }
    }
    if let Some(enabled) = config.left_handed
        && device.config_left_handed_is_available()
        && let Err(error) = device.config_left_handed_set(enabled)
    {
        warn!(?error, "failed to apply left-handed input setting");
    }
    if let Some(enabled) = config.natural_scroll
        && device.config_scroll_has_natural_scroll()
        && let Err(error) = device.config_scroll_set_natural_scroll_enabled(enabled)
    {
        warn!(?error, "failed to apply natural scroll setting");
    }
    if let Some(enabled) = config.middle_emulation
        && device.config_middle_emulation_is_available()
        && let Err(error) = device.config_middle_emulation_set_enabled(enabled)
    {
        warn!(?error, "failed to apply middle emulation setting");
    }
}

fn apply_touchpad_config(device: &mut LibinputDevice, config: &RuntimeTouchpadInputConfig) {
    if let Some(enabled) = config.tap_to_click
        && device.config_tap_finger_count() > 0
        && let Err(error) = device.config_tap_set_enabled(enabled)
    {
        warn!(?error, "failed to apply tap-to-click setting");
    }
    if let Some(map) = config.tap_button_map
        && device.config_tap_finger_count() > 0
    {
        let map = match map {
            RuntimeInputTapButtonMap::LeftRightMiddle => TapButtonMap::LeftRightMiddle,
            RuntimeInputTapButtonMap::LeftMiddleRight => TapButtonMap::LeftMiddleRight,
        };
        if let Err(error) = device.config_tap_set_button_map(map) {
            warn!(?error, "failed to apply tap button map");
        }
    }
    if let Some(method) = config.click_method {
        let method = match method {
            RuntimeInputClickMethod::ButtonAreas => ClickMethod::ButtonAreas,
            RuntimeInputClickMethod::Clickfinger => ClickMethod::Clickfinger,
        };
        if device.config_click_methods().contains(&method)
            && let Err(error) = device.config_click_set_method(method)
        {
            warn!(?error, "failed to apply click method");
        }
    }
    if let Some(method) = config.scroll_method {
        let method = match method {
            RuntimeInputScrollMethod::None => ScrollMethod::NoScroll,
            RuntimeInputScrollMethod::TwoFinger => ScrollMethod::TwoFinger,
            RuntimeInputScrollMethod::Edge => ScrollMethod::Edge,
            RuntimeInputScrollMethod::OnButtonDown => ScrollMethod::OnButtonDown,
        };
        if device.config_scroll_methods().contains(&method)
            && let Err(error) = device.config_scroll_set_method(method)
        {
            warn!(?error, "failed to apply scroll method");
        }
    }
    if let Some(enabled) = config.disable_while_typing
        && device.config_dwt_is_available()
        && let Err(error) = device.config_dwt_set_enabled(enabled)
    {
        warn!(?error, "failed to apply disable-while-typing setting");
    }
}

impl ShojiWM {
    pub fn handle_libinput_input_event(&mut self, event: &InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { device } => {
                self.register_libinput_device(device.clone());
            }
            InputEvent::DeviceRemoved { device } => {
                self.unregister_libinput_device(device.clone());
            }
            _ => {}
        }
    }
}
