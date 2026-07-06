//! `wp_color_management_v1` global.
//!
//! Advertises the compositor's color capabilities, exposes per-output image
//! descriptions, and records per-surface parametric image descriptions in
//! the surface `data_map` (same passive pattern as `tearing_control.rs`).
//!
//! Scope: parametric-only (no ICC), perceptual render intent, sRGB always;
//! PQ/BT.2020 entries are added when the `SHOJI_HDR_OUTPUTS` experiment is
//! enabled (`crate::color::hdr_experiment_enabled`).
//!
//! Note: the protocol specifies `set_image_description` as double-buffered
//! (applied on `wl_surface.commit`). We apply it immediately instead — the
//! same simplification `tearing_control.rs` documents; clients set this
//! once before their first commit in practice.

use std::sync::{
    Mutex,
    atomic::{
        AtomicBool,
        AtomicU32,
        Ordering
    },
};

use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_management_output_v1::{
        self,
        WpColorManagementOutputV1
    },
    wp_color_management_surface_feedback_v1::{
        self,
        WpColorManagementSurfaceFeedbackV1
    },
    wp_color_management_surface_v1::{
        self,
        WpColorManagementSurfaceV1
    },
    wp_color_manager_v1::{
        self,
        Feature,
        Primaries,
        RenderIntent,
        TransferFunction,
        WpColorManagerV1,
    },
    wp_image_description_creator_icc_v1::{
        self,
        WpImageDescriptionCreatorIccV1
    },
    wp_image_description_creator_params_v1::{
        self,
        WpImageDescriptionCreatorParamsV1
    },
    wp_image_description_info_v1::{
        self,
        WpImageDescriptionInfoV1
    },
    wp_image_description_v1::{
        self,
        WpImageDescriptionV1
    },
};
use smithay::reexports::wayland_server::{
    Client,
    DataInit,
    Dispatch,
    DisplayHandle,
    GlobalDispatch,
    New,
    Resource,
    WEnum,
    backend::ClientId,
    protocol::{wl_output::WlOutput,
               wl_surface::WlSurface},
};
use smithay::wayland::compositor::with_states;

use crate::color::{
    ColorPrimaries,
    ImageDescription,
    Luminances,
    TransferCharacteristics,
    hdr_experiment_enabled,
};

const VERSION: u32 = 1;

/// Compositor-side lookups the protocol cannot answer from surface state.
pub trait ColorManagementHandler {
    /// The image description of the signal this output is driven with.
    fn output_image_description(&mut self, output: &WlOutput) -> ImageDescription;
    /// The description the compositor prefers for this surface's content.
    fn surface_preferred_description(&mut self, surface: &WlSurface) -> ImageDescription;
    /// Deliver [`send_information`] for `info` *after* the current dispatch
    /// completes (e.g. from a calloop idle callback).
    ///
    /// This MUST NOT send synchronously: `done` is a destructor event, and
    /// wayland-backend (up to at least 0.3.15) stores a newly created
    /// object's data through a raw pointer *after* the request handler
    /// returns, without checking that the object is still alive. Destroying
    /// the info object inside the dispatch that created it is therefore a
    /// write-after-free that corrupts the heap.
    fn defer_image_description_info(
        &mut self,
        info: WpImageDescriptionInfoV1,
        description: ImageDescription,
    );
}

/// Aggregate dispatch bound so each impl below doesn't repeat nine clauses.
pub trait ColorManagementDispatch:
    Dispatch<WpColorManagerV1, ()>
    + Dispatch<WpColorManagementOutputV1, ColorOutputData>
    + Dispatch<WpColorManagementSurfaceV1, ColorSurfaceObjData>
    + Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData>
    + Dispatch<WpImageDescriptionCreatorIccV1, ()>
    + Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData>
    + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
    + Dispatch<WpImageDescriptionInfoV1, ()>
    + ColorManagementHandler
    + 'static
{
}

impl<T> ColorManagementDispatch for T where
    T: Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagementOutputV1, ColorOutputData>
        + Dispatch<WpColorManagementSurfaceV1, ColorSurfaceObjData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData>
        + Dispatch<WpImageDescriptionCreatorIccV1, ()>
        + Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpImageDescriptionInfoV1, ()>
        + ColorManagementHandler
        + 'static
{
}

/// Per-surface color state, stored in the surface's `data_map`.
#[derive(Debug, Default)]
pub struct ColorSurfaceData {
    /// A `wp_color_management_surface_v1` already exists for this surface;
    /// the protocol requires erroring on a second one.
    has_surface_object: AtomicBool,
    /// Committed image description; `None` => untagged (compositor assumes
    /// sRGB).
    description: Mutex<Option<ImageDescription>>,
}

fn with_color_surface_data<T>(
    surface: &WlSurface,
    f: impl FnOnce(&ColorSurfaceData) -> T,
) -> T {
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(ColorSurfaceData::default);
        f(states.data_map.get::<ColorSurfaceData>().unwrap())
    })
}

/// Render-side read: the surface's committed image description, or `None`
/// for untagged content (treat as sRGB).
pub fn surface_image_description(surface: &WlSurface) -> Option<ImageDescription> {
    if !surface.is_alive() {
        return None;
    }
    with_states(surface, |states| {
        states
            .data_map
            .get::<ColorSurfaceData>()
            .and_then(
                |data| *data
                .description
                .lock()
                .unwrap()
            )
    })
}

/// Manager state for the `wp_color_manager_v1` global.
#[derive(Debug)]
pub struct ColorManagementState;

impl ColorManagementState {
    /// Create and advertise the `wp_color_manager_v1` global.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()> + ColorManagementDispatch,
    {
        display.create_global::<D, WpColorManagerV1, _>(VERSION, ());
        Self
    }
}

/// Sent once on bind: parametric-only, perceptual intent, sRGB always;
/// HDR entries only while the experiment gate is on so clients never
/// submit PQ content the render pipeline can't handle yet.
fn send_supported(manager: &WpColorManagerV1) {
    manager.supported_intent(RenderIntent::Perceptual);
    manager.supported_feature(Feature::Parametric);
    manager.supported_primaries_named(Primaries::Srgb);
    manager.supported_tf_named(TransferFunction::Srgb);
    if hdr_experiment_enabled() {
        manager.supported_feature(Feature::SetLuminances);
        manager.supported_primaries_named(Primaries::Bt2020);
        manager.supported_tf_named(TransferFunction::St2084Pq);
        manager.supported_tf_named(TransferFunction::ExtLinear);
    }
    manager.done();
}

/// Monotonic identity for `wp_image_description_v1.ready`. Unique per
/// object is conformant (equal ids must mean identical descriptions;
/// distinct ids carry no meaning).
fn next_identity() -> u32 {
    static NEXT_IDENTITY: AtomicU32 = AtomicU32::new(1);
    NEXT_IDENTITY.fetch_add(
        1,
        Ordering::Relaxed
    )
}

/// Object data for `wp_color_management_output_v1`.
#[derive(Debug)]
pub struct ColorOutputData {
    output: WlOutput,
}

/// Object data for `wp_color_management_surface_v1`.
#[derive(Debug)]
pub struct ColorSurfaceObjData {
    surface: WlSurface,
}

/// Object data for `wp_color_management_surface_feedback_v1`.
#[derive(Debug)]
pub struct FeedbackData {
    surface: WlSurface,
}

/// Object data for `wp_image_description_v1`. `None` marks a failed
/// description (created only to consume the id on an error path).
#[derive(Debug)]
pub struct ImageDescriptionData {
    description: Option<ImageDescription>,
}

/// Accumulator for `wp_image_description_creator_params_v1`.
#[derive(Debug, Default)]
pub struct ParametricCreatorData {
    params: Mutex<CreatorParams>,
}

#[derive(Debug, Default, Clone, Copy)]
struct CreatorParams {
    primaries: Option<ColorPrimaries>,
    tf: Option<TransferCharacteristics>,
    luminances: Option<Luminances>,
    max_cll: Option<u32>,
    max_fall: Option<u32>,
}

/// Initialize a description object, mark it ready, and return it.
fn init_ready_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<WpImageDescriptionV1>,
    description: ImageDescription,
) -> WpImageDescriptionV1
where
    D: Dispatch<WpImageDescriptionV1, ImageDescriptionData> + 'static,
{
    let object = data_init.init(
        id,
        ImageDescriptionData {
            description: Some(description),
        },
    );
    object.ready(next_identity());
    object
}

fn protocol_primaries(primaries: ColorPrimaries) -> Primaries {
    match primaries {
        ColorPrimaries::Srgb => Primaries::Srgb,
        ColorPrimaries::Bt2020 => Primaries::Bt2020,
    }
}

fn protocol_tf(tf: TransferCharacteristics) -> TransferFunction {
    match tf {
        TransferCharacteristics::Srgb => TransferFunction::Srgb,
        TransferCharacteristics::St2084Pq => TransferFunction::St2084Pq,
        TransferCharacteristics::ExtLinear => TransferFunction::ExtLinear,
    }
}

/// Send the full information event burst, ending with the `done` destructor
/// event. Only call this *outside* the dispatch that created `info` — see
/// [`ColorManagementHandler::defer_image_description_info`].
pub fn send_information(
    info: &WpImageDescriptionInfoV1,
    description: &ImageDescription
) {
    let chroma = description.primaries.chromaticities();
    let (r_x, r_y) = chroma.red.to_protocol();
    let (g_x, g_y) = chroma.green.to_protocol();
    let (b_x, b_y) = chroma.blue.to_protocol();
    let (w_x, w_y) = chroma.white.to_protocol();
    info.primaries(
        r_x,
        r_y,
        g_x,
        g_y,
        b_x,
        b_y,
        w_x,
        w_y
    );
    info.primaries_named(
        protocol_primaries(
            description.primaries
        )
    );
    info.tf_named(
        protocol_tf(
            description.tf
        )
    );
    let luminances = description.effective_luminances();
    info.luminances(
        (luminances.min * 10000.0).round() as u32,
        luminances.max.round() as u32,
        luminances.reference.round() as u32,
    );
    // `done` is a destructor event: the info object dies here.
    info.done();
}

impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: GlobalDispatch<WpColorManagerV1, ()> + ColorManagementDispatch,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        manager: New<WpColorManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(
            manager,
            ()
        );
        send_supported(
            &manager
        );
    }
}

impl<D> Dispatch<WpColorManagerV1, (), D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        manager: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_manager_v1::Request::GetOutput {
                id,
                output
            } => {
                data_init.init(
                    id,
                    ColorOutputData {
                        output
                    }
                );
            }
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                // Always init to consume the id, even on the error path.
                let already_present = with_color_surface_data(
                    &surface,
                    |data| {
                    data
                        .has_surface_object
                        .swap(
                            true,
                            Ordering::Relaxed
                        )
                }
                );
                data_init.init(
                    id,
                    ColorSurfaceObjData {
                        surface: surface.clone(),
                    },
                );
                if already_present {
                    manager.post_error(
                        wp_color_manager_v1::Error::SurfaceExists,
                        "wl_surface already has a wp_color_management_surface_v1",
                    );
                }
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback {
                id,
                surface
            } => {
                data_init.init(
                    id,
                    FeedbackData {
                        surface
                    }
                );
            }
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                data_init.init(
                    obj,
                    ()
                );
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "icc_v2_v4 is not supported",
                );
            }
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                data_init.init(
                    obj,
                    ParametricCreatorData::default()
                );
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                data_init.init(
                    image_description,
                    ImageDescriptionData {
                        description: None
                    }
                );
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "windows_scrgb is not supported",
                );
            }
            wp_color_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpColorManagementOutputV1, ColorOutputData, D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _output_obj: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        data: &ColorOutputData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_output_v1::Request::GetImageDescription {
                image_description,
            } => {
                let description = state.output_image_description(&data.output);
                init_ready_description(
                    data_init,
                    image_description,
                    description
                );
            }
            wp_color_management_output_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

fn reset_surface_color(surface: &WlSurface) {
    if !surface.is_alive() {
        return;
    }
    with_color_surface_data(surface, |data| {
        data.has_surface_object.store(false, Ordering::Relaxed);
        *data.description.lock().unwrap() = None;
    });
}

impl<D> Dispatch<WpColorManagementSurfaceV1, ColorSurfaceObjData, D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        surface_obj: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &ColorSurfaceObjData,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                if !data.surface.is_alive() {
                    surface_obj.post_error(
                        wp_color_management_surface_v1::Error::Inert,
                        "the wl_surface has been destroyed",
                    );
                    return;
                }
                if render_intent != WEnum::Value(RenderIntent::Perceptual) {
                    surface_obj.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "unsupported render intent",
                    );
                    return;
                }
                let description = image_description
                    .data::<ImageDescriptionData>()
                    .and_then(|data| data.description);
                let Some(description) = description else {
                    surface_obj.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "image description is not usable",
                    );
                    return;
                };
                with_color_surface_data(&data.surface, |surface_data| {
                    *surface_data.description.lock().unwrap() = Some(description);
                });
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                if data.surface.is_alive() {
                    with_color_surface_data(&data.surface, |surface_data| {
                        *surface_data.description.lock().unwrap() = None;
                    });
                }
            }
            wp_color_management_surface_v1::Request::Destroy => {
                reset_surface_color(&data.surface);
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut D,
        _client: ClientId,
        _surface_obj: &WpColorManagementSurfaceV1,
        data: &ColorSurfaceObjData,
    ) {
        // Safety net for clients that drop the object without an explicit
        // destroy request (e.g. on disconnect).
        reset_surface_color(&data.surface);
    }
}

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData, D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        state: &mut D,
        _client: &Client,
        feedback: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        data: &FeedbackData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_feedback_v1::Request::GetPreferred {
                image_description,
            }
            | wp_color_management_surface_feedback_v1::Request::GetPreferredParametric {
                image_description,
            } => {
                if !data.surface.is_alive() {
                    data_init
                        .init(image_description, ImageDescriptionData { description: None });
                    feedback.post_error(
                        wp_color_management_surface_feedback_v1::Error::Inert,
                        "the wl_surface has been destroyed",
                    );
                    return;
                }
                let description = state.surface_preferred_description(
                    &data.surface
                );
                init_ready_description(
                    data_init,
                    image_description,
                    description
                );
            }
            wp_color_management_surface_feedback_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorIccV1, (), D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _creator: &WpImageDescriptionCreatorIccV1,
        _request: wp_image_description_creator_icc_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // Only reachable after the unsupported_feature error already killed
        // the client; nothing to do.
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, ParametricCreatorData, D>
    for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        creator: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &ParametricCreatorData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_image_description_creator_params_v1::{Error, Request};
        match request {
            Request::SetTfNamed { tf } => {
                let WEnum::Value(named) = tf else {
                    creator.post_error(
                        Error::InvalidTf,
                        "invalid transfer function"
                    );
                    return;
                };
                let mapped = match named {
                    TransferFunction::Srgb => Some(TransferCharacteristics::Srgb),
                    TransferFunction::St2084Pq if hdr_experiment_enabled() => {
                        Some(TransferCharacteristics::St2084Pq)
                    }
                    TransferFunction::ExtLinear if hdr_experiment_enabled() => {
                        Some(TransferCharacteristics::ExtLinear)
                    }
                    _ => None,
                };
                let Some(mapped) = mapped else {
                    creator.post_error(
                        Error::InvalidTf,
                        "unsupported transfer function"
                    );
                    return;
                };
                let mut params = data.params.lock().unwrap();
                if params.tf.replace(mapped).is_some() {
                    creator.post_error(
                        Error::AlreadySet,
                        "transfer function already set"
                    );
                }
            }
            Request::SetPrimariesNamed { primaries } => {
                let WEnum::Value(named) = primaries else {
                    creator.post_error(
                        Error::InvalidPrimariesNamed,
                        "invalid primaries"
                    );
                    return;
                };
                let mapped = match named {
                    Primaries::Srgb => Some(ColorPrimaries::Srgb),
                    Primaries::Bt2020 if hdr_experiment_enabled() => Some(ColorPrimaries::Bt2020),
                    _ => None,
                };
                let Some(mapped) = mapped else {
                    creator.post_error(
                        Error::InvalidPrimariesNamed,
                        "unsupported primaries"
                    );
                    return;
                };
                let mut params = data.params
                    .lock()
                    .unwrap();
                if params.primaries.replace(mapped).is_some() {
                    creator.post_error(
                        Error::AlreadySet,
                        "primaries already set"
                    );
                }
            }
            Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                if !hdr_experiment_enabled() {
                    creator.post_error(
                        Error::UnsupportedFeature,
                        "set_luminances is not supported",
                    );
                    return;
                }
                let luminances = Luminances {
                    min: min_lum as f32 / 10000.0,
                    max: max_lum as f32,
                    reference: reference_lum as f32,
                };
                if luminances.max <= luminances.min || luminances.reference <= luminances.min {
                    creator.post_error(
                        Error::InvalidLuminance,
                        "invalid luminance ordering"
                    );
                    return;
                }
                let mut params = data.params
                    .lock()
                    .unwrap();
                if params.luminances.replace(luminances).is_some() {
                    creator.post_error(
                        Error::AlreadySet,
                        "luminances already set"
                    );
                }
            }
            Request::SetMaxCll { max_cll } => {
                let mut params = data.params
                    .lock()
                    .unwrap();
                if params.max_cll.replace(max_cll).is_some() {
                    creator.post_error(
                        Error::AlreadySet,
                        "max_cll already set"
                    );
                }
            }
            Request::SetMaxFall { max_fall } => {
                let mut params = data.params
                    .lock()
                    .unwrap();
                if params.max_fall.replace(max_fall).is_some() {
                    creator.post_error(
                        Error::AlreadySet,
                        "max_fall already set"
                    );
                }
            }
            Request::SetTfPower { .. } => {
                creator.post_error(
                    Error::UnsupportedFeature,
                    "set_tf_power is not supported"
                );
            }
            Request::SetPrimaries { .. } => {
                creator.post_error(
                    Error::UnsupportedFeature,
                    "set_primaries is not supported"
                );
            }
            Request::SetMasteringDisplayPrimaries { .. } => {
                creator.post_error(
                    Error::UnsupportedFeature,
                    "set_mastering_display_primaries is not supported",
                );
            }
            Request::SetMasteringLuminance { .. } => {
                creator.post_error(
                    Error::UnsupportedFeature,
                    "set_mastering_luminance is not supported",
                );
            }
            Request::Create { image_description } => {
                let params = *data.params.lock().unwrap();
                match (params.primaries, params.tf) {
                    (
                        Some(primaries),
                        Some(tf)
                    ) => {
                        init_ready_description(
                            data_init,
                            image_description,
                            ImageDescription {
                                primaries,
                                tf,
                                luminances: params.luminances,
                                max_cll: params.max_cll,
                                max_fall: params.max_fall,
                            },
                        );
                    }
                    _ => {
                        data_init.init(
                            image_description,
                            ImageDescriptionData {
                                description: None
                            },
                        );
                        creator.post_error(
                            Error::IncompleteSet,
                            "primaries and transfer function are required",
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

impl<D> Dispatch<WpImageDescriptionV1, ImageDescriptionData, D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        description_obj: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &ImageDescriptionData,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_v1::Request::GetInformation { information } => {
                let info = data_init.init(information, ());
                match &data.description {
                    // The info burst ends with the `done` destructor event;
                    // it must not be sent from inside this dispatch (which
                    // created `info`) or wayland-backend writes the new
                    // object's data through a freed pointer afterwards.
                    Some(description) => {
                        state
                            .defer_image_description_info(
                                info,
                                *description,
                            );
                    }
                    None => {
                        description_obj.post_error(
                            wp_image_description_v1::Error::NoInformation,
                            "image description has no information",
                        );
                    }
                }
            }
            wp_image_description_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpImageDescriptionInfoV1, (), D> for ColorManagementState
where
    D: ColorManagementDispatch,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _info: &WpImageDescriptionInfoV1,
        _request: wp_image_description_info_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // The interface has no requests.
    }
}

/// Delegate the `wp_color_management_v1` globals to [`ColorManagementState`].
#[macro_export]
macro_rules! delegate_color_management {
    ($(@<$( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+>)? $ty: ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1: $crate::protocols::color_management::ColorOutputData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1: $crate::protocols::color_management::ColorSurfaceObjData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1: $crate::protocols::color_management::FeedbackData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1: $crate::protocols::color_management::ParametricCreatorData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1: $crate::protocols::color_management::ImageDescriptionData
        ] => $crate::protocols::color_management::ColorManagementState);

        smithay::reexports::wayland_server::delegate_dispatch!($(@< $( $lt $( : $clt $(+ $dlt )* )? ),+ >)? $ty: [
            smithay::reexports::wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1: ()
        ] => $crate::protocols::color_management::ColorManagementState);
    };
}
