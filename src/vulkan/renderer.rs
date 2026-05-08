use glam::Mat4;

use crate::mesh;
use crate::vulkan::buffer::{GpuBuffer, create_buffer, create_device_local_buffer};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::descriptors::{
    create_descriptor_pool, create_descriptor_set_layout, create_descriptor_sets,
};
use crate::vulkan::pipeline::{PipelineData, create_pipeline, create_render_pass};
use crate::vulkan::swapchain::{
    SwapchainData, cleanup_swapchain, create_swapchain, find_depth_format,
};
use crate::vulkan::texture::Texture;
use ash::vk;

const MAX_FRAMES_IN_FLIGHT: usize = 2;
const UBO_SIZE: vk::DeviceSize = 64; // one mat4

pub struct Renderer {
    pub device: ash::Device,
    pub swapchain: SwapchainData,
    pub pipeline: PipelineData,
    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub image_available: Vec<vk::Semaphore>,
    pub render_finished: Vec<vk::Semaphore>,
    pub in_flight: Vec<vk::Fence>,
    pub images_in_flight: Vec<Option<vk::Fence>>,
    pub current_frame: usize,
    pub framebuffer_resized: bool,
    pub cube_vb: GpuBuffer,
    pub cube_ib: GpuBuffer,
    pub cube_index_count: u32,
    pub floor_vb: GpuBuffer,
    pub floor_ib: GpuBuffer,
    pub floor_index_count: u32,
    pub texture: Texture,
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    pub descriptor_sets: Vec<vk::DescriptorSet>,
    pub uniform_buffers: Vec<GpuBuffer>,
    pub uniform_mapped: Vec<*mut u8>,
}

impl Renderer {
    pub fn new(ctx: &VulkanContext, window_width: u32, window_height: u32) -> Self {
        let swapchain_loader = ash::khr::swapchain::Device::new(&ctx.instance, &ctx.device);

        let depth_format = find_depth_format(&ctx.instance, ctx.physical_device);
        let render_pass = create_render_pass(&ctx.device, vk::Format::B8G8R8A8_SRGB, depth_format);

        let swapchain = create_swapchain(
            &ctx.instance,
            &ctx.device,
            &ctx.surface_loader,
            &swapchain_loader,
            ctx.physical_device,
            ctx.surface,
            window_width,
            window_height,
            render_pass,
        );

        let descriptor_set_layout = create_descriptor_set_layout(&ctx.device);
        let pipeline = create_pipeline(
            &ctx.device,
            render_pass,
            swapchain.extent,
            descriptor_set_layout,
        );

        let command_pool = {
            let create_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.graphics_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            unsafe { ctx.device.create_command_pool(&create_info, None).unwrap() }
        };

        let (cube_verts, cube_indices) = mesh::cube(1.0);
        let (floor_verts, floor_indices) = mesh::floor(20.0, 0.0, 10.0);

        let cube_vb = create_device_local_buffer(
            ctx,
            command_pool,
            &cube_verts,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        );
        let cube_ib = create_device_local_buffer(
            ctx,
            command_pool,
            &cube_indices,
            vk::BufferUsageFlags::INDEX_BUFFER,
        );
        let floor_vb = create_device_local_buffer(
            ctx,
            command_pool,
            &floor_verts,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        );
        let floor_ib = create_device_local_buffer(
            ctx,
            command_pool,
            &floor_indices,
            vk::BufferUsageFlags::INDEX_BUFFER,
        );

        let texture = Texture::from_png(ctx, command_pool, "assets/texture.png");

        // Per-frame UBOs, persistently mapped.
        let mut uniform_buffers = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        let mut uniform_mapped: Vec<*mut u8> = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let buf = create_buffer(
                &ctx.device,
                UBO_SIZE,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                &ctx.instance,
                ctx.physical_device,
            );
            let ptr = unsafe {
                ctx.device
                    .map_memory(buf.memory, 0, UBO_SIZE, vk::MemoryMapFlags::empty())
                    .unwrap()
            } as *mut u8;
            uniform_buffers.push(buf);
            uniform_mapped.push(ptr);
        }

        let descriptor_pool = create_descriptor_pool(&ctx.device, MAX_FRAMES_IN_FLIGHT as u32);
        let descriptor_sets = create_descriptor_sets(
            &ctx.device,
            descriptor_set_layout,
            descriptor_pool,
            &uniform_buffers,
            &texture,
        );

        let command_buffers = {
            let alloc_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(MAX_FRAMES_IN_FLIGHT as u32);
            unsafe { ctx.device.allocate_command_buffers(&alloc_info).unwrap() }
        };

        let mut image_available = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        let mut in_flight = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);

        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            image_available.push(unsafe {
                ctx.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                    .unwrap()
            });
            in_flight.push(unsafe {
                ctx.device
                    .create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                    .unwrap()
            });
        }

        let mut render_finished = Vec::with_capacity(swapchain.images.len());
        for _ in 0..swapchain.images.len() {
            render_finished.push(unsafe {
                ctx.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                    .unwrap()
            });
        }

        let images_in_flight = vec![None; swapchain.images.len()];

        Self {
            device: ctx.device.clone(),
            swapchain,
            pipeline,
            command_pool,
            command_buffers,
            image_available,
            render_finished,
            in_flight,
            images_in_flight,
            current_frame: 0,
            framebuffer_resized: false,
            cube_vb,
            cube_ib,
            cube_index_count: cube_indices.len() as u32,
            floor_vb,
            floor_ib,
            floor_index_count: floor_indices.len() as u32,
            texture,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_sets,
            uniform_buffers,
            uniform_mapped,
        }
    }

    pub fn draw_frame(&mut self, ctx: &VulkanContext, view_proj: Mat4) {
        if self.framebuffer_resized {
            self.recreate_swapchain(ctx);
            self.framebuffer_resized = false;
            return;
        }

        let frame = self.current_frame;
        let fence = self.in_flight[frame];
        let image_available = self.image_available[frame];
        let command_buffer = self.command_buffers[frame];

        unsafe {
            ctx.device
                .wait_for_fences(std::slice::from_ref(&fence), true, u64::MAX)
                .unwrap();
        }

        let image_index = match unsafe {
            self.swapchain.swapchain_loader.acquire_next_image(
                self.swapchain.swapchain,
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        } {
            Ok((index, _suboptimal)) => index,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.recreate_swapchain(ctx);
                return;
            }
            Err(e) => panic!("Failed to acquire next image: {:?}", e),
        };

        if let Some(image_fence) = self.images_in_flight[image_index as usize] {
            unsafe {
                ctx.device
                    .wait_for_fences(std::slice::from_ref(&image_fence), true, u64::MAX)
                    .unwrap();
            }
        }
        self.images_in_flight[image_index as usize] = Some(fence);

        let render_finished = self.render_finished[image_index as usize];

        unsafe {
            ctx.device
                .reset_fences(std::slice::from_ref(&fence))
                .unwrap();
            ctx.device
                .reset_command_buffer(command_buffer, vk::CommandBufferResetFlags::empty())
                .unwrap();
        }

        // Write the per-frame UBO. The fence wait above guarantees this frame's
        // prior GPU work has finished, so this memory isn't being read.
        let mvp_cols = view_proj.to_cols_array();
        unsafe {
            std::ptr::copy_nonoverlapping(
                mvp_cols.as_ptr() as *const u8,
                self.uniform_mapped[frame],
                UBO_SIZE as usize,
            );
        }

        let extent = self.swapchain.extent;
        let framebuffer = self.swapchain.framebuffers[image_index as usize];

        record_command_buffer(
            &ctx.device,
            command_buffer,
            self.pipeline.render_pass,
            framebuffer,
            extent,
            self.pipeline.pipeline,
            self.pipeline.pipeline_layout,
            self.descriptor_sets[frame],
            &self.cube_vb,
            &self.cube_ib,
            self.cube_index_count,
            &self.floor_vb,
            &self.floor_ib,
            self.floor_index_count,
        );

        let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let submit_info = vk::SubmitInfo::default()
            .wait_semaphores(std::slice::from_ref(&image_available))
            .wait_dst_stage_mask(&wait_stages)
            .command_buffers(std::slice::from_ref(&command_buffer))
            .signal_semaphores(std::slice::from_ref(&render_finished));

        unsafe {
            ctx.device
                .queue_submit(
                    ctx.graphics_queue,
                    std::slice::from_ref(&submit_info),
                    fence,
                )
                .unwrap();
        }

        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(std::slice::from_ref(&render_finished))
            .swapchains(std::slice::from_ref(&self.swapchain.swapchain))
            .image_indices(std::slice::from_ref(&image_index));

        let result = unsafe {
            self.swapchain
                .swapchain_loader
                .queue_present(ctx.present_queue, &present_info)
        };

        match result {
            Ok(suboptimal_present) => {
                if suboptimal_present || self.framebuffer_resized {
                    self.framebuffer_resized = false;
                    self.recreate_swapchain(ctx);
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) | Err(vk::Result::SUBOPTIMAL_KHR) => {
                self.recreate_swapchain(ctx);
            }
            Err(e) => panic!("Failed to present: {:?}", e),
        }

        self.current_frame = (self.current_frame + 1) % MAX_FRAMES_IN_FLIGHT;
    }

    fn recreate_swapchain(&mut self, ctx: &VulkanContext) {
        unsafe { ctx.device.device_wait_idle().unwrap() };

        cleanup_swapchain(&ctx.device, &mut self.swapchain);

        // Destroy old per-image render-finished semaphores and recreate.
        unsafe {
            for &s in &self.render_finished {
                self.device.destroy_semaphore(s, None);
            }
        }

        let swapchain_loader = ash::khr::swapchain::Device::new(&ctx.instance, &ctx.device);
        let swapchain = create_swapchain(
            &ctx.instance,
            &ctx.device,
            &ctx.surface_loader,
            &swapchain_loader,
            ctx.physical_device,
            ctx.surface,
            self.swapchain.extent.width,
            self.swapchain.extent.height,
            self.pipeline.render_pass,
        );

        let mut render_finished = Vec::with_capacity(swapchain.images.len());
        for _ in 0..swapchain.images.len() {
            render_finished.push(unsafe {
                self.device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                    .unwrap()
            });
        }
        self.render_finished = render_finished;

        self.images_in_flight = vec![None; swapchain.images.len()];
        self.swapchain = swapchain;
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

            // Texture (sampler, view, image, memory).
            self.texture.destroy(&self.device);

            // Uniform buffers. memory free implicitly unmaps the persistent mapping.
            for ub in &self.uniform_buffers {
                ub.destroy(&self.device);
            }

            // Descriptor pool (frees sets implicitly), then layout.
            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.descriptor_set_layout, None);

            self.cube_vb.destroy(&self.device);
            self.cube_ib.destroy(&self.device);
            self.floor_vb.destroy(&self.device);
            self.floor_ib.destroy(&self.device);
            self.in_flight
                .iter()
                .for_each(|&f| self.device.destroy_fence(f, None));
            self.image_available
                .iter()
                .for_each(|&s| self.device.destroy_semaphore(s, None));
            self.render_finished
                .iter()
                .for_each(|&s| self.device.destroy_semaphore(s, None));
            self.device
                .free_command_buffers(self.command_pool, &self.command_buffers);
            self.device.destroy_command_pool(self.command_pool, None);
            self.device.destroy_pipeline(self.pipeline.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.pipeline.pipeline_layout, None);
            self.device
                .destroy_render_pass(self.pipeline.render_pass, None);
            cleanup_swapchain(&self.device, &mut self.swapchain);
        }
    }
}

fn record_command_buffer(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    extent: vk::Extent2D,
    pipeline: vk::Pipeline,
    pipeline_layout: vk::PipelineLayout,
    descriptor_set: vk::DescriptorSet,
    cube_vb: &GpuBuffer,
    cube_ib: &GpuBuffer,
    cube_index_count: u32,
    floor_vb: &GpuBuffer,
    floor_ib: &GpuBuffer,
    floor_index_count: u32,
) {
    let begin_info = vk::CommandBufferBeginInfo::default();
    unsafe {
        device
            .begin_command_buffer(command_buffer, &begin_info)
            .unwrap();
    }

    let clear_values = [
        vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.02, 0.02, 0.04, 1.0],
            },
        },
        vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        },
    ];

    let render_pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(render_pass)
        .framebuffer(framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        })
        .clear_values(&clear_values);

    let viewport = vk::Viewport::default()
        .x(0.0)
        .y(extent.height as f32)
        .width(extent.width as f32)
        .height(-(extent.height as f32))
        .min_depth(0.0)
        .max_depth(1.0);

    let scissor = vk::Rect2D::default()
        .offset(vk::Offset2D { x: 0, y: 0 })
        .extent(extent);

    unsafe {
        device.cmd_begin_render_pass(
            command_buffer,
            &render_pass_begin,
            vk::SubpassContents::INLINE,
        );
        device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport));
        device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&scissor));
        device.cmd_bind_pipeline(command_buffer, vk::PipelineBindPoint::GRAPHICS, pipeline);

        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            pipeline_layout,
            0,
            std::slice::from_ref(&descriptor_set),
            &[],
        );

        // Cube
        device.cmd_bind_vertex_buffers(
            command_buffer,
            0,
            std::slice::from_ref(&cube_vb.buffer),
            &[0],
        );
        device.cmd_bind_index_buffer(command_buffer, cube_ib.buffer, 0, vk::IndexType::UINT32);
        device.cmd_draw_indexed(command_buffer, cube_index_count, 1, 0, 0, 0);

        // Floor
        device.cmd_bind_vertex_buffers(
            command_buffer,
            0,
            std::slice::from_ref(&floor_vb.buffer),
            &[0],
        );
        device.cmd_bind_index_buffer(command_buffer, floor_ib.buffer, 0, vk::IndexType::UINT32);
        device.cmd_draw_indexed(command_buffer, floor_index_count, 1, 0, 0, 0);

        device.cmd_end_render_pass(command_buffer);
        device.end_command_buffer(command_buffer).unwrap();
    }
}
