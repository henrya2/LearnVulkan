#version 450

// Fullscreen triangle. No vertex buffer required.
// Generates three NDC positions that cover the screen: (-1,-1), (3,-1), (-1,3).
// The rasterizer clips to the viewport.

layout(location = 0) out vec2 vUV;

void main() {
    vec2 pos = vec2((gl_VertexIndex & 1) * 4.0 - 1.0,
                    (gl_VertexIndex & 2) * 2.0 - 1.0);
    gl_Position = vec4(pos, 0.0, 1.0);
    // vUV in [0, 1] across the visible portion of the triangle.
    vUV = pos * 0.5 + 0.5;
}
