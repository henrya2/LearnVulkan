use bytemuck::{Pod, Zeroable};

/// Per-frame global UBO. The `vec3` fields each have 16-byte std140 base
/// alignment, so a 4-byte `f32` pad is required after each `vec3` member to
/// advance to the next 16-byte boundary. The trailing `f32 prefilter_max_lod`
/// does not need a trailing pad member: std140 rounds the block size up to a
/// multiple of 16, which the GPU does automatically.
///
/// std140 layout (size = 164 B, GPU block size = 176 B after vec4 rounding):
/// | Offset | Bytes | Field                                |
/// |--------|-------|--------------------------------------|
/// | 0      | 64    | `view` (mat4)                        |
/// | 64     | 64    | `proj` (mat4)                        |
/// | 128    | 12    | `camera_pos` (vec3)                  |
/// | 140    | 4     | `__pad0` (align to 16)               |
/// | 144    | 12    | `light_dir` (vec3)                   |
/// | 156    | 4     | `light_intensity` (f32)              |
/// | 160    | 4     | `prefilter_max_lod` (f32)            |
/// | 176    |       | (block rounded up to multiple of 16) |
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: [f32; 16],
    pub proj: [f32; 16],
    pub camera_pos: [f32; 3],
    /// std140 alignment pad between `vec3 camera_pos` and `vec3 light_dir`.
    pub __pad0: f32,
    pub light_dir: [f32; 3],
    pub light_intensity: f32,
    /// Highest valid mip index for the prefilter cubemap, i.e.
    /// `prefilter.mip_levels - 1`. The PBR fragment shader maps roughness
    /// into the prefilter chain via `roughness * prefilter_max_lod`.
    pub prefilter_max_lod: f32,
}

const _: () = assert!(std::mem::size_of::<GlobalUniforms>() == 164);

/// std140 rounds the block size up to a multiple of 16. The CPU struct is
/// 164 B; the GPU-side block is 176 B. Use this for the descriptor `range` and
/// the UBO buffer size.
pub const GLOBAL_UBO_BLOCK_SIZE: u64 = 176;

/// Push constants are tightly packed in Vulkan (no std140). `mat4 model` is
/// 64 B and `uint material_index` is 4 B → 68 B total.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: [f32; 16],
    pub material_index: u32,
}

const _: () = assert!(std::mem::size_of::<PushConstants>() == 68);
