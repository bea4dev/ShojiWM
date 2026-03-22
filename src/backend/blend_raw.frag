#version 100

precision mediump float;

uniform sampler2D tex;
uniform sampler2D tex2;
uniform float blend_mode;
uniform float blend_alpha;

varying vec2 v_coords;

vec3 blend_normal(vec3 base, vec3 top) {
    return top;
}

vec3 blend_add(vec3 base, vec3 top) {
    return min(base + top, vec3(1.0));
}

vec3 blend_screen(vec3 base, vec3 top) {
    return 1.0 - (1.0 - base) * (1.0 - top);
}

vec3 blend_multiply(vec3 base, vec3 top) {
    return base * top;
}

void main() {
    vec4 current = texture2D(tex, v_coords);
    vec4 other = texture2D(tex2, v_coords);

    vec3 blended = current.rgb;
    if (blend_mode < 0.5) {
        blended = blend_normal(current.rgb, other.rgb);
    } else if (blend_mode < 1.5) {
        blended = blend_add(current.rgb, other.rgb);
    } else if (blend_mode < 2.5) {
        blended = blend_screen(current.rgb, other.rgb);
    } else {
        blended = blend_multiply(current.rgb, other.rgb);
    }

    current.rgb = mix(current.rgb, blended, clamp(blend_alpha, 0.0, 1.0));
    current.a = max(current.a, other.a * blend_alpha);
    gl_FragColor = current;
}
