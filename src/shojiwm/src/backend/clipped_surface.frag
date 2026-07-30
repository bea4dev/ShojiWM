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
uniform vec2 slot_size;
uniform vec2 slot_origin;
uniform vec2 mask_size;
uniform vec2 mask_origin;
uniform vec4 corner_radius;
uniform float rect_bounds_enabled;
uniform mat3 input_to_local;
uniform vec2 sample_uv_tl;
uniform vec2 sample_uv_br;
uniform vec2 adjusted_sample_uv_br;
uniform vec2 sample_buffer_size;
uniform vec2 sample_uv_snap_axes;
uniform float sample_uv_compensation_enabled;

// Per-surface color management. Compositing happens on sRGB-encoded BT.709
// values, so content tagged with any other transfer/primaries via
// `wp_color_management_v1` has to be converted on the way in — otherwise it is
// decoded as if it were sRGB (which it never was) and again by
// `output_encode.frag`, which is what makes PQ video look washed out.
//
// This is the SDR-correct conversion: HDR content is tone-mapped down into the
// compositing range rather than carried at HDR luminance, because the encode
// pass clamps to [0,1] before its own transfer. Carrying real HDR through
// needs linear compositing and is a separate change.
//
// 0 = passthrough (untagged, already sRGB/BT.709), 1 = ST 2084 PQ, 2 = extended linear.
uniform float src_transfer;
// 0 = BT.709/sRGB primaries, 1 = BT.2020.
uniform float src_primaries;
// Content reference white and peak in cd/m², from the surface's `Luminances`
// (or the protocol's per-transfer-function defaults when unset).
uniform float src_ref_nits;
uniform float src_max_nits;

// BT.2020 -> BT.709 linear-light gamut matrix, column-major. Inverse of the
// BT.2087 matrix in `output_encode.frag`; both are cross-checked against the
// CPU derivation in `color/colorimetry.rs`.
const mat3 BT2020_TO_BT709 = mat3(
     1.660491, -0.124550, -0.018151,
    -0.587641,  1.132900, -0.100579,
    -0.072850, -0.008349,  1.118730
);

// SMPTE ST 2084 (PQ) EOTF: PQ signal -> absolute cd/m². Inverse of
// `pq_inv_eotf` in `output_encode.frag`.
vec3 pq_eotf(vec3 e) {
    const float m1 = 0.1593017578125;  // 1305/8192
    const float m2 = 78.84375;         // 2523/32
    const float c1 = 0.8359375;        // 107/128
    const float c2 = 18.8515625;       // 2413/128
    const float c3 = 18.6875;          // 2392/128
    vec3 ep = pow(clamp(e, 0.0, 1.0), vec3(1.0 / m2));
    vec3 num = max(ep - vec3(c1), vec3(0.0));
    vec3 den = max(vec3(c2) - c3 * ep, vec3(0.000001));
    return 10000.0 * pow(num / den, vec3(1.0 / m1));
}

// sRGB inverse EOTF (IEC 61966-2-1 piecewise encode).
vec3 srgb_inv_eotf(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return mix(hi, lo, vec3(lessThanEqual(c, vec3(0.0031308))));
}

// Roll the content's luminance range down so its reference white lands on
// compositing-space 1.0. Extended Reinhard: linear near the reference white,
// asymptotically approaching 1.0 at the content peak. Deliberately simple —
// tone mapping is where perceptual quality lives and this is the knob to
// iterate on with real content.
vec3 tonemap_to_sdr(vec3 nits) {
    float ref_nits = max(src_ref_nits, 0.0001);
    float peak = max(src_max_nits, ref_nits) / ref_nits;
    vec3 n = nits / ref_nits;
    return n * (vec3(1.0) + n / vec3(peak * peak)) / (vec3(1.0) + n);
}

// sRGB EOTF (IEC 61966-2-1 piecewise decode).
vec3 srgb_eotf(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((max(c, vec3(0.0)) + vec3(0.055)) / 1.055, vec3(2.4));
    return mix(hi, lo, vec3(lessThanEqual(c, vec3(0.04045))));
}

// Convert one sampled, *unpremultiplied* texel into the compositing space.
vec3 to_compositing_space(vec3 c) {
    vec3 linear;
    if (src_transfer > 2.5) {
        // sRGB transfer, but non-sRGB primaries — decode only so the gamut
        // matrix below has linear light to work on.
        linear = srgb_eotf(c) * max(src_ref_nits, 0.0001);
    } else if (src_transfer > 1.5) {
        // Extended linear (scRGB): 1.0 is defined as 80 cd/m².
        linear = c * 80.0;
    } else {
        // PQ is absolute luminance.
        linear = pq_eotf(c);
    }
    if (src_primaries > 0.5) {
        linear = BT2020_TO_BT709 * linear;
    }
    return srgb_inv_eotf(clamp(tonemap_to_sdr(linear), 0.0, 1.0));
}

float rounded_alpha(vec2 coords, vec2 size) {
    if (coords.x < 0.0 || coords.y < 0.0 || coords.x > size.x || coords.y > size.y) {
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
    vec2 sample_coords = v_coords;
    if (sample_uv_compensation_enabled > 0.5) {
        vec2 original_range = max(sample_uv_br - sample_uv_tl, vec2(0.000001));
        vec2 range_coords = (v_coords - sample_uv_tl) / original_range;
        sample_coords = mix(sample_uv_tl, adjusted_sample_uv_br, range_coords);

        // Emulate nearest-neighbor with GL_CLAMP_TO_EDGE on the compensated
        // axes.  Snapping both axes turns the untouched one into nearest-
        // neighbor sampling too, which produces visible artifacts on fine
        // content.
        vec2 safe_buffer_size = max(sample_buffer_size, vec2(1.0));
        vec2 texel_size = vec2(1.0) / safe_buffer_size;
        // Clamp texel index to [0, N-1] so we never sample past the buffer
        // edge (GL_REPEAT would wrap and show the opposite side).
        vec2 texel_index = clamp(
            floor(sample_coords * safe_buffer_size),
            vec2(0.0),
            safe_buffer_size - vec2(1.0)
        );
        vec2 snapped_coords = (texel_index + 0.5) * texel_size;
        vec2 snap_axes = clamp(sample_uv_snap_axes, vec2(0.0), vec2(1.0));
        sample_coords = mix(sample_coords, snapped_coords, snap_axes);
        // Clamp to the first/last texel centers to emulate GL_CLAMP_TO_EDGE.
        vec2 min_coords = mix(vec2(0.0), texel_size * 0.5, snap_axes);
        vec2 max_coords = mix(
            vec2(1.0),
            sample_uv_br - texel_size * 0.5,
            snap_axes
        );
        sample_coords = clamp(sample_coords, min_coords, max_coords);
    }

    vec4 color = texture2D(tex, sample_coords);
    if (src_transfer > 0.5) {
        // Wayland buffers carry premultiplied alpha. A transfer function is
        // non-linear, so it has to be applied to the unpremultiplied value or
        // partially transparent edges pick up haloes; re-premultiply after.
        float a = max(color.a, 0.0001);
        color.rgb = to_compositing_space(color.rgb / a) * a;
    }
    vec2 local_coords = (input_to_local * vec3(v_coords, 1.0)).xy;
    if (rect_bounds_enabled > 0.5) {
        vec2 slot_coords = local_coords - slot_origin;
        if (slot_coords.x < 0.0 || slot_coords.y < 0.0 || slot_coords.x > slot_size.x || slot_coords.y > slot_size.y) {
            discard;
        }
    }
    vec2 mask_coords = local_coords - mask_origin;
    color *= rounded_alpha(mask_coords, mask_size);
    gl_FragColor = color * alpha;
}
