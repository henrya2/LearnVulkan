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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuMaterial {
    pub base_color_factor: [f32; 4],
    pub emissive_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub _pad: [f32; 4],
}

impl From<&PbrMaterial> for GpuMaterial {
    fn from(m: &PbrMaterial) -> Self {
        Self {
            base_color_factor: m.base_color_factor,
            emissive_factor: [
                m.emissive_factor[0],
                m.emissive_factor[1],
                m.emissive_factor[2],
                0.0,
            ],
            metallic_factor: m.metallic_factor,
            roughness_factor: m.roughness_factor,
            normal_scale: m.normal_scale,
            occlusion_strength: m.occlusion_strength,
            _pad: [0.0; 4],
        }
    }
}
