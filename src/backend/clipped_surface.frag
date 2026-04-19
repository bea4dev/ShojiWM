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
