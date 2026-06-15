use crate::vulkan::memory::{MemoryAllocator, OwnedBuffer};

pub struct GpuMesh {
    pub vertex_buffer: OwnedBuffer,
    pub index_buffer: OwnedBuffer,
    pub index_count: u32,
    pub material_index: usize,
    pub world_matrix: glam::Mat4,
}

impl GpuMesh {
    pub unsafe fn destroy(&self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe {
            // `to_mut` would clone the OwnedBuffer which is incorrect — we
            // want to free this exact allocation. OwnedBuffer::destroy takes
            // &mut self, so we use mutable references via raw pointers.
            let vb_ptr: *mut OwnedBuffer =
                &self.vertex_buffer as *const OwnedBuffer as *mut OwnedBuffer;
            let ib_ptr: *mut OwnedBuffer =
                &self.index_buffer as *const OwnedBuffer as *mut OwnedBuffer;
            (*vb_ptr).destroy(device, allocator);
            (*ib_ptr).destroy(device, allocator);
        }
    }
}
