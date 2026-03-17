precision highp float;

uniform float alpha;
uniform vec2 size;

uniform vec4 color;
uniform vec4 corner_radius;
uniform float border_width;
uniform float render_scale;

uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {
    if (coords.x < 0.0 || coords.y < 0.0 || coords.x > rect_size.x || coords.y > rect_size.y) {
        return 0.0;
    }

    vec2 center;
    float r;

    if (coords.x < radius.x && coords.y < radius.x) {
        r = radius.x;
        center = vec2(r, r);
    } else if (coords.x > rect_size.x - radius.y && coords.y < radius.y) {
        r = radius.y;
        center = vec2(rect_size.x - r, r);
    } else if (coords.x > rect_size.x - radius.z && coords.y > rect_size.y - radius.z) {
        r = radius.z;
        center = vec2(rect_size.x - r, rect_size.y - r);
    } else if (coords.x < radius.w && coords.y > rect_size.y - radius.w) {
        r = radius.w;
        center = vec2(r, rect_size.y - r);
    } else {
        return 1.0;
    }

    float dist = distance(coords, center);
    float half_px = 0.5 / max(render_scale, 1.0);
    return 1.0 - smoothstep(r - half_px, r + half_px, dist);
}

void main() {
    vec2 coords = v_coords * size;
    float shape_alpha = rounded_rect_alpha(coords, size, corner_radius);

    if (border_width > 0.0) {
        vec2 inner_size = max(size - vec2(border_width * 2.0), vec2(0.0));
        vec2 inner_coords = coords - vec2(border_width);
        vec4 inner_radius = max(corner_radius - vec4(border_width), vec4(0.0));
        float inner_alpha = rounded_rect_alpha(inner_coords, inner_size, inner_radius);
        shape_alpha = max(shape_alpha - inner_alpha, 0.0);
    }

    if (clip_enabled > 0.5) {
        vec2 clip_coords = coords - clip_rect.xy;
        shape_alpha *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }

    gl_FragColor = color * alpha * shape_alpha;
}
