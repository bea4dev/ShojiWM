uniform float phase_01;
uniform float speed;
uniform float radius_px;
uniform float frame_width_px;
uniform float glow_px;
uniform float intensity;
uniform float noise_scale;
uniform float edge_width;
uniform float noise_seed;

float rounded_rect_sdf(vec2 p, vec2 rect_size, float radius) {
    vec2 q = abs(p - rect_size * 0.5) - (rect_size * 0.5 - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

float value_noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);

    float a = hash12(i + vec2(0.0, 0.0));
    float b = hash12(i + vec2(1.0, 0.0));
    float c = hash12(i + vec2(0.0, 1.0));
    float d = hash12(i + vec2(1.0, 1.0));

    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

float combined_noise(vec2 noise_uv) {
    vec2 seed_offset = vec2(noise_seed * 17.0, noise_seed * -23.0);
    float coarse = value_noise(noise_uv + seed_offset);
    float fine = value_noise(noise_uv * 3.7 + vec2(17.0, -9.0) + seed_offset * 1.7);
    return coarse * 0.82 + fine * 0.18;
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec2 px = uv * rect_size;

    float outer = rounded_rect_sdf(px, rect_size, radius_px);
    vec2 inner_origin = vec2(frame_width_px);
    vec2 inner_size = max(rect_size - inner_origin * 2.0, vec2(1.0));
    float inner_radius = max(radius_px - frame_width_px, 0.0);
    float inner = rounded_rect_sdf(px - inner_origin, inner_size, inner_radius);

    float ring = (1.0 - smoothstep(-1.0, 1.0, outer)) * smoothstep(-1.0, 1.0, inner);

    float threshold = clamp(0.5 + (edge_width - 1.0) * 0.12, 0.05, 0.95);
    float t = phase_01 * speed;
    float scale = exp2(clamp(noise_scale, 0.0, 1.0) * 8.0 - 4.0);
    vec2 noise_uv = (uv * 2.0 - 1.0) * scale * 12.0 + vec2(t * 2.0, -t * 1.5);
    float n = combined_noise(noise_uv);
    float dist = abs(n - threshold);
    float deriv = max(0.0025, 0.018 / max(scale, 0.0001));

    // Thin contour line around the threshold.
    float line = 1.0 - smoothstep(0.0, deriv * max(edge_width, 0.05) * 4.6, dist);
    // Wider continuous aura around the same contour, not shifted copies.
    float aura = 1.0 - smoothstep(0.0, deriv * (6.0 + glow_px * 0.5), dist);
    aura = max(aura - line, 0.0);

    line *= ring;
    aura *= ring;

    float alpha = clamp((line + aura * 0.45) * intensity, 0.0, 1.0);
    vec3 color = vec3(line + aura * 0.6);

    return vec4(color, alpha);
}
