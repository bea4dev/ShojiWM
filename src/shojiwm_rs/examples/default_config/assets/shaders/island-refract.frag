uniform sampler2D sharp_scene;
uniform sampler2D soft_scene;
uniform sampler2D edge_scene;
uniform sampler2D silhouette;
uniform highp sampler2D distance_field;
uniform float rim_width_px;
uniform float refraction_px;
uniform float chromatic_shift_px;
uniform float highlight_strength;
uniform float highlight_width_px;
uniform float debug_view;
uniform float blur_mix;
uniform float edge_softness_px;
uniform float bevel_width_px;
uniform float bevel_shadow;

// Circular lens profile from Aghajari's Liquid Glass article:
// https://www.aghajari.com/publications/liquid-glass/
// t is distance FROM the boundary; x = 1-t in the article.
float circularLens(float distancePx, float widthPx) {
    float x = 1.0 - clamp(distancePx / widthPx, 0.0, 1.0);
    // A small finite lens thickness rounds off the singular derivative.
    // Preserve both endpoints and the circular character of the profile.
    float epsilon = clamp(edge_softness_px / widthPx, 0.0001, 0.5);
    float top = sqrt(1.0 + epsilon);
    return (top - sqrt(max(1.0 - x * x, 0.0) + epsilon))
        / (top - sqrt(epsilon));
}

float filteredCircularLens(float distancePx, float widthPx) {
    // Average over one framebuffer pixel: the ideal circle has an infinite
    // slope at the rim. Subpixel filtering softens only that last pixel,
    // reducing shimmer without replacing the circular profile by smoothstep.
    return 0.25 * (circularLens(distancePx - 0.375, widthPx)
                 + circularLens(distancePx - 0.125, widthPx)
                 + circularLens(distancePx + 0.125, widthPx)
                 + circularLens(distancePx + 0.375, widthPx));
}

vec3 backgroundAt(vec2 uv, vec2 size, float edgeBlur) {
    uv = clamp(uv, 0.5 / size, 1.0 - 0.5 / size);
    vec3 soft = mix(texture2D(soft_scene, uv).rgb,
                    texture2D(edge_scene, uv).rgb, edgeBlur);
    return mix(texture2D(sharp_scene, uv).rgb, soft, clamp(blur_mix, 0.0, 1.0));
}

vec4 shader_main(EffectContext effect) {
    vec2 uv = effect.texture_uv;
    vec2 size = effect.texture_size_phy_px;
    vec2 sourceMask = texture2D(silhouette, uv).rg;
    float mask = sourceMask.r;
    if (mask <= 0.001)
        return vec4(0.0);

    vec2 dx = vec2(3.0 / size.x, 0.0);
    vec2 dy = vec2(0.0, 3.0 / size.y);
    // Sobel estimate: merge three parallel differences instead of trusting
    // a single pair of samples on a changing rasterized corner.
    // B holds a smoothed copy of the distance, so the bend turns smoothly
    // around corners sharper than the rim rather than creasing.
    float tl = texture2D(distance_field, uv - dx - dy).b;
    float tr = texture2D(distance_field, uv + dx - dy).b;
    float bl = texture2D(distance_field, uv - dx + dy).b;
    float br = texture2D(distance_field, uv + dx + dy).b;
    vec2 gradient = vec2(tr + 2.0 * texture2D(distance_field, uv + dx).b + br
                         - tl - 2.0 * texture2D(distance_field, uv - dx).b - bl,
                         bl + 2.0 * texture2D(distance_field, uv + dy).b + br
                         - tl - 2.0 * texture2D(distance_field, uv - dy).b - tr) / 24.0;
    float magnitude = length(gradient);
    vec2 inward = gradient / max(magnitude, 0.0001);
    // R: distance from the boundary; G: distance of the ridge (the middle of
    // the shape) seen from here. See island-ridge.frag.
    vec2 field = texture2D(distance_field, uv).rg;
    float distance = max(field.r, 0.0);
    float rimWidth = max(rim_width_px, 1.0);
    // A shape thinner than the rim gets a narrower lens of the same form,
    // flat at its middle: a full-width lens would still be sloped where the
    // two sides meet, and their opposite normals would crease there.
    float lensWidth = clamp(field.g, 1.0, rimWidth);
    float lensScale = lensWidth / rimWidth;
    // Gentle in the interior, steeply curved close to the boundary.
    float profile = filteredCircularLens(distance, lensWidth);
    // Opposing normals cancel at a neck: attenuate rather than flip direction.
    float coherence = smoothstep(0.15, 0.85, magnitude);
    vec2 bend = inward * profile * coherence;
    // Bound excursion to the rim width, and scale it with a narrowed lens so
    // a thin shape bends like a thin lens rather than sampling past itself.
    float strength = min(max(refraction_px, 0.0), rimWidth) * lensScale;
    vec2 offset = bend * strength / size;
    // Separate RGB around the refracted coordinate along the same normal.
    // Keep chromatic separation in the outer half of the rim, with a circular
    // falloff of its own instead of spreading constant fringes across the UI.
    float chromaticProfile = filteredCircularLens(distance, max(lensWidth * 0.5, 1.0));
    vec2 dispersion = inward * coherence * chromaticProfile * chromatic_shift_px * lensScale / size;
    float edgeBlur = 0.8 * profile;
    vec3 color = vec3(backgroundAt(uv + offset + dispersion, size, edgeBlur).r,
                      backgroundAt(uv + offset, size, edgeBlur).g,
                      backgroundAt(uv + offset - dispersion, size, edgeBlur).b);

    // Light from the upper left catches the rim facing it, and, weaker, the
    // opposite one where it leaves the glass; the sides facing away stay
    // nearly dark.
    float light = dot(-inward, normalize(vec2(-0.45, -0.89)));
    float glint = 0.12 + 0.88 * pow(max(light, 0.0), 1.5) + 0.5 * pow(max(-light, 0.0), 1.5);
    // A soft band rather than a hairline: several pixels wide with no hard
    // inner edge, so it stays smooth on low-density screens. A broader sheen
    // follows the curvature of the lens.
    float band = exp(-distance / max(highlight_width_px, 0.5));
    float shine = clamp(highlight_strength * glint * coherence * (1.2 * band + 0.3 * profile), 0.0, 1.0);
    float bevel = min(max(bevel_width_px, 1.0), lensWidth);
    float innerBand = exp(-pow((distance - bevel * 0.5) / (bevel * 0.45), 2.0)) * coherence;
    color *= 1.0 - clamp(bevel_shadow, 0.0, 0.6) * innerBand * (0.5 + 0.5 * max(-light, 0.0));
    // Screen, not add: brightens dark glass the most and never clips to a
    // flat white line over bright content.
    color = 1.0 - (1.0 - color) * (1.0 - shine * vec3(0.94, 0.97, 1.0));

    if (debug_view > 2.5) color = vec3(0.5 + bend * 0.5, 0.5);
    else if (debug_view > 1.5) color = vec3(clamp(distance / rimWidth, 0.0, 1.0));
    else if (debug_view > 0.5) color = vec3(1.0);
    // QuickShell draws its antialiased tint over this behind effect. Applying
    // coverage twice makes corners thicker/darker. Solve source-over so the
    // combined coverage stays mask: a + (1-a)*behindAlpha = mask.
    float behindAlpha = clamp((mask - sourceMask.g) / max(1.0 - sourceMask.g, 0.0001), 0.0, 1.0);
    return vec4(color * behindAlpha, behindAlpha);
}
