use ash::vk;
use ktx2::Reader;

use crate::vulkan::buffer::{create_buffer, with_one_time_command};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::cubemap::Cubemap;

pub fn load_ktx2_cubemap(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
    path: &str,
) -> Cubemap {
    let file_data = std::fs::read(path)
        .unwrap_or_else(|e| panic!("Failed to read KTX2 file '{}': {}", path, e));
    let reader = Reader::new(&file_data)
        .unwrap_or_else(|e| panic!("Failed to parse KTX2 file '{}': {:?}", path, e));
    let header = reader.header();

    assert_eq!(
        header.face_count, 6,
        "KTX2 file must be a cubemap (6 faces), got {}",
        header.face_count
    );

    let format = header
        .format
        .map(|f| vk::Format::from_raw(f.0.get() as i32))
        .expect("KTX2 file has no valid format");
    let size = header.pixel_width;
    let mip_levels = header.level_count;

    // Bytes per pixel for common formats
    let bytes_per_pixel = match format {
        vk::Format::R16G16B16A16_SFLOAT => 8,
        vk::Format::R32G32B32A32_SFLOAT => 16,
        vk::Format::R8G8B8A8_SRGB | vk::Format::R8G8B8A8_UNORM => 4,
        vk::Format::R16G16_SFLOAT => 4,
        f => panic!("Unsupported KTX2 format: {:?}", f),
    };

    let cubemap = Cubemap::create_empty(
        &ctx.device,
        &ctx.instance,
        ctx.physical_device,
        size,
        mip_levels,
        format,
        vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
    );

    // Transition entire image to TRANSFER_DST
    with_one_time_command(ctx, command_pool, |cmd| unsafe {
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(cubemap.image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 6,
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
            std::slice::from_ref(&barrier),
        );
    });

    // Upload each mip level
    let levels: Vec<_> = reader.levels().collect();

    for (level_idx, level) in levels.iter().enumerate() {
        let level_size = size >> level_idx;
        let face_size_bytes = (level_size * level_size) as u64 * bytes_per_pixel as u64;
        let level_data = *level;

        // Create staging buffer for the entire level
        let staging = create_buffer(
            &ctx.device,
            level_data.len() as vk::DeviceSize,
            vk::BufferUsageFlags::TRANSFER_SRC,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            &ctx.instance,
            ctx.physical_device,
        );

        unsafe {
            let ptr = ctx
                .device
                .map_memory(
                    staging.memory,
                    0,
                    level_data.len() as vk::DeviceSize,
                    vk::MemoryMapFlags::empty(),
                )
                .unwrap();
            std::ptr::copy_nonoverlapping(level_data.as_ptr(), ptr as *mut u8, level_data.len());
            ctx.device.unmap_memory(staging.memory);
        }

        // Copy each face from the staging buffer
        with_one_time_command(ctx, command_pool, |cmd| unsafe {
            for face in 0..6u32 {
                let buffer_offset = face as u64 * face_size_bytes;
                let copy_region = vk::BufferImageCopy::default()
                    .buffer_offset(buffer_offset)
                    .buffer_row_length(0)
                    .buffer_image_height(0)
                    .image_subresource(vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: level_idx as u32,
                        base_array_layer: face,
                        layer_count: 1,
                    })
                    .image_extent(vk::Extent3D {
                        width: level_size,
                        height: level_size,
                        depth: 1,
                    });
                ctx.device.cmd_copy_buffer_to_image(
                    cmd,
                    staging.buffer,
                    cubemap.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    std::slice::from_ref(&copy_region),
                );
            }
        });

        unsafe { staging.destroy(&ctx.device) };
    }

    // Transition to SHADER_READ_ONLY
    with_one_time_command(ctx, command_pool, |cmd| unsafe {
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(cubemap.image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: mip_levels,
                base_array_layer: 0,
                layer_count: 6,
            })
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);
        ctx.device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            std::slice::from_ref(&barrier),
        );
    });

    cubemap
}
