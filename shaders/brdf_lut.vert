#version 450

layout(location = 0) out vec2 vUV;

void main() {
    // Fullscreen triangle: 3 vertices cover the entire clip space
    vec2 pos = vec2(gl_VertexIndex & 2, (gl_VertexIndex << 1) & 2);
    vUV = pos * 0.5;
    gl_Position = vec4(pos - 1.0, 0.0, 1.0);
}
