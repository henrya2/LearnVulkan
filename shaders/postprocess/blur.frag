#version 450

// 9-tap separable Gaussian. Direction chosen by push constant.
// Center tap weighted highest, four taps each side, weights sum to 1.0.

layout(set = 0, binding = 0) uniform sampler2D uInput;

layout(set = 1, binding = 0) uniform PostProcessUBO {
    vec4 exposurePack;        // .x = exposure, .y = bloom_threshold, .z = bloom_knee, .w = bloom_intensity
    vec4 bloom_weights[2];    // 8 logical weights packed in .xyzw of each
    vec4 tonemapPack;         // .x = floatBitsToUint(tonemap_op), .yzw = 0 (dead)
} pp;

layout(push_constant) uniform BlurPC {
    vec4 params;   // .xy = uTexelSize, .z = intBitsToFloat(uDirection), .w = 0 (dead)
} pc;

layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

// Standard 9-tap Gaussian weights (sigma ~= 2.0). Sum == 1.0.
const float W0 = 0.227027;
const float W1 = 0.194594;
const float W2 = 0.121622;
const float W3 = 0.054054;
const float W4 = 0.016216;

void main() {
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
    vec2 step = (floatBitsToInt(pc.params.z) == 0)
        ? vec2(pc.params.x, 0.0)
        : vec2(0.0, pc.params.y);

    vec3 color = texture(uInput, uv).rgb * W0;
    color += texture(uInput, uv + step * 1.0).rgb * W1;
    color += texture(uInput, uv - step * 1.0).rgb * W1;
    color += texture(uInput, uv + step * 2.0).rgb * W2;
    color += texture(uInput, uv - step * 2.0).rgb * W2;
    color += texture(uInput, uv + step * 3.0).rgb * W3;
    color += texture(uInput, uv - step * 3.0).rgb * W3;
    color += texture(uInput, uv + step * 4.0).rgb * W4;
    color += texture(uInput, uv - step * 4.0).rgb * W4;

    outColor = vec4(color, 1.0);
}
