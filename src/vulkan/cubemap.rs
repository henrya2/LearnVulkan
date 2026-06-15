use ash::vk;
use gpu_allocator::MemoryLocation;

use crate::vulkan::memory::{MemoryAllocator, OwnedImage};

pub struct Cubemap {
    pub image: vk::Image,
    pub allocation: Option<gpu_allocator::vulkan::Allocation>,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
    pub size: u32,
    pub mip_levels: u32,
    pub format: vk::Format,
}

impl Cubemap {
    /// Create an empty cube-compatible image with `mip_levels` mip chain.
    /// Uses the allocator; for large cubemaps (env, prefilter) the caller
    /// should use `create_dedicated` for its own `VkDeviceMemory` block.
    pub fn create(
        allocator: &mut MemoryAllocator,
        device: &ash::Device,
        name: &str,
        size: u32,
        mip_levels: u32,
        format: vk::Format,
        usage: vk::ImageUsageFlags,
        dedicated: bool,
    ) -> Self {
        let image_info = vk::ImageCreateInfo::default()
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
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let owned: OwnedImage = if dedicated {
            allocator.create_dedicated_image(device, name, &image_info)
        } else {
            allocator.create_image(device, name, &image_info, MemoryLocation::GpuOnly)
        };

        let view = unsafe {
            device
                .create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(owned.image)
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
            image: owned.image,
            allocation: owned.allocation,
            view,
            sampler,
            size,
            mip_levels,
            format,
        }
    }

    pub unsafe fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe {
            device.destroy_sampler(self.sampler, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
        }
        if let Some(allocation) = self.allocation.take() {
            allocator
                .inner
                .free(allocation)
                .expect("Failed to free cubemap allocation");
        }
    }
}
