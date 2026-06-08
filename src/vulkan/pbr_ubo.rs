use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: [f32; 16],
    pub proj: [f32; 16],
    pub camera_pos: [f32; 3],
    pub _pad0: f32,
    pub light_dir: [f32; 3],
    pub light_intensity: f32,
    /// Highest valid mip index for the prefilter cubemap, i.e.
    /// `prefilter.mip_levels - 1`. The PBR fragment shader maps roughness
    /// into the prefilter chain via `roughness * prefilter_max_lod`.
    pub prefilter_max_lod: f32,
    pub _pad1: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: [f32; 16],
    pub material_index: u32,
    pub _pad: [u32; 3],
}
