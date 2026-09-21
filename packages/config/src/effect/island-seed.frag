uniform highp sampler2D mask_input;

float maskAt(vec2 uv) {
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0))))
        return 0.0;
    return texture2D(mask_input, uv).r;
}

vec4 shader_main(EffectContext effect) {
    vec2 uv = effect.texture_uv;
    vec2 pixel = 1.0 / effect.texture_size_px;
    float center = maskAt(uv);
    float left = maskAt(uv - vec2(pixel.x, 0.0));
    float right = maskAt(uv + vec2(pixel.x, 0.0));
    float top = maskAt(uv - vec2(0.0, pixel.y));
    float bottom = maskAt(uv + vec2(0.0, pixel.y));
    float low = min(center, min(min(left, right), min(top, bottom)));
    float high = max(center, max(max(left, right), max(top, bottom)));
    if (low >= 0.5 || high < 0.5)
        return vec4(0.0);
    vec2 gradient = 0.5 * vec2(right - left, bottom - top);
    float magnitude = length(gradient);
    vec2 offset = gradient / max(magnitude, 0.0001)
        * clamp((center - 0.5) / max(magnitude, 0.0001), -1.0, 1.0);
    // Float RG: local boundary offset in pixels; B: valid seed.
    return vec4(-offset, 1.0, 1.0);
}
