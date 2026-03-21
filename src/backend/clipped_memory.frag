//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform vec2 element_size;
uniform float render_scale;
uniform float clip_enabled;
uniform vec4 clip_rect;
uniform vec4 clip_radius;

varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

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
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    if (clip_enabled > 0.5) {
        vec2 coords = v_coords * element_size;
        vec2 clip_coords = coords - clip_rect.xy;
        color *= rounded_rect_alpha(clip_coords, clip_rect.zw, clip_radius);
    }

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
