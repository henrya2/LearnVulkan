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
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    float bloom_weights[8];
    uint  tonemap_op;
    uint  _pad[3];
} pp;

layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

vec3 aces(vec3 c) {
    const float a = 2.51;
    const float b = 0.03;
    const float c2 = 2.43;
    const float d = 0.59;
    const float e = 0.14;
    return clamp((c * (a * c + b)) / (c * (c2 * c + d) + e), 0.0, 1.0);
}

vec3 reinhard(vec3 c) {
    return c / (c + vec3(1.0));
}

void main() {
    // Y-flip correction: scene color was rendered with a Y-flip viewport.
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);

    vec3 scene = texture(uSceneColor, uv).rgb;

    vec3 bloom = vec3(0.0);
    bloom += texture(uBloom0, uv).rgb * pp.bloom_weights[0];
    bloom += texture(uBloom1, uv).rgb * pp.bloom_weights[1];
    bloom += texture(uBloom2, uv).rgb * pp.bloom_weights[2];
    bloom += texture(uBloom3, uv).rgb * pp.bloom_weights[3];
    bloom += texture(uBloom4, uv).rgb * pp.bloom_weights[4];
    bloom += texture(uBloom5, uv).rgb * pp.bloom_weights[5];
    bloom += texture(uBloom6, uv).rgb * pp.bloom_weights[6];
    bloom += texture(uBloom7, uv).rgb * pp.bloom_weights[7];
    bloom *= pp.bloom_intensity;

    vec3 color = scene + bloom;
    color *= pow(2.0, pp.exposure);

    vec3 mapped;
    if (pp.tonemap_op == 1u) {
        mapped = reinhard(color);
    } else if (pp.tonemap_op == 2u) {
        mapped = aces(color);
    } else {
        // 0 = linear / none (useful for debug)
        mapped = clamp(color, 0.0, 1.0);
    }

    outColor = vec4(mapped, 1.0);
}
