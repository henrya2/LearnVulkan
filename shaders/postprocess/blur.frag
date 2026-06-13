#version 450

// 9-tap separable Gaussian. Direction chosen by push constant.
// Center tap weighted highest, four taps each side, weights sum to 1.0.

layout(set = 0, binding = 0) uniform sampler2D uInput;

layout(set = 1, binding = 0) uniform PostProcessUBO {
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    vec4 bloom_weights[2];
    uint  tonemap_op;
} pp;

layout(push_constant) uniform BlurPC {
    vec2 uTexelSize;  // 1.0 / extent of the input image
    int  uDirection;  // 0 = horizontal, 1 = vertical
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
    vec2 step = (pc.uDirection == 0)
        ? vec2(pc.uTexelSize.x, 0.0)
        : vec2(0.0, pc.uTexelSize.y);

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
