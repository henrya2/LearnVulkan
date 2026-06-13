use bytemuck::{Pod, Zeroable};

/// Per-frame global UBO. `camera_pos` and `light_dir` are stored as
/// `Vec4` (16 bytes each) rather than `vec3` + an explicit `f32` pad.
/// The byte layout in the GPU buffer is identical to the std140
/// `vec3` form: the GPU reads only the first 12 bytes of each field
/// as a `vec3`, and the trailing 4 "w" bytes are treated by std140
/// as the `vec3` alignment padding. The `.w` component is set to
/// `1.0` for `camera_pos` (homogeneous position) and `0.0` for
/// `light_dir` (direction vector) in `renderer.rs`; the shader
/// ignores it.
///
/// The trailing `_pad_tail: [f32; 2]` is purely there to make the
/// struct's `size_of` equal a multiple of 16, so that `bytemuck`'s
/// `Pod` derive accepts the struct. The GPU std140 block is 176 B
/// and the shader never reads past byte 167, so those last 8 bytes
/// are never observed on the GPU side.
///
/// std140 layout (struct size = 176 B, GPU block size = 176 B):
/// | Offset | Bytes | Field                                  |
/// |--------|-------|----------------------------------------|
/// | 0      | 64    | `view` (mat4)                          |
/// | 64     | 64    | `proj` (mat4)                          |
/// | 128    | 16    | `camera_pos` (vec4 — shader reads vec3)|
/// | 144    | 16    | `light_dir`  (vec4 — shader reads vec3)|
/// | 160    | 4     | `light_intensity` (f32)                |
/// | 164    | 4     | `prefilter_max_lod` (f32)              |
/// | 168    | 8     | `_pad_tail` (struct tail pad to 176)   |
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: glam::Mat4,
    pub proj: glam::Mat4,
    pub camera_pos: glam::Vec4,
    pub light_dir: glam::Vec4,
    pub light_intensity: f32,
    /// Highest valid mip index for the prefilter cubemap, i.e.
    /// `prefilter.mip_levels - 1`. The PBR fragment shader maps roughness
    /// into the prefilter chain via `roughness * prefilter_max_lod`.
    pub prefilter_max_lod: f32,
    /// Trailing pad so the struct is exactly 176 B (a multiple of 16),
    /// required for `bytemuck::Pod`. Not read by the GPU; see the
    /// module-level doc comment.
    pub _pad_tail: [f32; 2],
}

const _: () = assert!(std::mem::size_of::<GlobalUniforms>() == 176);

/// The CPU struct and the GPU std140 block are both 176 B. Use this for
/// the descriptor `range` and the UBO buffer size.
pub const GLOBAL_UBO_BLOCK_SIZE: u64 = 176;

/// Push constants are tightly packed in Vulkan (no std140). `mat4 model` is
/// 64 B and `uint material_index` is 4 B. `Mat4` has 16-byte alignment, so
/// the struct is padded to 80 B (12 B of explicit trailing `_pad` to satisfy
/// `bytemuck::Pod`). Vulkan only reads the first 68 B on the shader side, but
/// the push-constant range must cover the struct's `size_of`, so the pipeline
/// range is set from this size (see `renderer.rs`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: glam::Mat4,
    pub material_index: u32,
    /// Explicit trailing padding so the struct's `size_of` matches its align
    /// (16). Required for `bytemuck::Pod` (which rejects structs with implicit
    /// tail padding) and ensures the GPU push-constant range is a multiple of
    /// 16. Not read by the shader.
    pub _pad: [u32; 3],
}

const _: () = assert!(std::mem::size_of::<PushConstants>() == 80);
