// A small line icon for what is being dragged, drawn on a 16x16 grid that
// fills the component (centered, keeping its aspect).
//   kind 0: text   1: file   2: image   3: link   4: other data
uniform float kind;
uniform vec4 color;

const float STROKE = 1.5;

float segment(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

float line(vec2 p, vec2 a, vec2 b) {
    return segment(p, a, b) - STROKE * 0.5;
}

float box(vec2 p, vec2 min_corner, vec2 max_corner, float radius) {
    vec2 center = (min_corner + max_corner) * 0.5;
    vec2 half_size = (max_corner - min_corner) * 0.5 - radius;
    vec2 q = abs(p - center) - half_size;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float ring(float d) {
    return abs(d) - STROKE * 0.5;
}

float text_icon(vec2 p) {
    float d = line(p, vec2(3.0, 4.0), vec2(13.0, 4.0));
    d = min(d, line(p, vec2(3.0, 8.0), vec2(13.0, 8.0)));
    return min(d, line(p, vec2(3.0, 12.0), vec2(9.0, 12.0)));
}

float file_icon(vec2 p) {
    // A page with its top-right corner folded.
    float page = box(p, vec2(3.0, 1.5), vec2(13.0, 14.5), 1.0);
    float cut = (p.x - p.y - 7.5) * 0.70710678;
    float d = ring(max(page, cut));
    d = min(d, line(p, vec2(9.0, 1.5), vec2(9.0, 5.5)));
    d = min(d, line(p, vec2(9.0, 5.5), vec2(13.0, 5.5)));
    d = min(d, line(p, vec2(5.5, 9.0), vec2(10.5, 9.0)));
    return min(d, line(p, vec2(5.5, 12.0), vec2(10.5, 12.0)));
}

float image_icon(vec2 p) {
    float d = ring(box(p, vec2(1.5, 2.5), vec2(14.5, 13.5), 2.0));
    d = min(d, line(p, vec2(4.0, 11.0), vec2(7.0, 7.5)));
    d = min(d, line(p, vec2(7.0, 7.5), vec2(10.0, 11.0)));
    d = min(d, line(p, vec2(9.0, 9.8), vec2(10.5, 8.5)));
    d = min(d, line(p, vec2(10.5, 8.5), vec2(12.0, 11.0)));
    return min(d, length(p - vec2(10.5, 5.5)) - 1.3);
}

float link_icon(vec2 p) {
    // Two chain links at 45 degrees.
    float a = ring(segment(p, vec2(4.5, 11.5), vec2(7.5, 8.5)) - 2.3);
    float b = ring(segment(p, vec2(8.5, 7.5), vec2(11.5, 4.5)) - 2.3);
    return min(a, b);
}

float data_icon(vec2 p) {
    float d = box(p, vec2(2.5, 2.5), vec2(7.0, 7.0), 1.2);
    d = min(d, box(p, vec2(9.0, 2.5), vec2(13.5, 7.0), 1.2));
    d = min(d, box(p, vec2(2.5, 9.0), vec2(7.0, 13.5), 1.2));
    return min(d, box(p, vec2(9.0, 9.0), vec2(13.5, 13.5), 1.2));
}

vec4 paint_main(PaintContext ctx) {
    float side = min(ctx.size_phy_px.x, ctx.size_phy_px.y);
    float unit = side / 16.0;
    vec2 origin = (ctx.size_phy_px - vec2(side)) * 0.5;
    vec2 p = (ctx.frag_phy_px - origin) / unit;

    float d;
    if (kind < 0.5) {
        d = text_icon(p);
    } else if (kind < 1.5) {
        d = file_icon(p);
    } else if (kind < 2.5) {
        d = image_icon(p);
    } else if (kind < 3.5) {
        d = link_icon(p);
    } else {
        d = data_icon(p);
    }
    return shoji_premultiply(color) * shoji_coverage(d * unit);
}
