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

uniform vec2 half_pixel;
uniform float offset;

varying vec2 v_coords;

void main() {
    vec2 o = half_pixel * offset;

    vec4 sum = vec4(0.0);

    sum += texture2D(tex, v_coords + vec2(-o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2( o.x * 2.0, 0.0));
    sum += texture2D(tex, v_coords + vec2(0.0, -o.y * 2.0));
    sum += texture2D(tex, v_coords + vec2(0.0,  o.y * 2.0));

    sum += texture2D(tex, v_coords + vec2(-o.x,  o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2( o.x,  o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2(-o.x, -o.y)) * 2.0;
    sum += texture2D(tex, v_coords + vec2( o.x, -o.y)) * 2.0;

    gl_FragColor = sum / 12.0;
}
