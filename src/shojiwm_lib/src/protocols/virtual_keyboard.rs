//! `zwp_virtual_keyboard_manager_v1` (virtual-keyboard-unstable-v1).
//!
//! Smithay's implementation writes a virtual keyboard's keys straight to the
//! focused client, so `wtype` could type text but never reach a key bind.
//! Here every request goes to [`VirtualKeyboardHandler`], which feeds the
//! keys through the compositor's input path with the keymap the client
//! uploaded (see [`crate::virtual_keyboard`]).

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::{
    zwp_virtual_keyboard_manager_v1::{self, ZwpVirtualKeyboardManagerV1},
    zwp_virtual_keyboard_v1::{self, ZwpVirtualKeyboardV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_keyboard::KeymapFormat;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use tracing::debug;

const VERSION: u32 = 1;

pub trait VirtualKeyboardHandler {
    /// The keyboard uploaded a keymap in `XKB_KEYMAP_FORMAT_TEXT_V1`.
    fn virtual_keyboard_keymap(&mut self, keyboard: &ObjectId, keymap: String) -> bool;
    /// `key` is an evdev code in the keyboard's own keymap.
    fn virtual_keyboard_key(
        &mut self,
        keyboard: &ObjectId,
        time_msec: u32,
        key: u32,
        pressed: bool,
    );
    fn virtual_keyboard_modifiers(
        &mut self,
        keyboard: &ObjectId,
        depressed: u32,
        latched: u32,
        locked: u32,
        group: u32,
    );
    fn virtual_keyboard_destroyed(&mut self, keyboard: &ObjectId);
}

#[derive(Debug, Default)]
pub struct VirtualKeyboardData {
    has_keymap: AtomicBool,
}

#[derive(Debug, Default)]
pub struct VirtualKeyboardManagerState;

impl VirtualKeyboardManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwpVirtualKeyboardManagerV1, ()>,
        D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
        D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardData>,
        D: 'static,
    {
        display.create_global::<D, ZwpVirtualKeyboardManagerV1, _>(VERSION, ());
        Self
    }
}

/// The keymap text behind a `keymap` request's fd, without the trailing NUL.
fn read_keymap(file: File, size: u32) -> std::io::Result<String> {
    let mut bytes = vec![0; size as usize];
    file.read_exact_at(&mut bytes, 0)?;
    if let Some(end) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(end);
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

impl<D> GlobalDispatch<ZwpVirtualKeyboardManagerV1, (), D> for VirtualKeyboardManagerState
where
    D: GlobalDispatch<ZwpVirtualKeyboardManagerV1, ()>,
    D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardData>,
    D: VirtualKeyboardHandler,
    D: 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<ZwpVirtualKeyboardManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(manager, ());
    }
}

impl<D> Dispatch<ZwpVirtualKeyboardManagerV1, (), D> for VirtualKeyboardManagerState
where
    D: Dispatch<ZwpVirtualKeyboardManagerV1, ()>,
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardData>,
    D: VirtualKeyboardHandler,
    D: 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _manager: &ZwpVirtualKeyboardManagerV1,
        request: zwp_virtual_keyboard_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        // ShojiWM has one seat, so the requested one is not consulted.
        if let zwp_virtual_keyboard_manager_v1::Request::CreateVirtualKeyboard { seat: _, id } =
            request
        {
            data_init.init(id, VirtualKeyboardData::default());
        }
    }
}

impl<D> Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardData, D> for VirtualKeyboardManagerState
where
    D: Dispatch<ZwpVirtualKeyboardV1, VirtualKeyboardData>,
    D: VirtualKeyboardHandler,
    D: 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        keyboard: &ZwpVirtualKeyboardV1,
        request: zwp_virtual_keyboard_v1::Request,
        data: &VirtualKeyboardData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let id = keyboard.id();
        match request {
            zwp_virtual_keyboard_v1::Request::Keymap { format, fd, size } => {
                if format != KeymapFormat::XkbV1 as u32 {
                    debug!(format, "virtual keyboard keymap in an unsupported format");
                    return;
                }
                let keymap = match read_keymap(File::from(fd), size) {
                    Ok(keymap) => keymap,
                    Err(error) => {
                        debug!(?error, "could not read a virtual keyboard keymap");
                        return;
                    }
                };
                if state.virtual_keyboard_keymap(&id, keymap) {
                    data.has_keymap.store(true, Ordering::Relaxed);
                }
            }
            zwp_virtual_keyboard_v1::Request::Key {
                time,
                key,
                state: key_state,
            } => {
                if !data.has_keymap.load(Ordering::Relaxed) {
                    keyboard.post_error(
                        zwp_virtual_keyboard_v1::Error::NoKeymap,
                        "`key` sent before keymap.",
                    );
                    return;
                }
                state.virtual_keyboard_key(&id, time, key, key_state == 1);
            }
            zwp_virtual_keyboard_v1::Request::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                if !data.has_keymap.load(Ordering::Relaxed) {
                    keyboard.post_error(
                        zwp_virtual_keyboard_v1::Error::NoKeymap,
                        "`modifiers` sent before keymap.",
                    );
                    return;
                }
                state.virtual_keyboard_modifiers(
                    &id,
                    mods_depressed,
                    mods_latched,
                    mods_locked,
                    group,
                );
            }
            zwp_virtual_keyboard_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: ClientId,
        keyboard: &ZwpVirtualKeyboardV1,
        _data: &VirtualKeyboardData,
    ) {
        state.virtual_keyboard_destroyed(&keyboard.id());
    }
}

/// Delegate the `zwp_virtual_keyboard_manager_v1` global to
/// [`VirtualKeyboardManagerState`].
#[macro_export]
macro_rules! delegate_virtual_keyboard {
    ($ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: ()
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1: ()
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);

        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_misc::zwp_virtual_keyboard_v1::server::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1: $crate::protocols::virtual_keyboard::VirtualKeyboardData
        ] => $crate::protocols::virtual_keyboard::VirtualKeyboardManagerState);
    };
}
