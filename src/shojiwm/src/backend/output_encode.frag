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
// Absolute luminance (cd/m2) that sRGB full white maps to on the PQ signal.
uniform float sdr_nits;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// sRGB EOTF (IEC 61966-2-1 piecewise decode).
vec3 srgb_eotf(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((c + vec3(0.055)) / 1.055, vec3(2.4));
    return mix(hi, lo, vec3(lessThanEqual(c, vec3(0.04045))));
}

// BT.709 -> BT.2020 linear-light gamut matrix (BT.2087), column-major.
// Cross-checked against the CPU derivation in color/colorimetry.rs tests.
const mat3 BT709_TO_BT2020 = mat3(
    0.627404, 0.069097, 0.016391,
    0.329283, 0.919540, 0.088013,
    0.043313, 0.011362, 0.895595
);

// SMPTE ST 2084 (PQ) inverse EOTF: absolute luminance -> PQ signal.
vec3 pq_inv_eotf(vec3 nits) {
    const float m1 = 0.1593017578125;  // 1305/8192
    const float m2 = 78.84375;         // 2523/32
    const float c1 = 0.8359375;        // 107/128
    const float c2 = 18.8515625;       // 2413/128
    const float c3 = 18.6875;          // 2392/128
    vec3 y = clamp(nits / 10000.0, 0.0, 1.0);
    vec3 ym = pow(y, vec3(m1));
    return pow((vec3(c1) + c2 * ym) / (vec3(1.0) + c3 * ym), vec3(m2));
}

void main() {
    // The intermediate holds the finished composite as sRGB-encoded values.
    vec4 color = texture2D(tex, v_coords);
    vec3 linear = srgb_eotf(clamp(color.rgb, 0.0, 1.0));
    vec3 bt2020 = BT709_TO_BT2020 * linear;
    vec3 pq = pq_inv_eotf(bt2020 * sdr_nits);
    vec4 result = vec4(pq, 1.0) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
