uniform float noise_amount;

float hash(vec2 p) {
    p = fract(p * vec2(0.1031, 0.1030));
    p += dot(p, p.yx + 33.33);
    return fract((p.x + p.y) * p.x);
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec4 color = texture2D(tex, uv);
    vec2 pixel = floor(uv * rect_size);
    float cell_size = 3.0;
    vec2 cell = floor(pixel / cell_size);
    vec2 local = mod(pixel, cell_size);

    vec2 salt_pos = floor(vec2(
        hash(cell + vec2(1.0, 0.0)),
        hash(cell + vec2(0.0, 1.0))
    ) * cell_size);

    float cell_area = cell_size * cell_size;
    float density = clamp(noise_amount * cell_area, 0.0, 1.0);
    float active = step(1.0 - density, hash(cell + vec2(7.0, 11.0)));
    float salt = active * (1.0 - step(0.5, distance(local, salt_pos)));

    color.rgb = mix(color.rgb, vec3(1.0), salt * 0.35);
    return color;
}
