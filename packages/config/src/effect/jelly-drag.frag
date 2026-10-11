uniform vec2 lag_px;   // how far the far side trails the window, logical px
uniform vec2 grab_px;  // the grabbed point, logical px from the window's top-left

// Jelly drag: shifts each pixel by the spring lag, more the farther it is from
// the grabbed point. The grabbed point stays under the pointer; the far side
// trails behind and wobbles back.
vec4 shader_main(EffectContext effect) {
    vec2 grab = grab_px * effect.scale;
    vec2 size = effect.frame_rect_phy_px.zw;

    // Full lag at the corner farthest from the grabbed point, none at it.
    float reach = max(length(max(grab, size - grab)), 1.0);
    float weight = clamp(length(effect_frame_phy_px(effect) - grab) / reach, 0.0, 1.0);
    weight = weight * weight * (3.0 - 2.0 * weight);

    vec2 sample_px = effect_texture_phy_px(effect) - lag_px * effect.scale * weight;
    vec2 uv = sample_px / effect.texture_size_phy_px;
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return vec4(0.0);
    }
    return texture2D(tex, uv);
}
