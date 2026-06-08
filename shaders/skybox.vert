#version 450

layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    vec3 cameraPos;
    float _pad0;
    vec3 lightDir;
    float lightIntensity;
    float prefilterMaxLod;
} globals;

layout(location = 0) in vec3 inPos;
layout(location = 0) out vec3 vDirection;

void main() {
    // Strip translation from view matrix so the skybox follows the camera
    mat4 rotView = mat4(mat3(globals.view));
    vec4 clipPos = globals.proj * rotView * vec4(inPos, 1.0);
    // Force depth to far plane (1.0 after perspective divide)
    gl_Position = clipPos.xyww;
    vDirection = inPos;
}
