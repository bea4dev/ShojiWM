#version 100

attribute vec2 vert;
varying vec2 v_coords;

void main() {
    v_coords = vert;
    vec2 position = vert * 2.0 - 1.0;
    gl_Position = vec4(position, 1.0, 1.0);
}
