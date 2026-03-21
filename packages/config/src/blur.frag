vec4 shader_main(vec2 uv, vec2 rect_size) {
    vec4 color = texture2D(tex, uv);
    color.rgb = mix(color.rgb, vec3(dot(color.rgb, vec3(0.2126, 0.7152, 0.0722))), 0.05);
    color.rgb += vec3(0.04);
    color.a = 0.9;
    return color;
}
