#version 450

layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    vec4 cameraPos;
    vec4 lightDir;
    float lightIntensity;
    float prefilterMaxLod;
} globals;

layout(push_constant) uniform PushConstants {
    mat4 model;
    uint materialIndex;
} pc;

layout(location = 0) in vec3 inPos;
layout(location = 1) in vec3 inNormal;
layout(location = 2) in vec4 inTangent;
layout(location = 3) in vec2 inUV;

layout(location = 0) out vec3 vWorldPos;
layout(location = 1) out vec3 vNormal;
layout(location = 2) out vec4 vTangent;
layout(location = 3) out vec2 vUV;

void main() {
    vec4 worldPos = pc.model * vec4(inPos, 1.0);
    vWorldPos = worldPos.xyz;
    gl_Position = globals.proj * globals.view * worldPos;

    mat3 normalMatrix = transpose(inverse(mat3(pc.model)));
    vNormal = normalize(normalMatrix * inNormal);
    vTangent = vec4(normalize(normalMatrix * inTangent.xyz), inTangent.w);
    vUV = inUV;
}
