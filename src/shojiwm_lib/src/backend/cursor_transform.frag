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
// Maps a position in the transformed box (0..1) to the untransformed cursor
// texture (0..1).
uniform mat3 uv_matrix;

varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 uv = (uv_matrix * vec3(v_coords, 1.0)).xy;
    vec2 inside = step(vec2(0.0), uv) * step(uv, vec2(1.0));
    float mask = inside.x * inside.y;
    vec4 color = texture2D(tex, uv);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha * mask;
#else
    color = color * alpha * mask;
#endif

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
