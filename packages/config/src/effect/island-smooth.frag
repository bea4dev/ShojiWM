uniform vec2 axis;
uniform float smoothing_px;
uniform highp sampler2D field_input;

// The texture ends where the capture is cut off, e.g. at the edge of the
// screen, while the field goes on past it. Continue it there with the slope it
// has at the edge (an odd reflection) rather than repeating the edge value:
// a flat field past the edge bends the smoothed gradient along that edge, and
// near a corner the lens direction turns with it.
float fieldAt(vec2 uv, vec2 size) {
    vec2 low = 0.5 / size;
    vec2 high = 1.0 - low;
    vec2 edge = clamp(uv, low, high);
    if (edge == uv)
        return texture2D(field_input, uv).r;
    return 2.0 * texture2D(field_input, edge).r
        - texture2D(field_input, clamp(2.0 * edge - uv, low, high)).r;
}

vec4 shader_main(EffectContext effect) {
    vec2 uv = effect.texture_uv;
    vec2 size = effect.texture_size_phy_px;
    vec2 delta = smoothing_px * axis / size;
    float distance = fieldAt(uv, size) * 0.375;
    distance += (fieldAt(uv - delta, size) + fieldAt(uv + delta, size)) * 0.25;
    distance += (fieldAt(uv - delta * 2.0, size) + fieldAt(uv + delta * 2.0, size)) * 0.0625;
    return vec4(distance, 0.0, 0.0, 1.0);
}
