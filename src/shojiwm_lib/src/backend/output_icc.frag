#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
// The monitor ICC profile as a 3D LUT (`color::icc`), packed into a 2D texture
// lut_size^2 wide and lut_size high: blue slices side by side, each with red
// across and green down.
uniform sampler2D lut;
uniform float lut_size;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Trilinear lookup: the texture's linear filter interpolates red and green
// inside a blue slice (the half-texel offsets keep it from bleeding into the
// next slice), and the two nearest slices are mixed for blue.
vec3 lut_lookup(vec3 c) {
    vec3 position = clamp(c, 0.0, 1.0) * (lut_size - 1.0);
    float slice = min(floor(position.b), lut_size - 2.0);
    float blend = position.b - slice;
    float width = lut_size * lut_size;
    float x = position.r + 0.5;
    float y = (position.g + 0.5) / lut_size;
    vec3 low = texture2D(lut, vec2((slice * lut_size + x) / width, y)).rgb;
    vec3 high = texture2D(lut, vec2(((slice + 1.0) * lut_size + x) / width, y)).rgb;
    return mix(low, high, blend);
}

void main() {
    // The intermediate holds the finished composite: sRGB-relative,
    // gamma-encoded values, opaque.
    vec4 color = texture2D(tex, v_coords);
    vec4 result = vec4(lut_lookup(color.rgb), 1.0) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
