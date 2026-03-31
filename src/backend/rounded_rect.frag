precision highp float;

uniform float alpha;
uniform vec2 size;

uniform vec4 color;
uniform vec4 corner_radius;
uniform float border_width;
uniform float inner_enabled;
uniform vec4 inner_rect;
uniform vec4 inner_radius;
uniform float render_scale;

uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {
    vec2 half_size = rect_size * 0.5;
    vec2 p = coords - half_size;
    float r;
    if (p.x >= 0.0) {
        r = p.y >= 0.0 ? radius.z : radius.y;
    } else {
        r = p.y >= 0.0 ? radius.w : radius.x;
    }
    vec2 q = abs(p) - (half_size - vec2(r));
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
    float half_px = 0.5 / max(abs(render_scale), 0.0001);
    return 1.0 - smoothstep(-half_px, half_px, dist);
}

void main() {
    vec2 coords = v_coords * size;
    float shape_alpha = rounded_rect_alpha(coords, size, corner_radius);

    if (inner_enabled > 0.5 && border_width > 0.0) {
        vec2 outer_origin = inner_rect.xy - vec2(border_width);
        vec2 outer_size = inner_rect.zw + vec2(border_width * 2.0);
        vec4 outer_radius = inner_radius + vec4(border_width);
        float outer_alpha = rounded_rect_alpha(coords - outer_origin, outer_size, outer_radius);
        vec2 inner_coords = coords - inner_rect.xy;
        float inner_alpha = rounded_rect_alpha(inner_coords, inner_rect.zw, inner_radius);
        shape_alpha = max(outer_alpha - inner_alpha, 0.0);
    } else if (inner_enabled > 0.5) {
        vec2 inner_coords = coords - inner_rect.xy;
        float inner_alpha = rounded_rect_alpha(inner_coords, inner_rect.zw, inner_radius);
        shape_alpha = max(shape_alpha - inner_alpha, 0.0);
    } else if (border_width > 0.0) {
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
