// Magic Lamp, ported from KWin (kwin/src/plugins/magiclamp/magiclamp.cpp).
//
// KWin cuts the window into a 40x40 grid and moves every grid point towards
// the taskbar entry ("icon"). Here every screen pixel looks for the window
// pixel that KWin's formulas move onto it:
//
// - along the motion axis (y for an icon above or below the window, x for
//   one at the side) KWin's formula only grows, so a binary search finds the
//   source line;
// - across, KWin pulls the line towards the icon by a factor, which is
//   linear, so it is solved directly.
//
// All lengths are logical px, relative to the window frame's top-left.
uniform vec4 icon;    // the icon: x, y, width, height
uniform vec4 params;  // progress (0 = window, 1 = in the icon), side, shadow margin, unused

const float SIDE_TOP = 0.0;
const float SIDE_BOTTOM = 1.0;
const float SIDE_LEFT = 2.0;
const float SIDE_RIGHT = 3.0;

float cube(float v) {
    return v * v * v;
}

// KWin's p_progress: |min(offset / distance, 1)|, without the 0 / 0.
float turn(float offset, float distance) {
    if (abs(distance) < 1e-6) {
        return offset == 0.0 ? 0.0 : 1.0;
    }
    return abs(min(offset / distance, 1.0));
}

// One line of the window, moved: x = where the line lands, y = KWin's
// p_progress (how far the line has turned towards the icon).
//
// `s` is the line's place on the motion axis, `len` the window's length on
// it, `icon_start` and `icon_size` the icon's place and size on it.
// `towards_end` is true for an icon below or right of the window (KWin's
// Bottom and Right branches), false for Top and Left.
vec2 move_line(float s, float len, float icon_start, float icon_size, bool towards_end, float p) {
    if (towards_end) {
        // KWin's Bottom / Right.
        float factor = s + (len - s) * p;
        float offset = (icon_start + s) * p * cube(factor) / cube(len);
        float landed = min(icon_start, s + offset);
        float pp = turn(offset, icon_start - s);
        return vec2(landed, pp);
    }
    // KWin's Top / Left.
    float icon_end = icon_start + icon_size;
    float factor = len - s + s * p;
    float offset = (len - icon_end + s) * p * cube(factor) / cube(len);
    float landed = max(icon_end, s - offset);
    float pp = turn(offset, s - icon_end);
    return vec2(landed, pp);
}

vec4 shader_main(EffectContext effect) {
    float p = params.x;
    float side = params.y;
    float margin = params.z;
    vec2 size = effect.frame_rect_phy_px.zw / effect.scale;
    vec2 here = effect_frame_phy_px(effect) / effect.scale;
    if (p <= 0.0) {
        return texture2D(tex, effect.texture_uv);
    }

    // Work on (motion axis, cross axis).
    bool vertical = side < 1.5;
    vec2 target = vertical ? here.yx : here;
    vec2 len = vertical ? size.yx : size;
    vec4 box = vertical ? icon.yxwz : icon; // start, cross start, size, cross size
    bool towards_end = side == SIDE_BOTTOM || side == SIDE_RIGHT;

    // KWin moves the window's shadow with it: search a little past the frame.
    float lo = -margin;
    float hi = len.x + margin;
    float first = move_line(lo, len.x, box.x, box.z, towards_end, p).x;
    float last = move_line(hi, len.x, box.x, box.z, towards_end, p).x;
    if (target.x < first || target.x > last) {
        return vec4(0.0);
    }
    // The landing place only grows with s: bisect for the line on this pixel.
    for (int i = 0; i < 14; i++) {
        float mid = 0.5 * (lo + hi);
        if (move_line(mid, len.x, box.x, box.z, towards_end, p).x < target.x) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    float s = 0.5 * (lo + hi);
    float pp = move_line(s, len.x, box.x, box.z, towards_end, p).y;

    // Across: landed = c + (box.y + box.w * c / len.y - c) * pp. Solve for c.
    float stretch = 1.0 - pp + pp * box.w / len.y;
    if (stretch < 1e-4) {
        return vec4(0.0);
    }
    float c = (target.y - pp * box.y) / stretch;

    vec2 source = vertical ? vec2(c, s) : vec2(s, c);
    vec2 uv = (effect.frame_rect_phy_px.xy + source * effect.scale) / effect.texture_size_phy_px;
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return vec4(0.0);
    }
    return texture2D(tex, uv);
}
