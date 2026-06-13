use bytemuck::{Pod, Zeroable};
use glam::Vec4;

/// Number of mips in the bloom pyramid. Each mip is weighted by
/// `bloom_weights[i]` in the composite shader. See `BloomPyramid::MIP_COUNT`.
pub const BLOOM_MIP_COUNT: usize = 8;

/// Per-frame postprocess uniform, shared across bright, blur, and
/// composite fragment shaders. Each shader uses a subset of the fields.
///
/// # Layout (must match the GLSL `PostProcessUBO` declarations)
///
/// The block is a flat sequence of `Vec4` and `[Vec4; 2]` fields —
/// the project-wide Vec4-base-element rule. There is no `_pad`
/// field: the trailing `tonemap_pack` provides the std140 block
/// round-up automatically. Per the channel-reuse policy, every
/// channel of every field is either consumed by the GLSL block
/// or explicitly reserved on both sides — no speculative padding.
///
/// | Offset | Bytes | Field | Notes |
/// |--------|-------|-------|-------|
/// | 0      | 16    | `exposure_pack`       | `.x`=exposure, `.y`=bloom_threshold, `.z`=bloom_knee, `.w`=bloom_intensity (all 4 channels consumed) |
/// | 16     | 32    | `bloom_weights[2]`    | 8 logical weights packed in `.xyzw` of each (all 8 channels consumed) |
/// | 48     | 16    | `tonemap_pack`        | `.x`=tonemap_op (uint via `f32::from_bits`), `.y`=reserved, `.z`=reserved, `.w`=std140 block round-up |
/// | total  | 64    |                       | |
///
/// References:
/// - Vulkan Guide, *Shader Memory Layout / Standard Buffer Layout*,
///   §"std140 Layout" — <https://docs.vulkan.org/guide/latest/shader_memory_layout.html>
/// - Vulkan Spec, *Standard Buffer Layout* —
///   <https://docs.vulkan.org/spec/latest/chapters/interfaces.html#interfaces-resources-standard-layout>
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PostProcessUBO {
    /// `.x` = exposure (stops), `.y` = bloom_threshold, `.z` = bloom_knee,
    /// `.w` = bloom_intensity. All four channels are consumed.
    pub exposure_pack: Vec4,
    /// 8 bloom weights, 4 per `Vec4` channel (`.xyzw`). All eight
    /// channels are consumed.
    pub bloom_weights: [Vec4; BLOOM_MIP_COUNT / 4],
    /// `.x` = tonemap_op (bit-packed `u32` via `f32::from_bits`).
    /// `0 = linear`, `1 = Reinhard`, `2 = ACES`. `.y` and `.z`
    /// are **reserved** per the channel-reuse policy (no current
    /// consumer); `.w` is the std140 block round-up (always 0 on CPU).
    pub tonemap_pack: Vec4,
}

impl PostProcessUBO {
    /// Pack 8 logical weights into the 2 `Vec4`s. Weights are
    /// interpreted 4-per-element in order: `weights[0..4]` go into
    /// `bloom_weights[0]`, `weights[4..8]` into `bloom_weights[1]`.
    pub fn set_bloom_weights(&mut self, weights: &[f32; BLOOM_MIP_COUNT]) {
        for i in 0..(BLOOM_MIP_COUNT / 4) {
            self.bloom_weights[i].x = weights[i * 4 + 0];
            self.bloom_weights[i].y = weights[i * 4 + 1];
            self.bloom_weights[i].z = weights[i * 4 + 2];
            self.bloom_weights[i].w = weights[i * 4 + 3];
        }
    }

    /// Exposure in stops. Applied as `pow(2, exposure)` in the
    /// composite shader.
    #[inline]
    #[allow(dead_code)]
    pub fn set_exposure(&mut self, v: f32) {
        self.exposure_pack.x = v;
    }

    /// See [`Self::set_exposure`].
    #[inline]
    #[allow(dead_code)]
    pub fn exposure(&self) -> f32 {
        self.exposure_pack.x
    }

    /// Soft-knee threshold (Frostbite style).
    #[inline]
    pub fn set_bloom_threshold(&mut self, v: f32) {
        self.exposure_pack.y = v;
    }

    /// See [`Self::set_bloom_threshold`].
    #[inline]
    #[allow(dead_code)]
    pub fn bloom_threshold(&self) -> f32 {
        self.exposure_pack.y
    }

    /// Knee width, fraction of `bloom_threshold`.
    #[inline]
    pub fn set_bloom_knee(&mut self, v: f32) {
        self.exposure_pack.z = v;
    }

    /// See [`Self::set_bloom_knee`].
    #[inline]
    #[allow(dead_code)]
    pub fn bloom_knee(&self) -> f32 {
        self.exposure_pack.z
    }

    /// Bloom intensity multiplier applied after the weighted sum.
    #[inline]
    pub fn set_bloom_intensity(&mut self, v: f32) {
        self.exposure_pack.w = v;
    }

    /// See [`Self::set_bloom_intensity`].
    #[inline]
    #[allow(dead_code)]
    pub fn bloom_intensity(&self) -> f32 {
        self.exposure_pack.w
    }

    /// Tonemap operator. The shader's `if/else` chain picks
    /// `0 = linear/none`, `1 = Reinhard`, `2 = ACES`.
    /// On the CPU the `u32` is bit-packed into `tonemap_pack.x`
    /// via `f32::from_bits`; the GPU reads it back with
    /// `floatBitsToUint(pp.tonemap_pack.x)`.
    #[inline]
    pub fn set_tonemap_op(&mut self, v: u32) {
        self.tonemap_pack.x = f32::from_bits(v);
    }

    /// See [`Self::set_tonemap_op`].
    #[inline]
    #[allow(dead_code)]
    pub fn tonemap_op(&self) -> u32 {
        self.tonemap_pack.x.to_bits()
    }
}

impl Default for PostProcessUBO {
    fn default() -> Self {
        let mut ubo = Self {
            exposure_pack: Vec4::ZERO,
            bloom_weights: [Vec4::ZERO; BLOOM_MIP_COUNT / 4],
            tonemap_pack: Vec4::ZERO,
        };
        // Canonical defaults.
        ubo.set_bloom_threshold(1.0);
        ubo.set_bloom_knee(0.5);
        ubo.set_bloom_intensity(0.04);
        ubo.set_tonemap_op(2); // ACES
        // Approximate Gaussian weights (sum = 1.225, not normalised on purpose
        // — the bloom intensity is a separate global multiplier).
        ubo.set_bloom_weights(&[0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05, 0.025]);
        ubo
    }
}

/// Blur push constants. Vulkan push constants are tightly packed
/// (no std140), but the project still applies the Vec4-base-element
/// rule: the struct is a single `Vec4` whose `.xy` channels carry
/// the texel size and whose `.z` channel carries the bit-packed
/// `i32` direction. `.w` is **reserved** per the channel-reuse policy
/// (no current consumer).
///
/// The shader side declares:
/// ```glsl
/// layout(push_constant) uniform BlurPC {
///     vec4 params;   // .xy = uTexelSize, .z = intBitsToFloat(uDirection), .w reserved
/// } pc;
/// ```
///
/// Total struct size: 16 B (one `Vec4`). Vulkan's push-constant range
/// is set to this size; only the first 12 B carry meaningful data on
/// the GPU, but the extra 4 B are harmless.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BlurPushConstants {
    /// `.xy` = `texel_size` (1.0 / input image extent).
    /// `.z` = `direction` (bit-packed `i32` via `f32::from_bits`).
    /// `.w` = reserved (per channel-reuse policy; no current consumer).
    pub params: Vec4,
}

impl BlurPushConstants {
    /// `1.0 / extent` of the input image in each axis.
    #[inline]
    pub fn set_texel_size(&mut self, x: f32, y: f32) {
        self.params.x = x;
        self.params.y = y;
    }

    /// See [`Self::set_texel_size`].
    #[inline]
    #[allow(dead_code)]
    pub fn texel_size(&self) -> [f32; 2] {
        [self.params.x, self.params.y]
    }

    /// `0` = horizontal, `1` = vertical. The `i32` is bit-packed into
    /// `params.z`; the GPU reads `intBitsToFloat(pc.params.z)`.
    #[inline]
    pub fn set_direction(&mut self, v: i32) {
        self.params.z = f32::from_bits(v as u32);
    }

    /// See [`Self::set_direction`].
    #[inline]
    #[allow(dead_code)]
    pub fn direction(&self) -> i32 {
        self.params.z.to_bits() as i32
    }
}

const _: () = assert!(std::mem::size_of::<BlurPushConstants>() == 16);

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

    #[test]
    fn tonemap_op_round_trips_through_bit_cast() {
        let mut ubo = PostProcessUBO::default();
        for v in [0u32, 1, 2, 42, u32::MAX] {
            ubo.set_tonemap_op(v);
            assert_eq!(ubo.tonemap_op(), v);
        }
    }

    #[test]
    fn blur_push_direction_round_trips_through_bit_cast() {
        let mut pc = BlurPushConstants { params: Vec4::ZERO };
        for v in [0i32, 1, -1, i32::MIN, i32::MAX] {
            pc.set_direction(v);
            assert_eq!(pc.direction(), v);
        }
    }
}
