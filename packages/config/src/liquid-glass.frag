// Full-panel liquid glass aligned to the effect rect.
// BORDER_RADIUS_PX now affects the actual visible contour because the lens mask
// fills the whole rect instead of creating a tiny central "badge".

const float INSET_PX = 0.0;
const float BORDER_RADIUS_PX = 0.0;
const float POWER_EXPONENT = 6.0;
const float LENS_STRENGTH = 0.18;
const float EDGE_DISTORTION_PX = 6.0;
const float EDGE_WIDTH_PX = 10.0;
const float SAMPLE_RANGE = 3.0;
const float SAMPLE_OFFSET = 0.45;
const float WHITE_TINT = 0.08;
const float HIGHLIGHT = 0.10;

float rounded_rect_alpha(vec2 coords, vec2 rect_size, vec4 radius) {
    if (coords.x < 0.0 || coords.y < 0.0 || coords.x > rect_size.x || coords.y > rect_size.y) {
        return 0.0;
    }

    vec2 center;
    float r;

    if (coords.x < radius.x && coords.y < radius.x) {
        r = radius.x;
        center = vec2(r, r);
    } else if (coords.x > rect_size.x - radius.y && coords.y < radius.y) {
        r = radius.y;
        center = vec2(rect_size.x - r, r);
    } else if (coords.x > rect_size.x - radius.z && coords.y > rect_size.y - radius.z) {
        r = radius.z;
        center = vec2(rect_size.x - r, rect_size.y - r);
    } else if (coords.x < radius.w && coords.y > rect_size.y - radius.w) {
        r = radius.w;
        center = vec2(r, rect_size.y - r);
    } else {
        return 1.0;
    }

    float dist = distance(coords, center);
    return 1.0 - smoothstep(r - 0.5, r + 0.5, dist);
}

float rounded_rect_sdf(vec2 coords, vec2 rect_size, float radius) {
    vec2 center = rect_size * 0.5;
    vec2 half_size = max(rect_size * 0.5 - vec2(radius), vec2(0.0));
    vec2 q = abs(coords - center) - half_size;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

vec2 rounded_rect_normal(vec2 coords, vec2 rect_size, float radius) {
    float eps = 1.0;
    float dx =
        rounded_rect_sdf(coords + vec2(eps, 0.0), rect_size, radius) -
        rounded_rect_sdf(coords - vec2(eps, 0.0), rect_size, radius);
    float dy =
        rounded_rect_sdf(coords + vec2(0.0, eps), rect_size, radius) -
        rounded_rect_sdf(coords - vec2(0.0, eps), rect_size, radius);
    vec2 normal = normalize(vec2(dx, dy));
    if (length(normal) < 0.001) {
        return vec2(0.0);
    }
    return normal;
}

vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec2 origin = vec2(INSET_PX);
    vec2 size = max(rect_size - vec2(INSET_PX * 2.0), vec2(1.0));
    vec2 coords = uv * rect_size - origin;
    float radius = max(BORDER_RADIUS_PX - INSET_PX, 0.0);

    float clip_mask = rounded_rect_alpha(coords, size, vec4(radius));
    vec4 base = texture2D(tex, uv);
    if (clip_mask <= 0.0) {
        return base;
    }

    vec2 local_uv = clamp(coords / size, vec2(0.0), vec2(1.0));
    vec2 centered = (local_uv - vec2(0.5)) * 2.0;
    float aspect = size.x / max(size.y, 1.0);

    float rounded_box =
        pow(abs(centered.x * aspect), POWER_EXPONENT) +
        pow(abs(centered.y), POWER_EXPONENT);

    float lens_mask = clamp(1.0 - rounded_box, 0.0, 1.0);
    float sdf = rounded_rect_sdf(coords, size, radius);
    float edge_boost = 1.0 - smoothstep(0.0, EDGE_WIDTH_PX, -sdf);
    edge_boost *= clip_mask;

    float transition = smoothstep(0.0, 1.0, lens_mask) * clip_mask;
    vec2 lens = ((local_uv - 0.5) * (1.0 - lens_mask * LENS_STRENGTH) + 0.5);
    vec2 edge_normal = rounded_rect_normal(coords, size, radius);
    lens += (edge_normal * edge_boost * EDGE_DISTORTION_PX) / max(size, vec2(1.0));
    vec2 lens_uv = clamp((lens * size + origin) / rect_size, vec2(0.0), vec2(1.0));

    vec4 accum = vec4(0.0);
    float total = 0.0;
    for (float x = -SAMPLE_RANGE; x <= SAMPLE_RANGE; x += 1.0) {
        for (float y = -SAMPLE_RANGE; y <= SAMPLE_RANGE; y += 1.0) {
            vec2 offset = vec2(x, y) * SAMPLE_OFFSET / rect_size;
            accum += texture2D(tex, clamp(lens_uv + offset, vec2(0.0), vec2(1.0)));
            total += 1.0;
        }
    }
    vec4 blurred = accum / total;

    float edge = max(smoothstep(0.70, 1.0, 1.0 - lens_mask), edge_boost);
    vec4 lit = blurred;
    lit.rgb = mix(lit.rgb, vec3(1.0), WHITE_TINT + edge * HIGHLIGHT);

    return mix(base, lit, transition);
}
