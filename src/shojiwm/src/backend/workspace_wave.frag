//_DEFINES
#ifdef EXTERNAL
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#ifdef EXTERNAL
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
varying vec2 v_coords;
uniform float alpha;
uniform float progress;
uniform float direction;
uniform vec2 screen_size;
uniform vec3 theme_accent;

void main() {
    vec4 old = texture2D(tex, v_coords);
    if (progress <= 0.0) { gl_FragColor = old * alpha; return; }
    if (progress >= 1.0) { gl_FragColor = vec4(0.0); return; }
    vec2 origin = vec2(screen_size.x * 0.5, direction > 0.0 ? screen_size.y : 0.0);
    vec2 delta = v_coords * screen_size - origin;
    float distance = length(delta);
    float angle = distance > 0.001 ? atan(delta.y, delta.x) : 0.0;
    float reach = length(vec2(screen_size.x * 0.5, screen_size.y)) + 100.0;
    float radius = reach * (1.0 - pow(1.0 - progress, 1.4));
    float life = sin(progress * 3.14159265);
    float crest = sin(angle * 3.0 + progress * 7.0) * 0.55
        + sin(angle * 7.0 - progress * 11.0) * 0.30
        + cos(angle * 11.0 + progress * 9.0) * 0.15;
    float depth = max(0.0, radius + life * min(42.0, reach * 0.045) * crest) - distance;
    float reveal = smoothstep(-0.8, 0.8, depth);
    float width = clamp(min(screen_size.x, screen_size.y) * 0.022, 14.0, 28.0);
    float band = (1.0 - smoothstep(0.0, width, depth)) * reveal * life * 0.88;
    float highlight = exp(-pow((depth - 3.0) / 1.8, 2.0)) * reveal * life * 0.45;
    float wake = exp(-pow((depth - 14.0 - progress * 12.0) / 4.0, 2.0)) * reveal * life * 0.16;
    vec4 color = old * (1.0 - reveal);
    color = mix(color, vec4(theme_accent, 1.0), band);
    color = mix(color, vec4(mix(theme_accent, vec3(1.0), 0.35), 1.0), highlight);
    gl_FragColor = mix(color, vec4(theme_accent, 1.0), wake) * alpha;
}
