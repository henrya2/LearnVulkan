use ash::vk;
use bytemuck::Pod;

use crate::vulkan::context::VulkanContext;

pub struct GpuBuffer {
    pub buffer: vk::Buffer,
    pub memory: vk::DeviceMemory,
    pub size: vk::DeviceSize,
}

impl GpuBuffer {
    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            device.destroy_buffer(self.buffer, None);
            device.free_memory(self.memory, None);
        }
    }
}

fn find_memory_type(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> u32 {
    let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    for i in 0..mem_props.memory_type_count {
        if (type_filter & (1 << i)) != 0
            && mem_props.memory_types[i as usize]
                .property_flags
                .contains(properties)
        {
            return i;
        }
    }
    panic!("Failed to find suitable memory type")
}

fn create_buffer(
    device: &ash::Device,
    size: vk::DeviceSize,
    usage: vk::BufferUsageFlags,
    properties: vk::MemoryPropertyFlags,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> GpuBuffer {
    let buffer_info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let buffer = unsafe { device.create_buffer(&buffer_info, None).unwrap() };

    let mem_reqs = unsafe { device.get_buffer_memory_requirements(buffer) };
    let mem_type = find_memory_type(
        instance,
        physical_device,
        mem_reqs.memory_type_bits,
        properties,
    );

    let alloc_info = vk::MemoryAllocateInfo::default()
        .allocation_size(mem_reqs.size)
        .memory_type_index(mem_type);
    let memory = unsafe { device.allocate_memory(&alloc_info, None).unwrap() };

    unsafe {
        device.bind_buffer_memory(buffer, memory, 0).unwrap();
    }

    GpuBuffer {
        buffer,
        memory,
        size,
    }
}

pub fn create_device_local_buffer<T: Pod>(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
    data: &[T],
    usage: vk::BufferUsageFlags,
) -> GpuBuffer {
    let device = &ctx.device;
    let size = (data.len() * std::mem::size_of::<T>()) as vk::DeviceSize;

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
        std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, ptr as *mut u8, size as usize);
        device.unmap_memory(staging.memory);
    }

    let target = create_buffer(
        device,
        size,
        usage | vk::BufferUsageFlags::TRANSFER_DST,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
        &ctx.instance,
        ctx.physical_device,
    );

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

        let copy_region = vk::BufferCopy::default().size(size);
        device.cmd_copy_buffer(
            cmd,
            staging.buffer,
            target.buffer,
            std::slice::from_ref(&copy_region),
        );

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

    unsafe {
        staging.destroy(device);
    }

    target
}
