use ash::vk;
use bytemuck::Pod;
use gpu_allocator::MemoryLocation;

use crate::vulkan::context::VulkanContext;
use crate::vulkan::memory::OwnedBuffer;

/// Allocate a HOST_VISIBLE | HOST_COHERENT buffer, memcpy `data` into it, then
/// create the DEVICE_LOCAL target and copy staging -> target via a one-time
/// command buffer. The staging buffer is destroyed at the end of this
/// function; the target is returned for the caller to manage via
/// `OwnedBuffer::destroy`.
pub fn create_device_local_buffer<T: Pod>(
    ctx: &mut VulkanContext,
    command_pool: vk::CommandPool,
    name: &str,
    data: &[T],
    usage: vk::BufferUsageFlags,
) -> OwnedBuffer {
    let device = &ctx.device.clone();
    let size = (data.len() * std::mem::size_of::<T>()) as vk::DeviceSize;

    // Create a HOST_VISIBLE staging buffer and copy `data` into it via the
    // mapped pointer. The pointer is already at the correct sub-allocation
    // offset.
    let mut staging = ctx.allocator.create_buffer(
        device,
        &format!("{name}_staging"),
        size,
        vk::BufferUsageFlags::TRANSFER_SRC,
        MemoryLocation::CpuToGpu,
    );
    let ptr = staging
        .allocation
        .as_ref()
        .expect("create_device_local_buffer: staging allocation missing")
        .mapped_ptr()
        .expect("create_device_local_buffer: staging CpuToGpu allocation not mapped")
        .as_ptr() as *mut u8;
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, ptr, size as usize);
    }
    // The mapping will be unmapped internally when `allocator.free` runs
    // in `staging.destroy` below. No explicit `unmap_memory` call.

    // Create the DEVICE_LOCAL target.
    let target = ctx.allocator.create_buffer(
        device,
        name,
        size,
        usage | vk::BufferUsageFlags::TRANSFER_DST,
        MemoryLocation::GpuOnly,
    );

    with_one_time_command(ctx, command_pool, |cmd| unsafe {
        let copy_region = vk::BufferCopy::default().size(size);
        device.cmd_copy_buffer(
            cmd,
            staging.buffer,
            target.buffer,
            std::slice::from_ref(&copy_region),
        );
    });

    // Free the staging allocation.
    staging.destroy(device, &mut ctx.allocator);

    target
}

/// Allocate a primary command buffer from `command_pool`, begin it with ONE_TIME_SUBMIT,
/// run `record`, then submit to the graphics queue and wait for idle.
pub fn with_one_time_command<F: FnOnce(vk::CommandBuffer)>(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
    record: F,
) {
    let device = &ctx.device;
    unsafe {
        let cmd = device
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
            .unwrap()[0];

        device
            .begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .unwrap();

        record(cmd);

        device.end_command_buffer(cmd).unwrap();

        let submit_info = vk::SubmitInfo::default().command_buffers(std::slice::from_ref(&cmd));
        device
            .queue_submit(
                ctx.graphics_queue,
                std::slice::from_ref(&submit_info),
                vk::Fence::null(),
            )
            .unwrap();
        device.queue_wait_idle(ctx.graphics_queue).unwrap();

        device.free_command_buffers(command_pool, std::slice::from_ref(&cmd));
    }
}
