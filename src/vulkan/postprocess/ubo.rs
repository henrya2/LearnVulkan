use bytemuck::{Pod, Zeroable};

/// Per-frame postprocess uniform, shared across bright, blur, and composite
/// fragment shaders. Each shader uses a subset of the fields.
///
/// std140 layout: float[8] aligns to 16, so `bloom_weights` starts at offset 16.
/// `tonemap_op` + `_pad` occupy 16 bytes at offset 48. Total: 64 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PostProcessUBO {
    pub exposure: f32,            // offset 0
    pub bloom_threshold: f32,     // offset 4
    pub bloom_knee: f32,          // offset 8
    pub bloom_intensity: f32,     // offset 12
    pub bloom_weights: [f32; 8],  // offset 16
    pub tonemap_op: u32,          // offset 48 (0=linear, 1=reinhard, 2=aces)
    pub _pad: [u32; 3],           // offset 52
}

impl Default for PostProcessUBO {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            bloom_threshold: 1.0,
            bloom_knee: 0.5,
            bloom_intensity: 0.04,
            bloom_weights: [0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05, 0.025],
            tonemap_op: 2, // ACES
            _pad: [0; 3],
        }
    }
}

/// Blur push constants: texel size and direction. 12 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BlurPushConstants {
    pub texel_size: [f32; 2],
    pub direction: i32,
    pub _pad: i32,
}
