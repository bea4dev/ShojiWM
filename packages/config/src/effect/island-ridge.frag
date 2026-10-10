uniform highp sampler2D field_input;
// The same distance, smoothed over a good part of the rim: its gradient turns
// smoothly around corners sharper than the rim instead of creasing along
// their bisector.
uniform highp sampler2D direction_input;
// Farther ridges than this do not matter: the lens is at most this wide.
uniform float rim_width_px;

// How far in the shape is thick: the signed distance of the ridge (medial
// axis) reached by walking from this pixel up the distance gradient. Where
// that is closer than the rim, the lens must flatten out by the ridge, or the
// two sides' opposite normals meet there in a crease.
const int STEPS = 24;

float fieldAt(vec2 p, vec2 size) {
    return texture2D(field_input, clamp(p, vec2(0.5), size - 0.5) / size).r;
}

float directionAt(vec2 p, vec2 size) {
    return texture2D(direction_input, clamp(p, vec2(0.5), size - 0.5) / size).r;
}

vec4 shader_main(EffectContext effect) {
    vec2 size = effect.texture_size_phy_px;
    vec2 p = effect.texture_uv * size;
    float here = fieldAt(p, size);
    float smoothed = directionAt(p, size);
    // Walk the way the lens bends (island-refract.frag), so a corner's
    // bisector reaches into the shape rather than straight across it.
    vec2 gradient = vec2(directionAt(p + vec2(1.5, 0.0), size) - directionAt(p - vec2(1.5, 0.0), size),
                         directionAt(p + vec2(0.0, 1.5), size) - directionAt(p - vec2(0.0, 1.5), size));
    float magnitude = length(gradient);
    if (magnitude < 0.0001)
        return vec4(here, max(here, 0.0), smoothed, 1.0);
    vec2 dir = gradient / magnitude;
    float stepPx = max(rim_width_px, 1.0) / float(STEPS);

    // Walk until the distance falls again: a later rise belongs to another
    // shape across a neck.
    float previous = here;
    float best = here;
    float before = here;
    float after = here;
    bool fell = false;
    for (int i = 1; i <= STEPS; i++) {
        float value = fieldAt(p + dir * (stepPx * float(i)), size);
        if (value > best) {
            before = previous;
            best = value;
            after = value;
        } else if (after == best) {
            after = value;
        }
        if (value < best - 0.5 * stepPx) {
            fell = true;
            break;
        }
        previous = value;
    }
    // Ridges are tents: the peak between three samples sits half their
    // slope difference above the highest one.
    float ridge = best + 0.5 * abs((best - before) - (best - after));
    if (after == best)
        ridge = best;
    // Past a corner's bisector the distance stops rising but does not fall:
    // it is the distance to the adjacent side, still sloping across the walk.
    // That is no far side, so the lens keeps its full width there instead of
    // narrowing beside every corner. Only a level crest across the walk, like
    // the axis at a pill's end, is a ridge without a fall.
    if (!fell) {
        vec2 end = p + dir * (stepPx * float(STEPS));
        vec2 slope = vec2(directionAt(end + vec2(1.5, 0.0), size) - directionAt(end - vec2(1.5, 0.0), size),
                          directionAt(end + vec2(0.0, 1.5), size) - directionAt(end - vec2(0.0, 1.5), size)) / 3.0;
        ridge = mix(ridge, max(ridge, rim_width_px), smoothstep(0.25, 0.75, length(slope)));
    }
    // R: signed distance (unchanged); G: ridge distance; B: the smoothed
    // distance the bend direction is taken from.
    return vec4(here, max(ridge, 0.0), smoothed, 1.0);
}
