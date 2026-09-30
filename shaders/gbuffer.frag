#version 450

// Geometry pass of the deferred renderer.
//
// Writes the surface parameters into the G-buffer instead of shading:
//   location 0: albedo.rgb                + occlusion  (RGBA16F)
//   location 1: world-space normal.xyz    + roughness  (RGBA16F)
//   location 2: emissive.rgb              + metallic   (RGBA16F)
//
// The vertex stage is `pbr.vert` (unchanged): it supplies the world
// position, world-space normal, tangent and UV. The material is fully
// resolved here, so the lighting pass never touches a material again.
//
// Alpha is *not* stored — the composite pass writes an opaque swapchain
// image, and `albedo.a` carries the ambient occlusion factor instead.

layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    mat4 invViewProj;        // used by the deferred lighting pass
    vec4 cameraPos;          // .xyz used, .w reserved (channel-reuse policy)
    vec4 lightDir;           // .xyz used (negated before NdotL), .w reserved
    vec4 lightingPack;       // .x = lightIntensity, .y = prefilterMaxLod, .z..w reserved
    vec4 deferredPack;       // .x = floatBitsToUint(debugView), .yzw reserved
} globals;

struct Material {
    vec4 baseColorFactor;
    vec4 emissiveFactor;     // .rgb used, .w is the std140 alignment pad (NEVER a bit-pack target)
    vec4 factorPack;         // .x = metallicFactor, .y = roughnessFactor, .z = normalScale, .w = occlusionStrength
};

layout(std140, set = 0, binding = 1) uniform MaterialBuffer {
    Material materials[64];
} materialBuffer;

layout(set = 1, binding = 0) uniform sampler2D uBaseColor;
layout(set = 1, binding = 1) uniform sampler2D uMetallicRoughness;
layout(set = 1, binding = 2) uniform sampler2D uNormal;
layout(set = 1, binding = 3) uniform sampler2D uOcclusion;
layout(set = 1, binding = 4) uniform sampler2D uEmissive;

layout(push_constant) uniform PushConstants {
    mat4 model;
    vec4 tail;   // .x = floatBitsToUint(materialIndex), .yzw reserved (channel-reuse policy)
} pc;

layout(location = 0) in vec3 vWorldPos;
layout(location = 1) in vec3 vNormal;
layout(location = 2) in vec4 vTangent;
layout(location = 3) in vec2 vUV;

layout(location = 0) out vec4 gAlbedo;
layout(location = 1) out vec4 gNormal;
layout(location = 2) out vec4 gEmissive;

void main() {
    Material mat = materialBuffer.materials[floatBitsToUint(pc.tail.x)];

    vec4 baseColorSample = texture(uBaseColor, vUV);
    vec3 baseColor = baseColorSample.rgb * mat.baseColorFactor.rgb;

    vec4 mrSample = texture(uMetallicRoughness, vUV);
    float metallic = clamp(mrSample.b * mat.factorPack.x, 0.0, 1.0);
    float roughness = clamp(mrSample.g * mat.factorPack.y, 0.045, 1.0);

    vec3 normalSample = texture(uNormal, vUV).rgb;
    normalSample = normalSample * 2.0 - 1.0;
    normalSample = normalize(vec3(normalSample.xy * mat.factorPack.z, normalSample.z));

    vec3 N = normalize(vNormal);
    vec3 T = normalize(vTangent.xyz);
    T = normalize(T - N * dot(N, T));
    vec3 B = normalize(cross(N, T)) * vTangent.w;
    mat3 TBN = mat3(T, B, N);
    N = normalize(TBN * normalSample);

    float aoSample = texture(uOcclusion, vUV).r;
    float occlusion = mix(1.0, aoSample, clamp(mat.factorPack.w, 0.0, 1.0));
    vec3 emissive = texture(uEmissive, vUV).rgb * mat.emissiveFactor.rgb;

    gAlbedo = vec4(baseColor, occlusion);
    gNormal = vec4(N, roughness);
    gEmissive = vec4(emissive, metallic);
}
