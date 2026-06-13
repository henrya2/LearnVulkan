use bytemuck::{Pod, Zeroable};

/// Number of mips in the bloom pyramid. Each mip is weighted by
/// `bloom_weights[i]` in the composite shader. See `BloomPyramid::MIP_COUNT`.
pub const BLOOM_MIP_COUNT: usize = 8;

/// Per-frame postprocess uniform, shared across bright, blur, and composite
/// fragment shaders. Each shader uses a subset of the fields.
///
/// # std140 layout (must match the GLSL `PostProcessUBO` declarations)
///
/// GLSL with no explicit block layout uses std140 in Vulkan. In std140, a
/// `float[N]` array is laid out like `vec4[N]` (each element aligned to a
/// vec4 boundary), so the array occupies `N * 16` bytes and individual
/// elements are 16 bytes apart. The full struct therefore occupies 160 bytes:
///
/// | Offset | Bytes | Field |
/// |--------|-------|-------|
/// | 0      | 16    | `exposure`, `bloom_threshold`, `bloom_knee`, `bloom_intensity` (vec4) |
/// | 16     | 128   | `bloom_weights` (8 vec4s — actual weight in slot 0 of each vec4, rest padding) |
/// | 144    | 4     | `tonemap_op` (uint) |
/// | 160    |       | (block rounded up to multiple of 16 by std140) |
/// | total  | 160   | |
///
/// The trailing 12 B between `tonemap_op` and the 160 B block end are part of
/// the std140 block-size round-up to a vec4 boundary. The CPU struct holds
/// them explicitly as `_pad[3]` so `size_of` matches the GPU's 160 B block;
/// the GLSL block does not need a corresponding member — std140 rounds the
/// block size up automatically.
///
/// The 8 logical weights live at `bloom_weights[0]`, `bloom_weights[4]`,
/// `bloom_weights[8]`, `bloom_weights[12]`, `bloom_weights[16]`,
/// `bloom_weights[20]`, `bloom_weights[24]`, `bloom_weights[28]`. Use
/// [`PostProcessUBO::set_bloom_weights`] to write the logical weights; the
/// padding slots will be zeroed. (Strictly only the weight slots need to be
/// set, but zeroing the padding is harmless and keeps the UBO dump tidy.)
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
    /// std140 `float[8]` packed as 8 vec4s. Use [`Self::set_bloom_weights`].
    /// Offset 16, length 128 bytes.
    pub bloom_weights: [f32; BLOOM_MIP_COUNT * 4],
    pub tonemap_op: u32,          // offset 144 (0=linear, 1=reinhard, 2=aces)
    /// std140 block-size rounding: the GPU block is rounded up to a multiple
    /// of 16, so the trailing 12 B must be present on the CPU side. The GLSL
    /// `PostProcessUBO` block does not declare a corresponding member.
    pub _pad: [u32; 3],           // offset 148
}

impl PostProcessUBO {
    /// Set the 8 logical bloom weights, scattering them into the std140
    /// vec4-strided slots and zeroing the padding.
    pub fn set_bloom_weights(&mut self, weights: &[f32; BLOOM_MIP_COUNT]) {
        for i in 0..BLOOM_MIP_COUNT {
            self.bloom_weights[i * 4] = weights[i];
            // Zero the three padding floats that follow each weight in std140.
            self.bloom_weights[i * 4 + 1] = 0.0;
            self.bloom_weights[i * 4 + 2] = 0.0;
            self.bloom_weights[i * 4 + 3] = 0.0;
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
            bloom_weights: [0.0; BLOOM_MIP_COUNT * 4],
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
const _: [(); 160] = [(); std::mem::size_of::<PostProcessUBO>()];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postprocess_ubo_is_160_bytes() {
        assert_eq!(std::mem::size_of::<PostProcessUBO>(), 160);
    }

    #[test]
    fn set_bloom_weights_places_values_at_stride_16_offsets() {
        let mut ubo = PostProcessUBO::default();
        ubo.set_bloom_weights(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        // Each weight at index i*4, padding zeros in i*4+1..i*4+3.
        for i in 0..BLOOM_MIP_COUNT {
            let base = i * 4;
            assert_eq!(ubo.bloom_weights[base], (i + 1) as f32);
            assert_eq!(ubo.bloom_weights[base + 1], 0.0);
            assert_eq!(ubo.bloom_weights[base + 2], 0.0);
            assert_eq!(ubo.bloom_weights[base + 3], 0.0);
        }
    }
}
