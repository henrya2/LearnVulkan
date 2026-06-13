use bytemuck::{Pod, Zeroable};

/// Number of mips in the bloom pyramid. Each mip is weighted by
/// `bloom_weights[i]` in the composite shader. See `BloomPyramid::MIP_COUNT`.
pub const BLOOM_MIP_COUNT: usize = 8;

/// Per-frame postprocess uniform, shared across bright, blur, and composite
/// fragment shaders. Each shader uses a subset of the fields.
///
/// # std140 layout (must match the GLSL `PostProcessUBO` declarations)
///
/// All three postprocess shaders (`bright.frag`, `blur.frag`, `composite.frag`)
/// declare the block identically: a leading vec4 of scalar controls followed
/// by `vec4 bloom_weights[2]`. In std140, `vec4[2]` is 16-byte-aligned and
/// occupies 32 B starting at offset 16.
///
/// | Offset | Bytes | Field |
/// |--------|-------|-------|
/// | 0      | 16    | `exposure`, `bloom_threshold`, `bloom_knee`, `bloom_intensity` (vec4) |
/// | 16     | 32    | `bloom_weights` (2 vec4s — 8 logical weights packed in .x..w) |
/// | 48     | 4     | `tonemap_op` (uint) |
/// | 52     | 12    | `_pad[3]` (std140 block-size round-up to 64) |
/// | total  | 64    | |
///
/// References:
/// - Vulkan Guide, *Shader Memory Layout / Standard Buffer Layout*,
///   §"std140 Layout" — <https://docs.vulkan.org/guide/latest/shader_memory_layout.html>
/// - Vulkan Spec, *Offset and Stride Assignment* — `float[N]` has base
///   alignment 16 in std140
///   <https://docs.vulkan.org/spec/latest/chapters/interfaces.html#interfaces-resources-standard-layout>
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PostProcessUBO {
    pub exposure: f32,            // offset 0
    pub bloom_threshold: f32,     // offset 4
    pub bloom_knee: f32,          // offset 8
    pub bloom_intensity: f32,     // offset 12
    pub bloom_weights: [glam::Vec4; BLOOM_MIP_COUNT / 4], // offset 16, 32 B
    pub tonemap_op: u32,          // offset 48 (0=linear, 1=reinhard, 2=aces)
    /// std140 block-size rounding: the GPU block is rounded up to a multiple
    /// of 16, so the trailing 12 B must be present on the CPU side. The GLSL
    /// `PostProcessUBO` block does not declare a corresponding member.
    pub _pad: [u32; 3],           // offset 52
}

impl PostProcessUBO {
    pub fn set_bloom_weights(&mut self, weights: &[f32; BLOOM_MIP_COUNT]) {
        for i in 0..(BLOOM_MIP_COUNT / 4) {
            self.bloom_weights[i].x = weights[i * 4 + 0];
            self.bloom_weights[i].y = weights[i * 4 + 1];
            self.bloom_weights[i].z = weights[i * 4 + 2];
            self.bloom_weights[i].w = weights[i * 4 + 3];
        }
    }
}

impl Default for PostProcessUBO {
    fn default() -> Self {
        let mut ubo = Self {
            exposure: 0.0,
            bloom_threshold: 1.0,
            bloom_knee: 0.5,
            bloom_intensity: 0.04,
            bloom_weights: [glam::Vec4::ZERO; BLOOM_MIP_COUNT / 4],
            tonemap_op: 2, // ACES
            _pad: [0; 3],
        };
        // Approximate Gaussian weights (sum = 1.225, not normalised on purpose
        // — the bloom intensity is a separate global multiplier).
        ubo.set_bloom_weights(&[0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05, 0.025]);
        ubo
    }
}

/// Blur push constants: texel size and direction. 12 bytes total. Push
/// constants in Vulkan are tightly packed (no std140 padding); the struct
/// matches the shader's `BlurPC` block exactly.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BlurPushConstants {
    pub texel_size: [f32; 2],
    pub direction: i32,
}

const _: () = assert!(std::mem::size_of::<BlurPushConstants>() == 12);

// Compile-time check: the UBO must match the std140 size the shaders see.
const _: [(); 64] = [(); std::mem::size_of::<PostProcessUBO>()];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postprocess_ubo_is_64_bytes() {
        assert_eq!(std::mem::size_of::<PostProcessUBO>(), 64);
    }

    #[test]
    fn set_bloom_weights_packs_eight_weights_into_two_vec4s() {
        let mut ubo = PostProcessUBO::default();
        ubo.set_bloom_weights(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        // 4 weights per vec4, packed into .x, .y, .z, .w.
        assert_eq!(ubo.bloom_weights[0].x, 1.0);
        assert_eq!(ubo.bloom_weights[0].y, 2.0);
        assert_eq!(ubo.bloom_weights[0].z, 3.0);
        assert_eq!(ubo.bloom_weights[0].w, 4.0);
        assert_eq!(ubo.bloom_weights[1].x, 5.0);
        assert_eq!(ubo.bloom_weights[1].y, 6.0);
        assert_eq!(ubo.bloom_weights[1].z, 7.0);
        assert_eq!(ubo.bloom_weights[1].w, 8.0);
    }
}
