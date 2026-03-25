uniform float phase_01;
uniform float speed;

vec3 hsv2rgb(vec3 c) {
    vec4 k = vec4(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    vec3 p = abs(fract(c.xxx + k.xyz) * 6.0 - k.www);
    return c.z * mix(k.xxx, clamp(p - k.xxx, 0.0, 1.0), c.y);
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    float hue = fract(uv.x - phase_01 * speed);
    vec3 rainbow = hsv2rgb(vec3(hue, 0.9, 1.0));

    float vertical = 0.85 + (1.0 - abs(uv.y * 2.0 - 1.0)) * 0.15;
    return vec4(rainbow * vertical, 1.0);
}
