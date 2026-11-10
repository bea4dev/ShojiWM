// The glyph of a drag's action badge, centered on the badge by its geometry
// rather than by a font's metrics. Lengths are logical px from the middle.
//   kind 0: + copy   1: → move   2: ? ask   3: × refused
uniform float kind;
uniform vec4 color;  // straight alpha

const float STROKE = 2.2;

float segment(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

float question(vec2 p) {
    // Its middle, from the top of the hook to the bottom of the dot, on the badge's.
    p.y += 0.4;
    vec2 c = vec2(0.0, -2.2);
    float r = 3.2;
    vec2 q = p - c;
    float a = atan(q.y, q.x);
    // The hook: from the left, over the top, round to below the right.
    vec2 end = c + r * vec2(0.70710678);
    float hook = (a <= 0.78539816)
        ? abs(length(q) - r)
        : min(length(p - (c + vec2(-r, 0.0))), length(p - end));
    float d = min(hook, segment(p, end, vec2(0.0, 1.5)));
    d = min(d, segment(p, vec2(0.0, 1.5), vec2(0.0, 2.6)));
    d -= STROKE * 0.5;
    float dot_ = length(p - vec2(0.0, 5.3)) - 1.35;
    return min(d, dot_);
}

float glyph(vec2 p) {
    float w = STROKE * 0.5;
    if (kind < 0.5) {
        return min(segment(p, vec2(-5.0, 0.0), vec2(5.0, 0.0)),
                   segment(p, vec2(0.0, -5.0), vec2(0.0, 5.0))) - w;
    }
    if (kind < 1.5) {
        float d = segment(p, vec2(-5.0, 0.0), vec2(5.0, 0.0));
        d = min(d, segment(p, vec2(5.0, 0.0), vec2(1.0, -4.0)));
        d = min(d, segment(p, vec2(5.0, 0.0), vec2(1.0, 4.0)));
        return d - w;
    }
    if (kind < 2.5) {
        return question(p);
    }
    return min(segment(p, vec2(-4.0, -4.0), vec2(4.0, 4.0)),
               segment(p, vec2(-4.0, 4.0), vec2(4.0, -4.0))) - w;
}

vec4 paint_main(PaintContext ctx) {
    float s = ctx.scale;
    vec2 p = (ctx.frag_phy_px - ctx.size_phy_px * 0.5) / s;
    return shoji_premultiply(color) * shoji_coverage(glyph(p) * s);
}
