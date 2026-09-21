uniform vec2 axis;
uniform float smoothing_px;
uniform highp sampler2D field_input;

vec4 shader_main(EffectContext effect) {
    vec2 uv = effect.texture_uv;
    vec2 delta = smoothing_px * axis / effect.texture_size_px;
    vec4 center = texture2D(field_input, uv);
    float distance = center.r * 0.375;
    distance += (texture2D(field_input, uv - delta).r + texture2D(field_input, uv + delta).r) * 0.25;
    distance += (texture2D(field_input, uv - delta * 2.0).r + texture2D(field_input, uv + delta * 2.0).r) * 0.0625;
    return vec4(distance, 0.0, 0.0, 1.0);
}
