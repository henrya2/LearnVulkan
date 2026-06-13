use bytemuck::{Pod, Zeroable};
use glam::Vec4;

pub struct PbrMaterial {
    pub base_color_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub emissive_factor: [f32; 3],
    pub base_color_texture: Option<usize>,
    pub metallic_roughness_texture: Option<usize>,
    pub normal_texture: Option<usize>,
    pub occlusion_texture: Option<usize>,
    pub emissive_texture: Option<usize>,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
}

impl PbrMaterial {
    pub fn default_gltf() -> Self {
        Self {
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            metallic_factor: 1.0,
            roughness_factor: 1.0,
            emissive_factor: [0.0, 0.0, 0.0],
            base_color_texture: None,
            metallic_roughness_texture: None,
            normal_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
            normal_scale: 1.0,
            occlusion_strength: 1.0,
        }
    }
}

/// GPU-side material layout. The `Material` struct in `pbr.frag` is
/// declared inside a `std140` UBO; as an array element, the array
/// stride must be a multiple of 16. The struct is 48 B, which is
/// already a multiple of 16, so no trailing pad member is required.
///
/// # Layout (must match the GLSL `Material` declaration)
///
/// | Offset | Bytes | Field | Notes |
/// |--------|-------|-------|-------|
/// | 0      | 16    | `base_color_factor` | vec4 — RGB multiplied with base-color sample, A multiplied with alpha |
/// | 16     | 16    | `emissive_factor`   | vec4 — only `.rgb` used (emissive multiplier); `.w` is the std140 alignment pad |
/// | 32     | 16    | `factor_pack`       | vec4 — `.x`=metallic_factor, `.y`=roughness_factor, `.z`=normal_scale, `.w`=occlusion_strength |
/// | total  | 48    |                      | |
///
/// All four trailing scalars are now packed into `factor_pack`, per
/// the project-wide Vec4-base-element rule. No `_pad` field is needed
/// because `factor_pack` is exactly 16 B and `#[repr(C)]` lines the
/// three `Vec4` fields up at offsets 0 / 16 / 32.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuMaterial {
    pub base_color_factor: Vec4,
    /// `.rgb` is the emissive multiplier. `.w` is the std140 alignment
    /// pad (the GPU never reads it; the Rust struct keeps it at 0).
    pub emissive_factor: Vec4,
    /// `.x` = `metallic_factor`, `.y` = `roughness_factor`,
    /// `.z` = `normal_scale`, `.w` = `occlusion_strength`.
    pub factor_pack: Vec4,
}

const _: () = assert!(std::mem::size_of::<GpuMaterial>() == 48);

impl From<&PbrMaterial> for GpuMaterial {
    fn from(m: &PbrMaterial) -> Self {
        Self {
            base_color_factor: Vec4::new(
                m.base_color_factor[0],
                m.base_color_factor[1],
                m.base_color_factor[2],
                m.base_color_factor[3],
            ),
            emissive_factor: Vec4::new(
                m.emissive_factor[0],
                m.emissive_factor[1],
                m.emissive_factor[2],
                0.0,
            ),
            factor_pack: Vec4::new(
                m.metallic_factor,
                m.roughness_factor,
                m.normal_scale,
                m.occlusion_strength,
            ),
        }
    }
}
