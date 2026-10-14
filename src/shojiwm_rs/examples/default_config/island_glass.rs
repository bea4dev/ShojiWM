//! Port of `packages/config/src/effect/island-glass.ts`: liquid-glass
//! refraction for QuickShell "island" layers, driven only by the layer's
//! alpha silhouette (a jump-flooded distance field gives the rim normal).

use shojiwm_rs::prelude::*;

/// All distances are framebuffer pixels.
struct IslandGlassOptions {
    /// Match LiquidIslandQS `LiquidGroup.tint.a`; required to retain its edge AA.
    surface_opacity: f64,
    rim_width: f64,
    refraction: f64,
    chromatic_shift: f64,
    highlight: f64,
    /// Width of the rim light along the edge.
    highlight_width: f64,
    blur_radius: i32,
    blur_passes: i32,
    blur_mix: f64,
    normal_smoothing: f64,
    edge_softness: f64,
    bevel_width: f64,
    bevel_shadow: f64,
    /// 0: glass, 1: mask, 2: distance, 3: surface gradient.
    debug_view: f64,
}

const OPTIONS: IslandGlassOptions = IslandGlassOptions {
    surface_opacity: 0.20,
    rim_width: 80.0,
    refraction: 80.0,
    chromatic_shift: 1.90,
    highlight: 0.6,
    highlight_width: 2.0,
    blur_radius: 2,
    blur_passes: 1,
    blur_mix: 1.0,
    normal_smoothing: 3.0,
    edge_softness: 2.0,
    bevel_width: 10.0,
    bevel_shadow: 0.0,
    debug_view: 0.0,
};

pub fn island_glass() -> SurfaceEffect {
    // Float local boundary offsets / signed distances keep subpixel precision.
    // Overwritten from the current mask each refresh; no temporal feedback.
    let distance_field = state_texture("island-distance-v2")
        .format(shojiwm_rs::ssd::EffectStateTextureFormat::Rgba16f)
        .scale(1.0)
        .resize(shojiwm_rs::ssd::EffectStateResizePolicy::Clear);

    let smooth = |axis: [f64; 2], smoothing: f64, input: &str| {
        shader_stage("./src/effect/island-smooth.frag")
            .uniform("axis", axis)
            .uniform("smoothing_px", smoothing)
            .texture("field_input", saved(input))
    };

    let direction_smoothing = OPTIONS.normal_smoothing.max(OPTIONS.rim_width * 0.2);

    let mut distance_pipeline = vec![
        // Float-prefilter the silhouette before selecting boundary seeds. The
        // final clip still uses the original mask, so UI edges stay crisp.
        Stage::from(smooth([1.0, 0.0], 1.0, "island-mask")),
        save("island-seed-mask-x"),
        smooth([0.0, 1.0], 1.0, "island-seed-mask-x").into(),
        save("island-seed-mask"),
        shader_stage("./src/effect/island-seed.frag")
            .texture("mask_input", saved("island-seed-mask"))
            .into(),
        save("island-float-step"),
    ];
    for jump in [64, 32, 16, 8, 4, 2, 1, 1] {
        distance_pipeline.push(
            shader_stage("./src/effect/island-jump.frag")
                .uniform("jump_px", jump)
                .texture("field_input", saved("island-float-step"))
                .into(),
        );
        distance_pipeline.push(save("island-float-step"));
    }
    distance_pipeline.extend([
        shader_stage("./src/effect/island-height.frag")
            .texture("silhouette", saved("island-seed-mask"))
            .texture("field_input", saved("island-float-step"))
            .uniform("distance_limit_px", 120.0)
            .into(),
        save("island-float-step"),
        smooth([1.0, 0.0], OPTIONS.normal_smoothing, "island-float-step").into(),
        save("island-float-step"),
        smooth([0.0, 1.0], OPTIONS.normal_smoothing, "island-float-step").into(),
        save("island-float-step"),
        // The bend direction comes from a much smoother copy, which rounds it
        // around corners sharper than the rim.
        smooth([1.0, 0.0], direction_smoothing, "island-float-step").into(),
        save("island-direction"),
        smooth([0.0, 1.0], direction_smoothing, "island-direction").into(),
        save("island-direction"),
        // Where the shape is thinner than the rim, the lens flattens out at
        // its middle instead of creasing there.
        // render_to_if_dirty keeps this final float texture as the state
        // without copying it, so state_source below reads it in the same raw
        // multi-texture coordinate system.
        shader_stage("./src/effect/island-ridge.frag")
            .uniform("rim_width_px", OPTIONS.rim_width)
            .texture("field_input", saved("island-float-step"))
            .texture("direction_input", saved("island-direction"))
            .into(),
    ]);

    let effect = Effect::new(backdrop_source())
        .capture_padding(64)
        .preserve_alpha()
        // A silhouette change affects an entire rim, not just the damaged pixel.
        .invalidate(Invalidate::on_source_damage_box(64))
        .stage(save("island-sharp"))
        .stage(dual_kawase_blur(OPTIONS.blur_radius, OPTIONS.blur_passes))
        .stage(save("island-soft"))
        .stage(save("island-edge-soft"))
        .stage(
            shader_stage("./src/effect/island-mask.frag")
                .texture("layer_mask", layer_source())
                .uniform("surface_opacity", OPTIONS.surface_opacity),
        )
        .stage(save("island-mask"))
        // The distance field depends only on the layer's own silhouette, so it
        // is rebuilt only when the layer's content changes, not whenever the
        // backdrop underneath it does. Its input is already aligned to the
        // padded backdrop; a direct layer source would be stretched.
        .stage(render_to_if_dirty(
            &distance_field,
            &[layer_source()],
            Effect::new(saved("island-mask")).stages(distance_pipeline),
        ))
        .stage(
            shader_stage("./src/effect/island-refract.frag")
                .texture("sharp_scene", saved("island-sharp"))
                .texture("soft_scene", saved("island-soft"))
                .texture("edge_scene", saved("island-edge-soft"))
                .texture("silhouette", saved("island-mask"))
                .texture("distance_field", state_source(&distance_field))
                .uniform("rim_width_px", OPTIONS.rim_width)
                .uniform("refraction_px", OPTIONS.refraction)
                .uniform("chromatic_shift_px", OPTIONS.chromatic_shift)
                .uniform("highlight_strength", OPTIONS.highlight)
                .uniform("highlight_width_px", OPTIONS.highlight_width)
                .uniform("debug_view", OPTIONS.debug_view)
                .uniform("blur_mix", OPTIONS.blur_mix)
                .uniform("edge_softness_px", OPTIONS.edge_softness)
                .uniform("bevel_width_px", OPTIONS.bevel_width)
                .uniform("bevel_shadow", OPTIONS.bevel_shadow),
        );

    // The surface is fixed-size and mostly transparent; its input mask follows
    // the silhouettes, so only that part (plus the smooth-min bulge where
    // shapes merge) is captured and processed.
    SurfaceEffect::new(effect)
        .region(EffectRegion::Input)
        .outsets(40)
}
