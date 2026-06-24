#version 450

// Final composite. Combines:
//   * scene color (linear HDR)
//   * 8 bloom mip samples
//   * exposure (stops, applied as pow(2, exposure))
//   * tonemapping (Linear / Reinhard / ACES)
//   * writes to sRGB swapchain (Vulkan performs final linear->sRGB encoding on store)

layout(set = 0, binding = 0) uniform sampler2D uSceneColor;
layout(set = 0, binding = 1) uniform sampler2D uBloom0;
layout(set = 0, binding = 2) uniform sampler2D uBloom1;
layout(set = 0, binding = 3) uniform sampler2D uBloom2;
layout(set = 0, binding = 4) uniform sampler2D uBloom3;
layout(set = 0, binding = 5) uniform sampler2D uBloom4;
layout(set = 0, binding = 6) uniform sampler2D uBloom5;
layout(set = 0, binding = 7) uniform sampler2D uBloom6;
layout(set = 0, binding = 8) uniform sampler2D uBloom7;

layout(set = 1, binding = 0) uniform PostProcessUBO {
    vec4 exposurePack;        // .x = exposure, .y = bloom_threshold, .z = bloom_knee, .w = bloom_intensity
    vec4 bloom_weights[2];    // 8 logical weights packed in .xyzw of each
    vec4 tonemapPack;         // .x = floatBitsToUint(tonemap_op), .yzw reserved (channel-reuse policy; .w is the std140 block round-up)
} pp;

layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

// ACES filmic tonemapping — Stephen Hill's RRT+ODT fit (2017).
//
// This is the de-facto standard replacement for the Narkowicz simplified
// curve: it is a least-squares fit of the full RRT (Reference Rendering
// Transform) + ODT (Output Device Transform) pipeline baked into a single
// per-channel formula, with a pre-multiply input matrix that converts the
// working color space to AP1 (ACES RRT space) and a post-multiply output
// matrix that converts back to display sRGB. Source:
//   https://github.com/TheRealMJP/BakingLab/blob/master/BakingLab/ACES.hlsl
//
// Input is linear sRGB primaries (post-exposure) in [0, +inf).
// Output is linear sRGB primaries clamped to [0, 1]. The Vulkan swapchain
// then performs the final linear->sRGB encoding on store.
//
// **IMPORTANT – GLSL column-major:**
// The HLSL reference declares `float3x3` rows; GLSL `mat3` is always
// column-major.  Each `vec3(…)` passed to the `mat3` constructor becomes a
// **column**.  To port HLSL row-major data verbatim the values must be
// **transposed**: `mat3(col0, col1, col2)` where
//   col_j[i] = HLSL_row_i[j].
// Using the untransposed HLSL rows as GLSL columns would silently produce
// the matrix transpose, which causes a strong magenta/purple colour cast
// in any bright area with non-zero green and blue because the output
// matrix's negative coefficients land in the wrong slots.

// sRGB => XYZ => D65_2_D60 => AP1 => RRT_SAT
const mat3 ACESInputMat = mat3(
    vec3(0.59719, 0.07600, 0.02840),  // col 0 — ported HLSL row 0
    vec3(0.35458, 0.90834, 0.13383),  // col 1 — ported HLSL row 1
    vec3(0.04823, 0.01566, 0.83777)   // col 2 — ported HLSL row 2
);

// ODT_SAT => XYZ => D60_2_D65 => sRGB
const mat3 ACESOutputMat = mat3(
    vec3( 1.60475, -0.10208, -0.00327),  // col 0 — ported HLSL row 0
    vec3(-0.53108,  1.10813, -0.07276),  // col 1 — ported HLSL row 1
    vec3(-0.07367, -0.00605,  1.07602)   // col 2 — ported HLSL row 2
);

vec3 acesRRTAndODTFit(vec3 v) {
    vec3 a = v * (v + 0.0245786) - 0.000090537;
    vec3 b = v * (0.983729 * v + 0.4329510) + 0.238081;
    return a / b;
}

vec3 aces(vec3 c) {
    c = ACESInputMat * c;
    c = acesRRTAndODTFit(c);
    c = ACESOutputMat * c;
    return clamp(c, 0.0, 1.0);
}

// Luminance Reinhard with per-channel color preservation.
// This is NOT the canonical Reinhard-Jodie (which applies per-channel),
// but a luminance-based variant: compression is applied to the luminance L
// only, while the per-channel ratio color/L is preserved. The whitepoint
// correction keeps bright highlights from desaturating to white.
// L_white = 4.0 (scene-linear luminance that maps to 1.0 in the display;
//                  tunable for taste; 4.0 preserves "sun-like" highlights)
const float L_WHITE = 4.0;

vec3 reinhardLuminance(vec3 c) {
    float L = dot(c, vec3(0.2126, 0.7152, 0.0722)); // Rec.709 luminance
    float Lt = L / (1.0 + L);
    // Per-channel whitepoint correction: each channel tints towards
    // (channel / L) * L_white / (1 + L_white) at the brightest end.
    vec3 cTint = (L > 0.0) ? (c / L) * Lt : vec3(0.0);
    vec3 cWhite = (L > 0.0) ? (c / L) * (L_WHITE / (1.0 + L_WHITE)) : vec3(0.0);
    return mix(cTint, cWhite, Lt * Lt);
}

void main() {
    // Y-flip correction: scene color was rendered with a Y-flip viewport.
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);

    vec3 scene = texture(uSceneColor, uv).rgb;

    vec3 bloom = vec3(0.0);
    bloom += texture(uBloom0, uv).rgb * pp.bloom_weights[0].r;
    bloom += texture(uBloom1, uv).rgb * pp.bloom_weights[0].g;
    bloom += texture(uBloom2, uv).rgb * pp.bloom_weights[0].b;
    bloom += texture(uBloom3, uv).rgb * pp.bloom_weights[0].a;
    bloom += texture(uBloom4, uv).rgb * pp.bloom_weights[1].r;
    bloom += texture(uBloom5, uv).rgb * pp.bloom_weights[1].g;
    bloom += texture(uBloom6, uv).rgb * pp.bloom_weights[1].b;
    bloom += texture(uBloom7, uv).rgb * pp.bloom_weights[1].a;
    bloom *= pp.exposurePack.w;

    vec3 color = scene + bloom;
    // Exposure: positive stops brighten, negative stops darken.
    color *= pow(2.0, pp.exposurePack.x);

    vec3 mapped;
    if (floatBitsToUint(pp.tonemapPack.x) == 1u) {
        mapped = reinhardLuminance(color);
    } else if (floatBitsToUint(pp.tonemapPack.x) == 2u) {
        mapped = aces(color);
    } else {
        // 0 = linear / none. Useful for debug: shows raw HDR (clipped to LDR).
        // No gamma / no sRGB curve here — Vulkan encodes linear->sRGB on store.
        mapped = clamp(color, 0.0, 1.0);
    }

    outColor = vec4(mapped, 1.0);
}
