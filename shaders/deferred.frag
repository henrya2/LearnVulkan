#version 450

// Deferred lighting pass.
//
// One fullscreen triangle (`postprocess/fullscreen.vert`). For every pixel it
// reads the G-buffer + depth, reconstructs the world-space position from the
// depth buffer with `invViewProj`, and runs the same Cook-Torrance GGX + IBL
// shading that `pbr.frag` runs in the forward path. The result is linear HDR
// written into the scene-color image, so the existing bloom + composite chain
// is untouched.
//
// Background pixels (depth == 1.0, i.e. no geometry wrote them) are filled
// from the environment cubemap using the reconstructed view ray, so the
// skybox needs no geometry draw in the deferred path.
//
// `globals.deferredPack.x` selects a G-buffer visualisation mode instead of
// the shaded result (debug views).

layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    mat4 invViewProj;        // inverse of (proj * view)
    vec4 cameraPos;          // .xyz used, .w reserved (channel-reuse policy)
    vec4 lightDir;           // .xyz used (negated before NdotL), .w reserved
    vec4 lightingPack;       // .x = lightIntensity, .y = prefilterMaxLod, .z..w reserved
    vec4 deferredPack;       // .x = floatBitsToUint(debugView), .yzw reserved
} globals;

layout(set = 0, binding = 2) uniform samplerCube uIrradianceMap;
layout(set = 0, binding = 3) uniform samplerCube uPrefilterMap;
layout(set = 0, binding = 4) uniform sampler2D uBRDFLUT;
layout(set = 0, binding = 5) uniform samplerCube uEnvironmentCubemap;

layout(set = 1, binding = 0) uniform sampler2D uGAlbedo;    // .rgb albedo,        .a occlusion
layout(set = 1, binding = 1) uniform sampler2D uGNormal;    // .xyz world normal,  .a roughness
layout(set = 1, binding = 2) uniform sampler2D uGEmissive;  // .rgb emissive,      .a metallic
layout(set = 1, binding = 3) uniform sampler2D uGDepth;

layout(location = 0) in vec2 vUV;
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
    // The G-buffer was rendered with the project's Y-flip viewport, so the
    // sampling UV is `vUV` with `.y` flipped — same convention as
    // `composite.frag` / `bright.frag` when they sample the scene color.
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);

    float d = texture(uGDepth, uv).r;

    // `vUV` is (ndc.xy + 1) * 0.5 with ndc.y = +1 at the top of the
    // framebuffer, so ndc.xy = vUV * 2 - 1 and no further flip is needed.
    // Depth is already in Vulkan NDC range [0, 1].
    vec4 clip = globals.invViewProj * vec4(vUV * 2.0 - 1.0, d, 1.0);
    vec3 worldPos = clip.xyz / clip.w;

    // Depth == 1.0 is the clear value: no geometry covers this pixel. The
    // reconstructed position lies on the far plane, so the view ray is exact.
    if (d >= 1.0) {
        vec3 dir = normalize(worldPos - globals.cameraPos.xyz);
        // Linear HDR, like skybox.frag — exposure + tonemapping happen in the
        // composite pass.
        outColor = vec4(textureLod(uEnvironmentCubemap, dir, 0.0).rgb, 1.0);
        return;
    }

    vec4 gAlbedo = texture(uGAlbedo, uv);
    vec4 gNormal = texture(uGNormal, uv);
    vec4 gEmissive = texture(uGEmissive, uv);

    vec3 baseColor = gAlbedo.rgb;
    float occlusion = gAlbedo.a;
    vec3 N = normalize(gNormal.xyz);
    float roughness = gNormal.a;
    vec3 emissive = gEmissive.rgb;
    float metallic = gEmissive.a;

    vec3 V = normalize(globals.cameraPos.xyz - worldPos);
    vec3 L = normalize(-globals.lightDir.xyz);
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
    vec3 Lo = (kD * baseColor / PI + specular) * NdotL * globals.lightingPack.x * lightColor;

    // Split-sum IBL
    vec3 F_ambient = fresnelSchlickRoughness(NdotV, F0, roughness);
    vec3 kD_ambient = (vec3(1.0) - F_ambient) * (1.0 - metallic);

    // Diffuse IBL
    vec3 irradiance = texture(uIrradianceMap, N).rgb;
    vec3 diffuse_ibl = irradiance * kD_ambient * baseColor;

    // Specular IBL
    vec3 R = reflect(-V, N);
    // The prefilter chain may have any number of mip levels; the renderer
    // reports `mip_levels - 1` as `globals.lightingPack.y`.
    vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * globals.lightingPack.y).rgb;
    vec2 brdf = texture(uBRDFLUT, vec2(NdotV, roughness)).rg;
    vec3 specular_ibl = prefilteredColor * (F_ambient * brdf.x + brdf.y);

    vec3 ambient = (diffuse_ibl + specular_ibl) * occlusion;

    vec3 color = ambient + Lo + emissive;

    // ---- G-buffer debug views ----
    // 0 = shaded, 1 = albedo, 2 = normal, 3 = roughness, 4 = metallic,
    // 5 = occlusion, 6 = linear depth. See `DeferredDebugView` in
    // src/vulkan/deferred/resources.rs.
    uint debugView = floatBitsToUint(globals.deferredPack.x);
    if (debugView == 1u) {
        outColor = vec4(baseColor, 1.0);
    } else if (debugView == 2u) {
        outColor = vec4(N * 0.5 + 0.5, 1.0);
    } else if (debugView == 3u) {
        outColor = vec4(vec3(roughness), 1.0);
    } else if (debugView == 4u) {
        outColor = vec4(vec3(metallic), 1.0);
    } else if (debugView == 5u) {
        outColor = vec4(vec3(occlusion), 1.0);
    } else if (debugView == 6u) {
        // Recover linear view-space depth from the projection matrix itself,
        // so no extra UBO channel is needed. With the project's left-handed
        // `perspective_lh` (depth range [0, 1]):
        //     z_ndc   = a - b / z_view,  a = proj[2][2],  b = -proj[3][2]
        //     z_view  = b / (a - z_ndc)
        //     z_near  = b / a
        //     z_far   = a * z_near / (a - 1)
        float pa = globals.proj[2][2];
        float pb = -globals.proj[3][2];
        float zNear = pb / pa;
        float zFar = pa * zNear / (pa - 1.0);
        float zView = pb / max(pa - d, 1e-6);
        outColor = vec4(vec3(clamp(zView / zFar, 0.0, 1.0)), 1.0);
    } else {
        // Output linear HDR. The postprocess composite pass applies exposure
        // and tonemapping, then writes to the sRGB swapchain which performs
        // final linear->sRGB encoding on store.
        outColor = vec4(color, 1.0);
    }
}
