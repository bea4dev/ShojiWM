uniform float strength;

// Fades a backdrop effect in and out: 0 is no effect at all (transparent),
// 1 the effect as it is. The alpha is forced to 1 first, as the default
// "opaque" effect output does.
vec4 shader_main(EffectContext effect) {
    vec4 color = texture2D(tex, effect.texture_uv);
    color.a = 1.0;
    return color * strength;
}
