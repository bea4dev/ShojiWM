uniform float phase_01;
uniform float speed;
uniform float radius_px;
uniform float frame_width_px;
uniform float glow_px;
uniform float intensity;

float rounded_rect_sdf(vec2 p, vec2 rect_size, float radius) {
    vec2 q = abs(p - rect_size * 0.5) - (rect_size * 0.5 - vec2(radius));
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float hash21(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec2 px = uv * rect_size;

    float outer = rounded_rect_sdf(px, rect_size, radius_px);
    vec2 inner_origin = vec2(frame_width_px);
    vec2 inner_size = max(rect_size - inner_origin * 2.0, vec2(1.0));
    float inner_radius = max(radius_px - frame_width_px, 0.0);
    float inner = rounded_rect_sdf(px - inner_origin, inner_size, inner_radius);

    float ring = (1.0 - smoothstep(-1.0, 1.0, outer)) * smoothstep(-1.0, 1.0, inner);
    float edge_band = exp(-abs(outer) / max(glow_px, 0.001));

    float t = phase_01 * speed;
    float perimeter_wave = sin(px.x * 0.18 - t * 18.0)
        + sin(px.y * 0.24 + t * 13.0)
        + sin((px.x + px.y) * 0.11 - t * 23.0)
        + sin((px.x - px.y) * 0.15 + t * 29.0);
    float spark_noise = hash21(floor(px * 0.12) + vec2(floor(t * 24.0), floor(t * 13.0)));
    float sparks = pow(clamp(0.5 + perimeter_wave * 0.125 + spark_noise * 0.7, 0.0, 1.0), 5.5);

    float bolt = ring * (0.24 + sparks * 1.9) * intensity;
    float aura = edge_band * (0.12 + sparks * 0.7) * intensity;

    vec3 electric = mix(vec3(0.15, 0.75, 1.0), vec3(0.92, 0.98, 1.0), clamp(sparks * 1.4, 0.0, 1.0));
    vec3 glow = electric * (bolt + aura * 0.65);
    float alpha = clamp(bolt + aura * 0.35, 0.0, 1.0);

    return vec4(glow, alpha);
}
