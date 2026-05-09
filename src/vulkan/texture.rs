use ash::vk;
use std::path::Path;

use crate::vulkan::buffer::{create_buffer, find_memory_type, with_one_time_command};
use crate::vulkan::context::VulkanContext;

pub struct Texture {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
}

impl Texture {
    /// Load a PNG file, decode to RGBA8, and upload to a device-local image.
    pub fn from_png<P: AsRef<Path>>(
        ctx: &VulkanContext,
        command_pool: vk::CommandPool,
        path: P,
    ) -> Self {
        let path = path.as_ref();
        let img = image::open(path)
            .unwrap_or_else(|e| panic!("failed to open texture {:?}: {}", path, e))
            .to_rgba8();
        let (width, height) = img.dimensions();
        Self::from_rgba8(ctx, command_pool, img.as_raw(), width, height)
    }

    /// Create a texture from raw RGBA8 pixel data with the given dimensions.
    pub fn from_rgba8(
        ctx: &VulkanContext,
        command_pool: vk::CommandPool,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Self {
        let device = &ctx.device;
        let size = (width as vk::DeviceSize) * (height as vk::DeviceSize) * 4;
        assert_eq!(pixels.len() as vk::DeviceSize, size);

        let mip_levels = (width.max(height) as f32).log2().floor() as u32 + 1;

        // Verify that the device supports blitting with R8G8B8A8_SRGB.
        let format_props = unsafe {
            ctx.instance
                .get_physical_device_format_properties(ctx.physical_device, vk::Format::R8G8B8A8_SRGB)
        };
        let required_blit_features = vk::FormatFeatureFlags::BLIT_SRC | vk::FormatFeatureFlags::BLIT_DST;
        assert!(
            format_props.optimal_tiling_features.contains(required_blit_features),
            "R8G8B8A8_SRGB does not support BLIT_SRC + BLIT_DST on this device; cannot generate mipmaps via blit"
        );

        // Staging buffer with pixel data.
        let staging = create_buffer(
            device,
            size,
            vk::BufferUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            &ctx.instance,
            ctx.physical_device,
        );
        unsafe {
            let ptr = device
                .map_memory(staging.memory, 0, size, vk::MemoryMapFlags::empty())
                .unwrap();
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), ptr as *mut u8, size as usize);
            device.unmap_memory(staging.memory);
        }

        // Device-local image.
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(mip_levels)
            .array_layers(1)
            .format(vk::Format::R8G8B8A8_SRGB)
            .tiling(vk::ImageTiling::OPTIMAL)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
            .samples(vk::SampleCountFlags::TYPE_1)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let image = unsafe { device.create_image(&image_info, None).unwrap() };

        let mem_reqs = unsafe { device.get_image_memory_requirements(image) };
        let mem_type = find_memory_type(
            &ctx.instance,
            ctx.physical_device,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        );
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type);
        let memory = unsafe { device.allocate_memory(&alloc_info, None).unwrap() };
        unsafe { device.bind_image_memory(image, memory, 0).unwrap() };

        // Upload via a single one-time command buffer:
        //   UNDEFINED -> TRANSFER_DST_OPTIMAL (all mip levels)
        //   copy_buffer_to_image (level 0)
        //   For each mip level i in 1..mip_levels:
        //     barrier: level i-1 TRANSFER_DST_OPTIMAL -> TRANSFER_SRC_OPTIMAL
        //     blit: level i-1 -> level i
        //   All levels TRANSFER_DST_OPTIMAL -> SHADER_READ_ONLY_OPTIMAL
        with_one_time_command(ctx, command_pool, |cmd| unsafe {
            let to_transfer = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: mip_levels,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
            ctx.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                std::slice::from_ref(&to_transfer),
            );

            let copy = vk::BufferImageCopy::default()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            ctx.device.cmd_copy_buffer_to_image(
                cmd,
                staging.buffer,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                std::slice::from_ref(&copy),
            );

            // Generate mip levels via blit chain.
            let mut mip_width = width as i32;
            let mut mip_height = height as i32;

            for i in 1..mip_levels {
                // Transition level i-1 from TRANSFER_DST_OPTIMAL to TRANSFER_SRC_OPTIMAL.
                let barrier = vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: i - 1,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
                ctx.device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    std::slice::from_ref(&barrier),
                );

                let src_offset = vk::Offset3D {
                    x: mip_width,
                    y: mip_height,
                    z: 1,
                };
                let dst_width = (mip_width / 2).max(1);
                let dst_height = (mip_height / 2).max(1);
                let dst_offset = vk::Offset3D {
                    x: dst_width,
                    y: dst_height,
                    z: 1,
                };

                let blit = vk::ImageBlit::default()
                    .src_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: i - 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .src_offsets([
                        vk::Offset3D { x: 0, y: 0, z: 0 },
                        src_offset,
                    ])
                    .dst_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: i,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .dst_offsets([
                        vk::Offset3D { x: 0, y: 0, z: 0 },
                        dst_offset,
                    ]);

                ctx.device.cmd_blit_image(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    std::slice::from_ref(&blit),
                    vk::Filter::LINEAR,
                );

                mip_width = dst_width;
                mip_height = dst_height;
            }

            // Final barrier: transition all mip levels to SHADER_READ_ONLY_OPTIMAL.
            // Levels that were blitted from are still in TRANSFER_SRC_OPTIMAL;
            // the last level written is in TRANSFER_DST_OPTIMAL.
            // We need two sub-ranges: one for the source levels, one for the last dest level.
            // However, a simpler approach: transition the full range with old_layout = TRANSFER_DST_OPTIMAL
            // for levels that were only destinations, and separately for TRANSFER_SRC_OPTIMAL levels.
            // Vulkan allows us to issue two barriers for different sub-ranges.
            let mut barriers = Vec::new();

            // Levels 0..mip_levels-1 are in TRANSFER_SRC_OPTIMAL (if mip_levels > 1).
            if mip_levels > 1 {
                barriers.push(
                    vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: mip_levels - 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        })
                        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                        .dst_access_mask(vk::AccessFlags::SHADER_READ),
                );
            }

            // The last mip level is in TRANSFER_DST_OPTIMAL.
            barriers.push(
                vk::ImageMemoryBarrier::default()
                    .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: mip_levels - 1,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    })
                    .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                    .dst_access_mask(vk::AccessFlags::SHADER_READ),
            );

            ctx.device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            );
        });

        unsafe { staging.destroy(device) };

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_SRGB)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 1,
            });
        let view = unsafe { device.create_image_view(&view_info, None).unwrap() };

        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::REPEAT)
            .address_mode_v(vk::SamplerAddressMode::REPEAT)
            .address_mode_w(vk::SamplerAddressMode::REPEAT)
            .anisotropy_enable(false)
            .max_anisotropy(1.0)
            .border_color(vk::BorderColor::INT_OPAQUE_BLACK)
            .unnormalized_coordinates(false)
            .compare_enable(false)
            .compare_op(vk::CompareOp::ALWAYS)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .mip_lod_bias(0.0)
            .min_lod(0.0)
            .max_lod((mip_levels - 1) as f32);
        let sampler = unsafe { device.create_sampler(&sampler_info, None).unwrap() };

        Self {
            image,
            memory,
            view,
            sampler,
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
