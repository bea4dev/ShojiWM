// Wobbly windows, KDE style: a 4x4 grid of spring points bends the window.
// Each point carries how far it is from its rest place (logical px). The
// grid is a bicubic Bezier patch, so the window bends as one smooth sheet.
uniform vec4 d0;  // points 0 and 1 (row 0, columns 0-1): xy, zw
uniform vec4 d1;  // points 2 and 3
uniform vec4 d2;  // row 1
uniform vec4 d3;
uniform vec4 d4;  // row 2
uniform vec4 d5;
uniform vec4 d6;  // row 3
uniform vec4 d7;
uniform float opacity;  // 1 = solid, lower while dragged

// Cubic Bernstein weights for t in 0..1.
vec4 bernstein(float t) {
    float s = 1.0 - t;
    return vec4(s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
}

vec2 row(vec4 a, vec4 b, vec4 w) {
    return a.xy * w.x + a.zw * w.y + b.xy * w.z + b.zw * w.w;
}

// The bend at a point of the window, in logical px.
vec2 bend(vec2 uv) {
    vec4 wu = bernstein(clamp(uv.x, 0.0, 1.0));
    vec4 wv = bernstein(clamp(uv.y, 0.0, 1.0));
    return row(d0, d1, wu) * wv.x
         + row(d2, d3, wu) * wv.y
         + row(d4, d5, wu) * wv.z
         + row(d6, d7, wu) * wv.w;
}

vec4 shader_main(EffectContext effect) {
    vec2 size = effect.frame_rect_phy_px.zw;
    vec2 here = effect_frame_phy_px(effect);

    // Find the source pixel that bends onto this one. Three refinement steps
    // keep even soft, large bends clean.
    vec2 source = here;
    for (int i = 0; i < 3; i++) {
        source = here - bend(source / size) * effect.scale;
    }

    vec2 uv = (effect.frame_rect_phy_px.xy + source) / effect.texture_size_phy_px;
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return vec4(0.0);
    }
    // Premultiplied alpha: fade every channel together.
    return texture2D(tex, uv) * opacity;
}
