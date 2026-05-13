use crate::vulkan::buffer::GpuBuffer;

pub struct GpuMesh {
    pub vertex_buffer: GpuBuffer,
    pub index_buffer: GpuBuffer,
    pub index_count: u32,
    pub material_index: usize,
    pub world_matrix: glam::Mat4,
}

impl GpuMesh {
    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            self.vertex_buffer.destroy(device);
        }
        unsafe {
            self.index_buffer.destroy(device);
        }
    }
}
