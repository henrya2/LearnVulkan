use ash::vk;

use crate::vulkan::buffer::GpuBuffer;
use crate::vulkan::texture::Texture;

/// set=0, binding=0: UNIFORM_BUFFER (vertex) — MVP
/// set=0, binding=1: COMBINED_IMAGE_SAMPLER (fragment) — base color texture
pub fn create_descriptor_set_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let bindings = [
        vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::VERTEX),
        vk::DescriptorSetLayoutBinding::default()
            .binding(1)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT),
    ];
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe { device.create_descriptor_set_layout(&info, None).unwrap() }
}

pub fn create_descriptor_pool(device: &ash::Device, frames: u32) -> vk::DescriptorPool {
    let sizes = [
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::UNIFORM_BUFFER,
            descriptor_count: frames,
        },
        vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: frames,
        },
    ];
    let info = vk::DescriptorPoolCreateInfo::default()
        .pool_sizes(&sizes)
        .max_sets(frames);
    unsafe { device.create_descriptor_pool(&info, None).unwrap() }
}

pub fn create_descriptor_sets(
    device: &ash::Device,
    layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    uniform_buffers: &[GpuBuffer],
    texture: &Texture,
) -> Vec<vk::DescriptorSet> {
    let layouts = vec![layout; uniform_buffers.len()];
    let alloc_info = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(pool)
        .set_layouts(&layouts);
    let sets = unsafe { device.allocate_descriptor_sets(&alloc_info).unwrap() };

    for (i, &set) in sets.iter().enumerate() {
        let buffer_info = vk::DescriptorBufferInfo::default()
            .buffer(uniform_buffers[i].buffer)
            .offset(0)
            .range(uniform_buffers[i].size);
        let image_info = vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(texture.view)
            .sampler(texture.sampler);

        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(std::slice::from_ref(&buffer_info)),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&image_info)),
        ];
        unsafe { device.update_descriptor_sets(&writes, &[]) };
    }

    sets
}
