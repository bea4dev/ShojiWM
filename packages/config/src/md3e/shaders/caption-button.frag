// One caption button (minimize, maximize or close) in the Material 3
// Expressive style: a glyph, and under it a bubble that grows out of the
// middle when the pointer comes and goes square and wider while pressed.
// The maximize glyph turns into the restore glyph and back; the close glyph
// turns a quarter as its bubble comes. Lengths are logical px unless they end
// in _phy.

uniform float kind;     // 0 minimize, 1 maximize, 2 close
uniform float hover;    // 0..1, a little past 1 while a spring overshoots
uniform float press;    // 0..1
uniform float restore;  // 0: the maximize glyph, 1: the restore glyph
uniform vec4 bubble;    // the bubble, straight alpha
uniform vec4 glyph;     // the glyph away from the bubble
uniform vec4 glyph_on;  // the glyph on the bubble

const float BUBBLE = 28.0;
const float STROKE = 1.5;

float segment(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

// Signed distance to a rounded box around the origin.
float box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// Signed distance to the glyph's stroke, in logical px.
float glyph_distance(vec2 p) {
    float w = STROKE * 0.5;
    if (kind < 0.5) {
        return segment(p, vec2(-5.0, 0.0), vec2(5.0, 0.0)) - w;
    }
    if (kind < 1.5) {
        float t = clamp(restore, 0.0, 1.0);
        // The front square shrinks to the lower left; the one behind comes
        // out from under it to the upper right.
        vec2 front_center = mix(vec2(0.0), vec2(-1.5, 1.5), t);
        float front_half = mix(5.0, 3.75, t);
        float front = abs(box(p - front_center, vec2(front_half), mix(2.0, 1.5, t))) - w;
        vec2 back_center = mix(vec2(0.0), vec2(1.5, -1.5), t);
        float back = abs(box(p - back_center, vec2(front_half), 1.5)) - w;
        // Only the part of it not behind the front square.
        float inside_front = box(p - front_center, vec2(front_half + w * 2.0), 1.5);
        back = max(back, -inside_front);
        return t > 0.001 ? min(front, back + (1.0 - t) * 4.0) : front;
    }
    float turn = clamp(hover, 0.0, 1.2) * 1.5707963;
    vec2 q = mat2(cos(turn), -sin(turn), sin(turn), cos(turn)) * p;
    return min(segment(q, vec2(-4.5), vec2(4.5)), segment(q, vec2(-4.5, 4.5), vec2(4.5, -4.5))) - w;
}

vec4 paint_main(PaintContext ctx) {
    float s = ctx.scale;
    vec2 p = (ctx.frag_phy_px - ctx.size_phy_px * 0.5) / s;
    float shown = clamp(hover, 0.0, 1.0);

    // The bubble: grows with the hover (overshooting with it), and when
    // pressed goes from a circle to a rounded square, a little wider.
    float size = BUBBLE * (0.55 + 0.45 * hover);
    vec2 half_size = vec2(size * 0.5 + 3.0 * press, size * 0.5);
    float radius = mix(size * 0.5, 9.0, press);
    float d = box(p, half_size, min(radius, size * 0.5)) * s;
    vec4 color = shoji_premultiply(bubble) * shoji_coverage(d) * smoothstep(0.0, 0.4, shown);

    vec4 ink = mix(glyph, glyph_on, shown);
    float g = glyph_distance(p) * s;
    return shoji_over(shoji_premultiply(ink) * shoji_coverage(g), color);
}
