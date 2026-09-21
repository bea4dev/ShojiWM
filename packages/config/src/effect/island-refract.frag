uniform sampler2D sharp_scene;
uniform sampler2D soft_scene;
uniform sampler2D edge_scene;
uniform sampler2D silhouette;
uniform highp sampler2D distance_field;
uniform float rim_width_px;
uniform float refraction_px;
uniform float chromatic_shift_px;
uniform float highlight_strength;
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
    vec2 size = effect.texture_size_px;
    vec2 sourceMask = texture2D(silhouette, uv).rg;
    float mask = sourceMask.r;
    if (mask <= 0.001)
        return vec4(0.0);

    vec2 dx = vec2(3.0 / size.x, 0.0);
    vec2 dy = vec2(0.0, 3.0 / size.y);
    // Sobel estimate: merge three parallel differences instead of trusting
    // a single pair of samples on a changing rasterized corner.
    float tl = texture2D(distance_field, uv - dx - dy).r;
    float tr = texture2D(distance_field, uv + dx - dy).r;
    float bl = texture2D(distance_field, uv - dx + dy).r;
    float br = texture2D(distance_field, uv + dx + dy).r;
    vec2 gradient = vec2(tr + 2.0 * texture2D(distance_field, uv + dx).r + br
                         - tl - 2.0 * texture2D(distance_field, uv - dx).r - bl,
                         bl + 2.0 * texture2D(distance_field, uv + dy).r + br
                         - tl - 2.0 * texture2D(distance_field, uv - dy).r - tr) / 24.0;
    float magnitude = length(gradient);
    vec2 inward = gradient / max(magnitude, 0.0001);
    float distance = max(texture2D(distance_field, uv).r, 0.0);
    float rimWidth = max(rim_width_px, 1.0);
    // Gentle in the interior, steeply curved close to the boundary.
    float profile = filteredCircularLens(distance, rimWidth);
    // Opposing normals cancel at a neck: attenuate rather than flip direction.
    float coherence = smoothstep(0.15, 0.85, magnitude);
    vec2 bend = inward * profile * coherence;
    // The previous 0.6*rim cap was specific to the smoothstep slope and is
    // not a no-foldover guarantee for a circular lens. Bound excursion to
    // the rim width while allowing the tutorial's pronounced edge lensing.
    float strength = min(max(refraction_px, 0.0), rimWidth);
    vec2 offset = bend * strength / size;
    // Separate RGB around the refracted coordinate along the same normal.
    // Keep chromatic separation in the outer half of the rim, with a circular
    // falloff of its own instead of spreading constant fringes across the UI.
    float chromaticProfile = filteredCircularLens(distance, max(rimWidth * 0.5, 1.0));
    vec2 dispersion = inward * coherence * chromaticProfile * chromatic_shift_px / size;
    float edgeBlur = 0.8 * profile;
    vec3 color = vec3(backgroundAt(uv + offset + dispersion, size, edgeBlur).r,
                      backgroundAt(uv + offset, size, edgeBlur).g,
                      backgroundAt(uv + offset - dispersion, size, edgeBlur).b);

    float light = dot(-inward, normalize(vec2(-0.45, -0.89)));
    float bevel = max(bevel_width_px, 1.0);
    float rim = exp(-distance / 1.6) * coherence;
    float innerBand = exp(-pow((distance - bevel * 0.5) / (bevel * 0.45), 2.0)) * coherence;
    // Thin reflection plus a broader inset shadow communicate a rounded lip.
    // Use mostly neutral light, avoiding a colored outline on bright backdrops.
    color *= 1.0 - clamp(bevel_shadow, 0.0, 0.6) * innerBand * (0.5 + 0.5 * max(-light, 0.0));
    color += vec3(0.94, 0.97, 1.0) * highlight_strength
        * (rim * (0.3 + 0.7 * max(light, 0.0)) + innerBand * 0.22 * max(light, 0.0));

    if (debug_view > 2.5) color = vec3(0.5 + bend * 0.5, 0.5);
    else if (debug_view > 1.5) color = vec3(clamp(distance / rimWidth, 0.0, 1.0));
    else if (debug_view > 0.5) color = vec3(1.0);
    // QuickShell draws its antialiased tint over this behind effect. Applying
    // coverage twice makes corners thicker/darker. Solve source-over so the
    // combined coverage stays mask: a + (1-a)*behindAlpha = mask.
    float behindAlpha = clamp((mask - sourceMask.g) / max(1.0 - sourceMask.g, 0.0001), 0.0, 1.0);
    return vec4(color * behindAlpha, behindAlpha);
}
