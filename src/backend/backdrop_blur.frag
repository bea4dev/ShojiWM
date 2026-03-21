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
uniform vec2 texel_step;
uniform float radius;

varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    vec2 step = texel_step * radius;

    vec4 color = vec4(0.0);
    color += texture2D(tex, v_coords + vec2(-step.x, -step.y)) * 0.0625;
    color += texture2D(tex, v_coords + vec2(0.0, -step.y)) * 0.125;
    color += texture2D(tex, v_coords + vec2(step.x, -step.y)) * 0.0625;

    color += texture2D(tex, v_coords + vec2(-step.x, 0.0)) * 0.125;
    color += texture2D(tex, v_coords) * 0.25;
    color += texture2D(tex, v_coords + vec2(step.x, 0.0)) * 0.125;

    color += texture2D(tex, v_coords + vec2(-step.x, step.y)) * 0.0625;
    color += texture2D(tex, v_coords + vec2(0.0, step.y)) * 0.125;
    color += texture2D(tex, v_coords + vec2(step.x, step.y)) * 0.0625;

    color *= alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
