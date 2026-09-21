uniform highp sampler2D silhouette;
uniform highp sampler2D field_input;
// Historical filename: now signed distance in float, not an 8-bit height.
uniform float distance_limit_px;

vec4 shader_main(EffectContext effect) {
    vec2 size = effect.texture_size_px;
    vec2 uv = (floor(effect.texture_uv * size) + 0.5) / size;
    vec4 nearest = texture2D(field_input, uv);
    float distance = nearest.b < 0.5 ? distance_limit_px
        : min(length(nearest.rg), distance_limit_px);
    float mask = texture2D(silhouette, uv).r;
    return vec4(mask >= 0.5 ? distance : -distance, 0.0, 0.0, 1.0);
}
