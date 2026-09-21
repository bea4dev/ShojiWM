uniform float jump_px;
uniform highp sampler2D field_input;

vec4 shader_main(EffectContext effect) {
    vec2 size = effect.texture_size_px;
    // Nearest-seed candidates must be fetched at texel centers.
    vec2 p = floor(effect.texture_uv * size) + 0.5;
    vec4 best = vec4(0.0);
    float bestDistance = 1.0e20;
    for (int y = -1; y <= 1; y++) {
        for (int x = -1; x <= 1; x++) {
            vec2 q = p + vec2(float(x), float(y)) * jump_px;
            if (any(lessThan(q, vec2(0.5))) || any(greaterThan(q, size - 0.5)))
                continue;
            vec4 candidate = texture2D(field_input, q / size);
            if (candidate.b < 0.5)
                continue;
            vec2 delta = candidate.rg + (q - p);
            float distanceSquared = dot(delta, delta);
            if (distanceSquared < bestDistance) {
                bestDistance = distanceSquared;
                best = vec4(delta, 1.0, 1.0);
            }
        }
    }
    return best;
}
