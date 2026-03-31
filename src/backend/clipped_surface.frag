precision highp float;

uniform float alpha;
varying vec2 v_coords;

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float clip_scale;
uniform vec2 clip_size;
uniform vec4 corner_radius;
uniform float rect_bounds_enabled;
uniform mat3 input_to_clip;

float rounded_alpha(vec2 coords, vec2 size) {
    if (rect_bounds_enabled > 0.5 && (coords.x < 0.0 || coords.y < 0.0 || coords.x > size.x || coords.y > size.y)) {
        return 0.0;
    }
    vec2 half_size = size * 0.5;
    vec2 p = coords - half_size;
    float radius;
    if (p.x >= 0.0) {
        radius = p.y >= 0.0 ? corner_radius.z : corner_radius.y;
    } else {
        radius = p.y >= 0.0 ? corner_radius.w : corner_radius.x;
    }
    vec2 q = abs(p) - (half_size - vec2(radius));
    float dist = min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - radius;
    float half_px = 0.5 / max(abs(clip_scale), 0.0001);
    return 1.0 - smoothstep(-half_px, half_px, dist);
}

void main() {
    vec4 color = texture2D(tex, v_coords);
    vec2 normalized = (input_to_clip * vec3(v_coords, 1.0)).xy;
    vec2 coords = normalized * clip_size;
    color *= rounded_alpha(coords, clip_size);
    gl_FragColor = color * alpha;
}
