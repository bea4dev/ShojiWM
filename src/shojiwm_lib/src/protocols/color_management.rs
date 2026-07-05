//! wp_color_management_v1: advertises per-output image descriptions and
//! records per-surface parametric image descriptions in the surface data_map.

use smithay::reexports::wayland_protocols::wp::color_management::v1::server::{
    wp_color_manager_v1::{self, WpColorManagerV1, RenderIntent, Feature, Primaries, TransferFunction},
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
    wp_image_description_v1::WpImageDescriptionV1,
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
};
use crate::color::{ImageDescription, TransferCharacteristics, ColorPrimaries};

const VERSION: u32 = 1;

/// Per-surface color state, stored in the surface's data_map
/// (same pattern as TearingControlSurfaceData in tearing_control.rs).
#[derive(Debug, Default)]
pub struct ColorSurfaceData {
    /// Committed image description; None => compositor assumes sRGB.
    pub description: Mutex<Option<ImageDescription>>,
}

pub struct ColorManagementState;

impl ColorManagementState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagementSurfaceV1, ColorSurfaceObjData>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, FeedbackData>
        + Dispatch<WpImageDescriptionV1, ImageDescriptionData>
        + Dispatch<WpImageDescriptionCreatorParamsV1, CreatorState>
        + 'static,
    {
        display.create_global::<D, WpColorManagerV1, _>(VERSION, ());
        Self
    }
}

/// Advertised on bind: parametric-only, no ICC (matches GLES pipeline scope).
pub fn send_supported(mgr: &WpColorManagerV1) {
    mgr.supported_feature(Feature::Parametric);
    mgr.supported_feature(Feature::SetPrimaries);
    mgr.supported_feature(Feature::SetLuminances);
    mgr.supported_rendering_intent(RenderIntent::Perceptual);
    mgr.supported_primaries_named(Primaries::Srgb);
    mgr.supported_primaries_named(Primaries::Bt2020);
    mgr.supported_tf_named(TransferFunction::Srgb);
    mgr.supported_tf_named(TransferFunction::St2084Pq);
    mgr.supported_tf_named(TransferFunction::ExtLinear);
    mgr.done();
}

/// Render-side read, mirroring surface_prefers_tearing (tearing_control.rs:71).
pub fn surface_image_description(surface: &WlSurface) -> ImageDescription {
    with_states(surface, |states| {
        states.data_map.get::<ColorSurfaceData>()
            .and_then(|d| d.description.lock().unwrap().clone())
            .unwrap_or(ImageDescription::SRGB)
    })
}