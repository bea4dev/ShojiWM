// Rounded-rect liquid glass based on signed distance.
// For each pixel inside the rounded rect, compute the SDF distance to the border,
// then sample from a point shifted toward the center. The shift is strongest near
// the edge and fades toward the interior.

uniform float inset_px;
uniform float border_radius_px;
uniform float edge_width_px;
uniform float edge_softness_px;
uniform float max_warp_px;
uniform float interior_warp_px;
uniform float white_tint;
uniform float edge_highlight;

float rounded_rect_sdf(vec2 coords, vec2 rect_size, float radius) {
    vec2 center;
    float r;

    if (coords.x < radius && coords.y < radius) {
        r = radius;
        center = vec2(r, r);
        return distance(coords, center) - r;
    } else if (coords.x > rect_size.x - radius && coords.y < radius) {
        r = radius;
        center = vec2(rect_size.x - r, r);
        return distance(coords, center) - r;
    } else if (coords.x > rect_size.x - radius && coords.y > rect_size.y - radius) {
        r = radius;
        center = vec2(rect_size.x - r, rect_size.y - r);
        return distance(coords, center) - r;
    } else if (coords.x < radius && coords.y > rect_size.y - radius) {
        r = radius;
        center = vec2(r, rect_size.y - r);
        return distance(coords, center) - r;
    }

    float dist_left = coords.x;
    float dist_right = rect_size.x - coords.x;
    float dist_top = coords.y;
    float dist_bottom = rect_size.y - coords.y;
    return -min(min(dist_left, dist_right), min(dist_top, dist_bottom));
}

float rounded_rect_alpha(vec2 p, vec2 rect_size, float radius) {
    float sdf = rounded_rect_sdf(p, rect_size, radius);
    return 1.0 - smoothstep(-0.5, 0.5, sdf);
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec2 origin = vec2(inset_px);
    vec2 size = max(rect_size - vec2(inset_px * 2.0), vec2(1.0));
    vec2 local = uv * rect_size - origin;
    float radius = max(border_radius_px - inset_px, 0.0);

    vec4 base = texture2D(tex, uv);

    float sdf = rounded_rect_sdf(local, size, radius);
    float mask = rounded_rect_alpha(local, size, radius);
    if (mask <= 0.0) {
        return base;
    }

    float dist_inside = max(-sdf, 0.0);
    float edge_band = 1.0 - smoothstep(
        edge_softness_px,
        edge_width_px + edge_softness_px,
        dist_inside
    );
    float interior_band = smoothstep(
        edge_width_px + edge_softness_px,
        edge_width_px + edge_softness_px + 24.0,
        dist_inside
    );

    vec2 center = size * 0.5;
    vec2 to_center = center - local;
    float to_center_len = max(length(to_center), 0.0001);
    vec2 dir = to_center / to_center_len;

    float edge_weight = pow(clamp(edge_band, 0.0, 1.0), 0.65);
    float warp_px = edge_weight * max_warp_px + interior_band * interior_warp_px;
    vec2 warped_local = local + dir * warp_px;
    vec2 warped_uv = clamp((warped_local + origin) / rect_size, vec2(0.0), vec2(1.0));

    vec4 refracted = texture2D(tex, warped_uv);
    vec4 color = refracted;

    float edge_light = edge_band * edge_highlight;
    color.rgb = mix(color.rgb, vec3(1.0), white_tint + edge_light);
    color.a = 1.0;

    return vec4(mix(base.rgb, color.rgb, mask), 1.0);
}
