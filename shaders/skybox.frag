#version 450

layout(set = 0, binding = 5) uniform samplerCube uEnvironmentCubemap;

layout(location = 0) in vec3 vDirection;
layout(location = 0) out vec4 outColor;

void main() {
    // Output linear HDR. The composite postprocess pass applies exposure +
    // tonemapping. Do not tonemap or gamma-correct here.
    vec3 color = textureLod(uEnvironmentCubemap, vDirection, 0.0).rgb;
    outColor = vec4(color, 1.0);
}
