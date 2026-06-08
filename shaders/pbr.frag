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

struct Material {
    vec4 baseColorFactor;
    vec4 emissiveFactor;
    float metallicFactor;
    float roughnessFactor;
    float normalScale;
    float occlusionStrength;
    vec4 _pad;
};

layout(std140, set = 0, binding = 1) uniform MaterialBuffer {
    Material materials[64];
} materialBuffer;

layout(set = 1, binding = 0) uniform sampler2D uBaseColor;
layout(set = 1, binding = 1) uniform sampler2D uMetallicRoughness;
layout(set = 1, binding = 2) uniform sampler2D uNormal;
layout(set = 1, binding = 3) uniform sampler2D uOcclusion;
layout(set = 1, binding = 4) uniform sampler2D uEmissive;

layout(set = 0, binding = 2) uniform samplerCube uIrradianceMap;
layout(set = 0, binding = 3) uniform samplerCube uPrefilterMap;
layout(set = 0, binding = 4) uniform sampler2D uBRDFLUT;

layout(push_constant) uniform PushConstants {
    mat4 model;
    uint materialIndex;
    uint _pad1[3];
} pc;

layout(location = 0) in vec3 vWorldPos;
layout(location = 1) in vec3 vNormal;
layout(location = 2) in vec4 vTangent;
layout(location = 3) in vec2 vUV;

layout(location = 0) out vec4 outColor;

const float PI = 3.14159265359;

float distributionGGX(vec3 N, vec3 H, float roughness) {
    float a = roughness * roughness;
    float a2 = a * a;
    float NdotH = max(dot(N, H), 0.0);
    float NdotH2 = NdotH * NdotH;
    float denom = (NdotH2 * (a2 - 1.0) + 1.0);
    denom = PI * denom * denom;
    return a2 / denom;
}

float geometrySchlickGGX(float NdotV, float roughness) {
    float r = (roughness + 1.0);
    float k = (r * r) / 8.0;
    float denom = NdotV * (1.0 - k) + k;
    return NdotV / denom;
}

float geometrySmith(vec3 N, vec3 V, vec3 L, float roughness) {
    float NdotV = max(dot(N, V), 0.0);
    float NdotL = max(dot(N, L), 0.0);
    float ggx2 = geometrySchlickGGX(NdotV, roughness);
    float ggx1 = geometrySchlickGGX(NdotL, roughness);
    return ggx1 * ggx2;
}

vec3 fresnelSchlick(float cosTheta, vec3 F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

vec3 fresnelSchlickRoughness(float cosTheta, vec3 F0, float roughness) {
    return F0 + (max(vec3(1.0 - roughness), F0) - F0)
            * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

void main() {
    Material mat = materialBuffer.materials[pc.materialIndex];

    vec4 baseColorSample = texture(uBaseColor, vUV);
    vec3 baseColor = baseColorSample.rgb * mat.baseColorFactor.rgb;
    float alpha = baseColorSample.a * mat.baseColorFactor.a;

    vec4 mrSample = texture(uMetallicRoughness, vUV);
    float metallic = clamp(mrSample.b * mat.metallicFactor, 0.0, 1.0);
    float roughness = clamp(mrSample.g * mat.roughnessFactor, 0.045, 1.0);

    vec3 normalSample = texture(uNormal, vUV).rgb;
    normalSample = normalSample * 2.0 - 1.0;
    normalSample = normalize(vec3(normalSample.xy * mat.normalScale, normalSample.z));

    vec3 N = normalize(vNormal);
    vec3 T = normalize(vTangent.xyz);
    T = normalize(T - N * dot(N, T));
    vec3 B = normalize(cross(N, T)) * vTangent.w;
    mat3 TBN = mat3(T, B, N);
    N = normalize(TBN * normalSample);

    float aoSample = texture(uOcclusion, vUV).r;
    float occlusion = mix(1.0, aoSample, clamp(mat.occlusionStrength, 0.0, 1.0));
    vec3 emissive = texture(uEmissive, vUV).rgb * mat.emissiveFactor.rgb;

    vec3 V = normalize(globals.cameraPos - vWorldPos);
    vec3 L = normalize(-globals.lightDir);
    vec3 H = normalize(V + L);

    float NdotL = max(dot(N, L), 0.0);
    float NdotV = max(dot(N, V), 0.0);

    vec3 F0 = mix(vec3(0.04), baseColor, metallic);
    vec3 F = fresnelSchlick(max(dot(H, V), 0.0), F0);

    float D = distributionGGX(N, H, roughness);
    float G = geometrySmith(N, V, L, roughness);

    vec3 numerator = D * G * F;
    float denominator = 4.0 * NdotV * NdotL + 0.0001;
    vec3 specular = numerator / denominator;

    vec3 kS = F;
    vec3 kD = (vec3(1.0) - kS) * (1.0 - metallic);

    vec3 lightColor = vec3(1.0, 0.98, 0.95);
    vec3 Lo = (kD * baseColor / PI + specular) * NdotL * globals.lightIntensity * lightColor;

    // Split-sum IBL
    vec3 F_ambient = fresnelSchlickRoughness(NdotV, F0, roughness);
    vec3 kD_ambient = (vec3(1.0) - F_ambient) * (1.0 - metallic);

    // Diffuse IBL
    vec3 irradiance = texture(uIrradianceMap, N).rgb;
    vec3 diffuse_ibl = irradiance * kD_ambient * baseColor;

    // Specular IBL
    vec3 R = reflect(-V, N);
    // The prefilter chain may have any number of mip levels; the renderer
    // reports `mip_levels - 1` as `globals.prefilterMaxLod`.
    vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * globals.prefilterMaxLod).rgb;
    vec2 brdf = texture(uBRDFLUT, vec2(NdotV, roughness)).rg;
    vec3 specular_ibl = prefilteredColor * (F_ambient * brdf.x + brdf.y);

    vec3 ambient = (diffuse_ibl + specular_ibl) * occlusion;

    vec3 color = ambient + Lo + emissive;

    // Output linear HDR. The postprocess composite pass applies exposure and
    // tonemapping, then writes to the sRGB swapchain which performs final
    // linear->sRGB encoding on store.
    outColor = vec4(color, alpha);
}
