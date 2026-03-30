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
    vec2 center;
    float radius;

    if (rect_bounds_enabled > 0.5 && (coords.x < 0.0 || coords.y < 0.0 || coords.x > size.x || coords.y > size.y)) {
        return 0.0;
    }

    if (coords.x < corner_radius.x && coords.y < corner_radius.x) {
        radius = corner_radius.x;
        center = vec2(radius, radius);
    } else if (coords.x > size.x - corner_radius.y && coords.y < corner_radius.y) {
        radius = corner_radius.y;
        center = vec2(size.x - radius, radius);
    } else if (coords.x > size.x - corner_radius.z && coords.y > size.y - corner_radius.z) {
        radius = corner_radius.z;
        center = vec2(size.x - radius, size.y - radius);
    } else if (coords.x < corner_radius.w && coords.y > size.y - corner_radius.w) {
        radius = corner_radius.w;
        center = vec2(radius, size.y - radius);
    } else {
        return 1.0;
    }

    float dist = distance(coords, center);
    float half_px = 0.5 / max(abs(clip_scale), 0.0001);
    return 1.0 - smoothstep(radius - half_px, radius + half_px, dist);
}

void main() {
    vec4 color = texture2D(tex, v_coords);
    vec2 normalized = (input_to_clip * vec3(v_coords, 1.0)).xy;
    vec2 coords = normalized * clip_size;
    color *= rounded_alpha(coords, clip_size);
    gl_FragColor = color * alpha;
}
