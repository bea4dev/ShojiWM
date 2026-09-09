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
// Absolute luminance (cd/m2) that SDR full white maps to on the PQ signal.
uniform float sdr_nits;
// Display gamma assumed for SDR content. See sdr_eotf below.
uniform float sdr_gamma;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// SDR display EOTF: a pure power law.
//
// Deliberately NOT the piecewise IEC 61966-2-1 decode. That curve is the exact
// inverse of the sRGB *encoding* function, whose linear segment near zero exists
// to avoid an infinite slope when encoding. Real SDR displays do not reproduce
// it; they follow approximately a pure gamma, and content is authored by people
// looking at such a display.
//
// The divergence is enormous in the shadows and nil elsewhere. Code 1 decodes to
// 3.035e-4 through the piecewise curve but 5.077e-6 at gamma 2.2 — sixty times
// brighter. By code 128 the two agree to within 1%.
//
// On an SDR panel none of this is visible: its own black floor, typically
// 0.1-0.3 cd/m2, swallows everything below roughly code 6. Mapped into PQ against
// true black it is laid bare. Code 1 landed at PQ 51.8/1023 instead of 6.6, so
// compression noise in dark scenes that was never meant to be seen became plainly
// visible — and coloured, because the lift is per channel, so a (0,3,0) artefact
// pixel went to PQ (51.5, 79.4, 27.9) and read as green. Measured 9/9/2026 on a
// Philips 8505: on a static test pattern every patch except (0,0,0) glowed.
//
// Overridable via SHOJI_SDR_GAMMA; 2.2 is sRGB's nominal display gamma, 2.4 is
// the BT.1886 figure for a dim viewing environment and suits a television.
vec3 sdr_eotf(vec3 c) {
    return pow(max(c, vec3(0.0)), vec3(sdr_gamma));
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
    // The intermediate holds the finished composite as SDR-encoded values.
    vec4 color = texture2D(tex, v_coords);
    vec3 linear = sdr_eotf(clamp(color.rgb, 0.0, 1.0));
    vec3 bt2020 = BT709_TO_BT2020 * linear;
    vec3 pq = pq_inv_eotf(bt2020 * sdr_nits);
    vec4 result = vec4(pq, 1.0) * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif

    gl_FragColor = result;
}
