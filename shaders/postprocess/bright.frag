#version 450

// Soft-knee bright pass.
// Extracts highlights above a threshold from the HDR scene color.
// Reads scene color (which was rendered with a Y-flip viewport, so flip vUV
// when sampling).

layout(set = 0, binding = 0) uniform sampler2D uSceneColor;

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

void main() {
    // Compensate for the project's Y-flip viewport when sampling back the scene.
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
    vec3 color = texture(uSceneColor, uv).rgb;

    float brightness = max(max(color.r, color.g), color.b);
    float threshold = pp.bloom_threshold;
    float knee = pp.bloom_knee * threshold + 1e-5;

    // Frostbite-style soft threshold.
    float soft = brightness - threshold + knee;
    soft = clamp(soft, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-5);
    float contribution = max(soft, brightness - threshold) / max(brightness, 1e-5);

    outColor = vec4(color * contribution, 1.0);
}
