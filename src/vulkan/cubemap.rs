use crate::vulkan::buffer::find_memory_type;
use ash::vk;

pub struct Cubemap {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
    pub size: u32,
    pub mip_levels: u32,
    pub format: vk::Format,
}

impl Cubemap {
    pub fn create_empty(
        device: &ash::Device,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        size: u32,
        mip_levels: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
    ) -> Self {
        let image = unsafe {
            device
                .create_image(
                    &vk::ImageCreateInfo::default()
                        .flags(vk::ImageCreateFlags::CUBE_COMPATIBLE)
                        .image_type(vk::ImageType::TYPE_2D)
                        .format(format)
                        .extent(vk::Extent3D {
                            width: size,
                            height: size,
                            depth: 1,
                        })
                        .mip_levels(mip_levels)
                        .array_layers(6)
                        .samples(vk::SampleCountFlags::TYPE_1)
                        .tiling(vk::ImageTiling::OPTIMAL)
                        .usage(usage)
                        .sharing_mode(vk::SharingMode::EXCLUSIVE)
                        .initial_layout(vk::ImageLayout::UNDEFINED),
                    None,
                )
                .unwrap()
        };

        let mem_reqs = unsafe { device.get_image_memory_requirements(image) };
        let memory = unsafe {
            let mem_type = find_memory_type(
                instance,
                physical_device,
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            );
            device
                .allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(mem_reqs.size)
                        .memory_type_index(mem_type),
                    None,
                )
                .unwrap()
        };
        unsafe { device.bind_image_memory(image, memory, 0).unwrap() };

        let view = unsafe {
            device
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::CUBE)
                        .format(format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: mip_levels,
                            base_array_layer: 0,
                            layer_count: 6,
                        }),
                    None,
                )
                .unwrap()
        };

        let sampler = unsafe {
            device
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .min_lod(0.0)
                        .max_lod(mip_levels as f32 - 1.0)
                        .max_anisotropy(1.0)
                        .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE),
                    None,
                )
                .unwrap()
        };

        Cubemap {
            image,
            memory,
            view,
            sampler,
            size,
            mip_levels,
            format,
        }
    }

    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            device.destroy_sampler(self.sampler, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }
}
