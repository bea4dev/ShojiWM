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
uniform vec2 clip_size;
uniform vec4 corner_radius;
uniform float rect_bounds_enabled;
uniform mat3 input_to_clip;
uniform vec2 sample_uv_tl;
uniform vec2 sample_uv_br;
uniform vec2 adjusted_sample_uv_br;
uniform vec2 sample_buffer_size;
uniform float sample_uv_compensation_enabled;

float rounded_alpha(vec2 coords, vec2 size) {
    if (rect_bounds_enabled > 0.5 && (coords.x < 0.0 || coords.y < 0.0 || coords.x > size.x || coords.y > size.y)) {
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
        sample_coords = clamp(sample_coords, vec2(0.0), vec2(1.0));

        // Emulate nearest-neighbor edge repeat when we compensate a one-pixel
        // projection mismatch. This keeps the client sharp instead of blending
        // the last texel into the corrected edge.
        vec2 safe_buffer_size = max(sample_buffer_size, vec2(1.0));
        vec2 texel_size = vec2(1.0) / safe_buffer_size;
        sample_coords = (floor(sample_coords * safe_buffer_size) + 0.5) * texel_size;
        sample_coords = clamp(sample_coords, texel_size * 0.5, vec2(1.0) - texel_size * 0.5);
    }

    vec4 color = texture2D(tex, sample_coords);
    vec2 normalized = (input_to_clip * vec3(v_coords, 1.0)).xy;
    vec2 coords = normalized * clip_size;
    color *= rounded_alpha(coords, clip_size);
    gl_FragColor = color * alpha;
}
