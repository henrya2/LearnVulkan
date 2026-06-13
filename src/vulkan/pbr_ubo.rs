use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec4};

/// Per-frame global UBO. Every field is either a `Mat4` (4 × `Vec4`
/// columns) or a `Vec4` — the project-wide "Vec4 base element" rule. See
/// `docs/shader_buffer_mem_layout.md` for the full rationale.
///
/// **Channel-reuse policy** (project rule, see `CODEBUDDY.md`):
/// every free channel of every group-named `Vec4` is fair game for a
/// bit-packed scalar, provided the GLSL block declares the slot and the
/// Rust struct mirrors it 1:1. The 1:1 mirror means the GLSL side
/// **must** declare every channel the Rust struct exposes, even if the
/// shader never reads that channel — otherwise the CPU can write a value
/// the GLSL block has not reserved.
///
/// The `vec4` form for `camera_pos` and `light_dir` matches the GLSL
/// `vec4` declarations exactly: the shader reads `.xyz` and the trailing
/// `.w` is a **reserved slot** (currently no consumer, but documented on
/// both sides per the channel-reuse policy). The CPU leaves it at 0.
///
/// The `lighting_pack: Vec4` carries the two `float` fields
/// (`light_intensity` and `prefilter_max_lod`) in its `.x` and `.y`
/// channels. The `.z` and `.w` channels are dead on both sides (always
/// zero) and are explicitly reserved for future scalars (see the
/// free-slot inventory in `CODEBUDDY.md`).
///
/// std140 layout (struct size = 176 B = 11 × 16, GPU block size = 176 B):
///
/// | Offset | Bytes | Field                                            |
/// |--------|-------|--------------------------------------------------|
/// | 0      | 64    | `view`            (mat4)                         |
/// | 64     | 64    | `proj`            (mat4)                         |
/// | 128    | 16    | `camera_pos`      (vec4 — shader reads .xyz;     |
/// |        |       |                  .w reserved, see policy)        |
/// | 144    | 16    | `light_dir`       (vec4 — shader reads .xyz;     |
/// |        |       |                  .w reserved, see policy)        |
/// | 160    | 16    | `lighting_pack`   (vec4 — .x = light_intensity,  |
/// |        |       |                  .y = prefilter_max_lod,         |
/// |        |       |                  .z = .w = reserved)             |
/// | total  | 176   | std140 block rounds up to 176 B                  |
///
/// **Why the block is 176 B but the struct is 160 B:** std140 rounds
/// the **block** up to a multiple of 16 (GLSL 4.5 §7.6.2.2), so a
/// 160 B struct is followed by 16 B of dead space that the GPU never
/// reads. On the CPU side we let `#[repr(C)]` round the **struct** up
/// to its own align (16) automatically, which is also 176 B because
/// 10 × 16 = 176. There is no `_pad` field: the trailing
/// `lighting_pack` already provides 16 B of "struct round-up" and
/// the std140 block round-up beyond that is the same 16 B — they
/// coincide at 176 B without any padding work.
///
/// The descriptor `range` and the UBO buffer size are both 176 B
/// (`GLOBAL_UBO_BLOCK_SIZE`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: Mat4,
    pub proj: Mat4,
    /// Camera position in world space. The shader reads `.xyz`; `.w` is
    /// reserved per the channel-reuse policy (no current consumer).
    pub camera_pos: Vec4,
    /// Directional light direction (pointing **from** the light). The
    /// shader reads `.xyz` and negates it (so the convention is "light
    /// direction toward the surface, then negate"); `.w` is reserved per
    /// the channel-reuse policy.
    pub light_dir: Vec4,
    /// `.x` = `light_intensity`, `.y` = `prefilter_max_lod`,
    /// `.z` = `.w` = reserved (always 0 on CPU; GLSL has them as
    /// reserved dead channels).
    pub lighting_pack: Vec4,
}

impl GlobalUniforms {
    /// Directional light intensity. See the PBR fragment shader's
    /// `Lo` term.
    #[inline]
    pub fn set_light_intensity(&mut self, v: f32) {
        self.lighting_pack.x = v;
    }

    /// See [`Self::set_light_intensity`].
    #[inline]
    #[allow(dead_code)]
    pub fn light_intensity(&self) -> f32 {
        self.lighting_pack.x
    }

    /// Highest valid mip index for the prefilter cubemap, i.e.
    /// `prefilter.mip_levels - 1`. The PBR fragment shader maps
    /// roughness into the prefilter chain via
    /// `roughness * prefilter_max_lod`.
    #[inline]
    pub fn set_prefilter_max_lod(&mut self, v: f32) {
        self.lighting_pack.y = v;
    }

    /// See [`Self::set_prefilter_max_lod`].
    #[inline]
    #[allow(dead_code)]
    pub fn prefilter_max_lod(&self) -> f32 {
        self.lighting_pack.y
    }

    /// `camera_pos.w` reserved slot. Currently no consumer; declared on
    /// both sides per the channel-reuse policy. Future scalar (e.g. a
    /// packed "is orthographic" flag) goes here.
    #[inline]
    #[allow(dead_code)]
    pub fn set_camera_pos_w(&mut self, v: f32) {
        self.camera_pos.w = v;
    }

    /// See [`Self::set_camera_pos_w`].
    #[inline]
    #[allow(dead_code)]
    pub fn camera_pos_w(&self) -> f32 {
        self.camera_pos.w
    }

    /// `light_dir.w` reserved slot. Currently no consumer; declared on
    /// both sides per the channel-reuse policy.
    #[inline]
    #[allow(dead_code)]
    pub fn set_light_dir_w(&mut self, v: f32) {
        self.light_dir.w = v;
    }

    /// See [`Self::set_light_dir_w`].
    #[inline]
    #[allow(dead_code)]
    pub fn light_dir_w(&self) -> f32 {
        self.light_dir.w
    }
}

const _: () = assert!(std::mem::size_of::<GlobalUniforms>() == 176);

/// The CPU struct and the GPU std140 block are both 176 B. Use this for
/// the descriptor `range` and the UBO buffer size.
pub const GLOBAL_UBO_BLOCK_SIZE: u64 = 176;

/// Push constants are tightly packed in Vulkan (Vulkan 1.3 §15.8.1)
/// and the project applies the same Vec4-base-element rule here: the
/// struct is a `Mat4` followed by a `Vec4` whose `.x` carries the
/// bit-packed `material_index`. The remaining `.y`/`.z`/`.w` channels
/// are **reserved** per the channel-reuse policy (no current consumer
/// beyond `.x`). The total is 80 B (5 × 16).
///
/// The shader side declares:
///
/// ```glsl
/// layout(push_constant) uniform PushConstants {
///     mat4 model;
///     vec4 tail;       // .x = materialIndex (uint), .yzw reserved
/// } pc;
/// ```
///
/// On the CPU side, [`PushConstants::set_material_index`] takes a
/// `u32` and writes `f32::from_bits(v)` into `.x`. The shader reads
/// `floatBitsToUint(pc.tail.x)`.
///
/// Why 80 B and not 68 B: `Mat4` has 16-byte alignment, so
/// `#[repr(C)]` rounds the struct up to a multiple of 16. The trailing
/// `Vec4` field is what makes that rounding explicit and satisfies
/// `bytemuck::Pod` (no implicit padding).
///
/// Vulkan only reads the first 68 B (`mat4` + `uint`) on the shader
/// side, but the pipeline push-constant range must cover the struct's
/// `size_of`, so the range is set from this 80 B value
/// (see `renderer.rs`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: Mat4,
    /// `.x` = `material_index` (bit-packed `u32` via `f32::from_bits`).
    /// `.y` / `.z` / `.w` = reserved (per channel-reuse policy; no
    /// current consumer).
    pub tail: Vec4,
}

impl PushConstants {
    /// Set the per-draw material index. The `u32` is bit-packed into
    /// `tail.x`; the shader reads it back with `floatBitsToUint(pc.tail.x)`.
    #[inline]
    pub fn set_material_index(&mut self, v: u32) {
        self.tail.x = f32::from_bits(v);
    }

    /// See [`Self::set_material_index`].
    #[inline]
    #[allow(dead_code)]
    pub fn material_index(&self) -> u32 {
        self.tail.x.to_bits()
    }
}

const _: () = assert!(std::mem::size_of::<PushConstants>() == 80);
