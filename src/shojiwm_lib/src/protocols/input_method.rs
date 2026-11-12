//! `zwp_input_method_manager_v2`, created here instead of by smithay's
//! `InputMethodManagerState` so the compositor knows which clients run an
//! input method. Smithay keeps the input method's client to itself, and keys
//! that client sends back through its own virtual keyboard must not go into
//! its keyboard grab again (see [`crate::virtual_keyboard`]). Every request
//! is still handled by smithay.

use smithay::reexports::wayland_protocols_misc::zwp_input_method_v2::server::zwp_input_method_manager_v2::{
    self, ZwpInputMethodManagerV2,
};
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, New};
use smithay::wayland::{Dispatch2, GlobalData, GlobalDispatch2};

use crate::state::ShojiWM;

/// The version smithay implements.
const VERSION: u32 = 1;

pub struct InputMethodManagerGlobal;

pub struct InputMethodManagerData;

impl InputMethodManagerGlobal {
    pub fn create(dh: &DisplayHandle) {
        dh.create_global::<ShojiWM, ZwpInputMethodManagerV2, _>(VERSION, Self);
    }
}

impl GlobalDispatch2<ZwpInputMethodManagerV2, ShojiWM> for InputMethodManagerGlobal {
    fn bind(
        &self,
        _state: &mut ShojiWM,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpInputMethodManagerV2>,
        data_init: &mut DataInit<'_, ShojiWM>,
    ) {
        data_init.init(resource, InputMethodManagerData);
    }
}

impl Dispatch2<ZwpInputMethodManagerV2, ShojiWM> for InputMethodManagerData {
    fn request(
        &self,
        state: &mut ShojiWM,
        client: &Client,
        manager: &ZwpInputMethodManagerV2,
        request: zwp_input_method_manager_v2::Request,
        dh: &DisplayHandle,
        data_init: &mut DataInit<'_, ShojiWM>,
    ) {
        if matches!(
            request,
            zwp_input_method_manager_v2::Request::GetInputMethod { .. }
        ) {
            let backend = dh.backend_handle();
            state
                .input_method_clients
                .retain(|id| backend.get_client_data(id.clone()).is_ok());
            state.input_method_clients.insert(client.id());
        }
        GlobalData.request(state, client, manager, request, dh, data_init);
    }
}
