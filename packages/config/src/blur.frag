vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec4 color = texture2D(tex, uv);
    color.rgb = mix(color.rgb, vec3(1.0), 0.12);
    color.rgb *= 1.03;
    color.a = 1.0;
    return color;
}
