// Smithay substitutes the variant defines here via a plain
// `src.replace("//_DEFINES_", ...)` (gles/shaders/mod.rs). Without this marker
// the replace is a no-op and all three compiled variants get identical source
// with EXTERNAL and NO_ALPHA *undefined* — so an external-OES buffer would be
// sampled through a `sampler2D` declaration, and NO_ALPHA (which smithay
// documents as mandatory for custom texture shaders) would never be honoured.
//_DEFINES_

// `#extension` must precede every non-preprocessor token in GLSL ES 1.00, so
// it has to come before `precision` and any declaration.
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
varying vec2 v_coords;
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
// 0 = passthrough (untagged, or already sRGB in sRGB primaries), 1 = ST 2084 PQ,
// 2 = extended linear (scRGB), 3 = sRGB transfer needing only a gamut change.
uniform float src_transfer;
// 0 = BT.709/sRGB primaries, 1 = BT.2020.
uniform float src_primaries;
// Content reference white in cd/m², from the surface's `Luminances` (or the
// protocol's per-transfer-function defaults when unset). Doubles as the
// compositing space's white point: 1.0 here means this many nits.
uniform float src_ref_nits;
// Tone-mapping knee parameters, already in the PQ domain because that is where
// BT.2390 operates. Precomputed on the CPU via `color::colorimetry::
// pq_inverse_eotf` — deriving them here would cost four extra pow-heavy calls
// per fragment for values that are constant across the whole surface.
uniform float src_pq_lo;   // content black
uniform float src_pq_hi;   // content peak
uniform float dst_pq_hi;   // what compositing-space 1.0 can represent

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

// SMPTE ST 2084 (PQ) inverse EOTF: absolute cd/m² -> PQ signal. Mirrors the
// one in `output_encode.frag`; needed here to lift non-PQ sources into the
// domain BT.2390 works in.
vec3 pq_inv_eotf(vec3 nits) {
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    vec3 y = clamp(nits / 10000.0, 0.0, 1.0);
    vec3 ym = pow(y, vec3(m1));
    return pow((vec3(c1) + c2 * ym) / (vec3(1.0) + c3 * ym), vec3(m2));
}

// ITU-R BT.2390 EETF, applied per channel on a PQ signal.
//
// Below the knee this is the *identity*, so content already inside the target
// range passes through untouched — the decisive advantage over a global curve
// like Reinhard, which pulls midtones down alongside highlights and leaves the
// whole image flat. Above the knee a Hermite spline rolls smoothly off to the
// target peak. Operating in PQ rather than linear light spreads the
// compression error evenly across *perceived* brightness.
float bt2390_channel(float e) {
    float range = max(src_pq_hi - src_pq_lo, 0.000001);
    float e1 = clamp((e - src_pq_lo) / range, 0.0, 1.0);
    float max_lum = clamp((dst_pq_hi - src_pq_lo) / range, 0.0, 1.0);
    float ks = 1.5 * max_lum - 0.5;

    float e2 = e1;
    if (ks < 1.0 && e1 > ks) {
        float t = (e1 - ks) / (1.0 - ks);
        float t2 = t * t;
        float t3 = t2 * t;
        e2 = (2.0 * t3 - 3.0 * t2 + 1.0) * ks
           + (t3 - 2.0 * t2 + t) * (1.0 - ks)
           + (-2.0 * t3 + 3.0 * t2) * max_lum;
    }
    return e2 * range + src_pq_lo;
}

vec3 bt2390_eetf(vec3 pq) {
    return vec3(
        bt2390_channel(pq.r),
        bt2390_channel(pq.g),
        bt2390_channel(pq.b)
    );
}

// sRGB EOTF (IEC 61966-2-1 piecewise decode).
vec3 srgb_eotf(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((max(c, vec3(0.0)) + vec3(0.055)) / 1.055, vec3(2.4));
    return mix(hi, lo, vec3(lessThanEqual(c, vec3(0.04045))));
}

// Convert one sampled, *unpremultiplied* texel into the compositing space.
vec3 to_compositing_space(vec3 c) {
    float ref_nits = max(src_ref_nits, 0.0001);

    // Lift the source into the PQ domain, which is where the tone curve works.
    vec3 pq;
    if (src_transfer > 2.5) {
        // sRGB transfer in wider primaries: decode, scale to absolute nits.
        pq = pq_inv_eotf(srgb_eotf(c) * ref_nits);
    } else if (src_transfer > 1.5) {
        // Extended linear (scRGB): 1.0 is defined as 80 cd/m².
        pq = pq_inv_eotf(max(c, vec3(0.0)) * 80.0);
    } else {
        // Already a PQ signal.
        pq = clamp(c, 0.0, 1.0);
    }

    // Compress the content's range into what the compositing space can hold.
    // SDR sources land entirely below the knee and pass through unchanged.
    vec3 linear = pq_eotf(bt2390_eetf(pq));

    if (src_primaries > 0.5) {
        linear = BT2020_TO_BT709 * linear;
    }
    // Normalize against the content's own reference white so diffuse white
    // lands on 1.0 rather than being scaled by an unrelated display value.
    return srgb_inv_eotf(clamp(linear / ref_nits, 0.0, 1.0));
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
