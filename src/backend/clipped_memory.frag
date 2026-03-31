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
