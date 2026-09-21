uniform sampler2D layer_mask;
uniform float surface_opacity;

vec4 shader_main(EffectContext effect) {
    float alpha = texture2D(layer_mask, effect.texture_uv).a;
    // QuickShell emits surface_opacity * antialiased geometric coverage.
    // A narrow smoothstep would harden that coverage back into a jagged edge.
    float coverage = clamp(alpha / max(surface_opacity, 0.0001), 0.0, 1.0);
    // R: geometry for distance/clip; G: source alpha for behind compositing.
    // Text inside the filled shape clamps to coverage=1, not a separate contour.
    return vec4(coverage, alpha, 0.0, 1.0);
}
