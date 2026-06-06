#version 450

layout(set = 0, binding = 5) uniform samplerCube uEnvironmentCubemap;

layout(location = 0) in vec3 vDirection;
layout(location = 0) out vec4 outColor;

vec3 acesToneMapping(vec3 color) {
    float a = 2.51;
    float b = 0.03;
    float c = 2.43;
    float d = 0.59;
    float e = 0.14;
    return clamp((color * (a * color + b)) / (color * (c * color + d) + e), 0.0, 1.0);
}

void main() {
    vec3 color = textureLod(uEnvironmentCubemap, vDirection, 0.0).rgb;
    color = acesToneMapping(color);
    outColor = vec4(color, 1.0);
}
