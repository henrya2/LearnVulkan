use glam::{Mat4, Vec3};

use crate::scene::gltf_loader::{Scene, load_gltf};
use crate::vulkan::buffer::{GpuBuffer, create_buffer};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::descriptors::{
    create_descriptor_pool, create_global_descriptor_set_layout,
    create_material_descriptor_set_layout,
};
use crate::vulkan::environment_map::create_synthetic_environment_map;
use crate::vulkan::pbr_ubo::{GlobalUniforms, PushConstants};
use crate::vulkan::pipeline::{PipelineData, create_pbr_pipeline, create_render_pass};
use crate::vulkan::swapchain::{
    SwapchainData, cleanup_swapchain, create_swapchain, find_depth_format,
};
use ash::vk;

const MAX_FRAMES_IN_FLIGHT: usize = 2;

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
    pub scene: Scene,
    pub global_uniforms: Vec<GpuBuffer>,
    pub global_mapped: Vec<*mut u8>,
    pub global_descriptor_set_layout: vk::DescriptorSetLayout,
    pub material_descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    pub global_descriptor_sets: Vec<vk::DescriptorSet>,
    pub material_descriptor_sets: Vec<vk::DescriptorSet>,
    pub env_map: crate::vulkan::texture::Texture,
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

        let command_pool = {
            let create_info = vk::CommandPoolCreateInfo::default()
                .queue_family_index(ctx.graphics_family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
            unsafe { ctx.device.create_command_pool(&create_info, None).unwrap() }
        };

        let scene = load_gltf(
            ctx,
            command_pool,
            "assets/models/DamagedHelmet/DamagedHelmet.gltf",
        );

        let global_descriptor_set_layout = create_global_descriptor_set_layout(&ctx.device);
        let material_descriptor_set_layout = create_material_descriptor_set_layout(&ctx.device);

        let push_constant_size = std::mem::size_of::<PushConstants>() as u32;
        let pipeline = create_pbr_pipeline(
            &ctx.device,
            render_pass,
            swapchain.extent,
            global_descriptor_set_layout,
            material_descriptor_set_layout,
            push_constant_size,
        );

        let ubo_size = std::mem::size_of::<GlobalUniforms>() as vk::DeviceSize;
        let mut global_uniforms = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        let mut global_mapped = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let buf = create_buffer(
                &ctx.device,
                ubo_size,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                &ctx.instance,
                ctx.physical_device,
            );
            let ptr = unsafe {
                ctx.device
                    .map_memory(buf.memory, 0, ubo_size, vk::MemoryMapFlags::empty())
                    .unwrap()
            } as *mut u8;
            global_uniforms.push(buf);
            global_mapped.push(ptr);
        }

        let descriptor_pool = create_descriptor_pool(&ctx.device, scene.materials.len() as u32);

        let env_map = create_synthetic_environment_map(ctx, command_pool);

        let global_descriptor_sets = {
            let layouts = vec![global_descriptor_set_layout; MAX_FRAMES_IN_FLIGHT];
            let alloc_info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&layouts);
            unsafe { ctx.device.allocate_descriptor_sets(&alloc_info).unwrap() }
        };

        for i in 0..MAX_FRAMES_IN_FLIGHT {
            let global_buffer_info = vk::DescriptorBufferInfo::default()
                .buffer(global_uniforms[i].buffer)
                .offset(0)
                .range(ubo_size);
            let material_buffer_info = vk::DescriptorBufferInfo::default()
                .buffer(scene.material_buffer.buffer)
                .offset(0)
                .range(scene.material_buffer.size);
            let env_map_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(env_map.view)
                .sampler(env_map.sampler);

            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(0)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(std::slice::from_ref(&global_buffer_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(1)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                    .buffer_info(std::slice::from_ref(&material_buffer_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(2)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&env_map_info)),
            ];
            unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
        }

        let material_descriptor_sets = {
            let layouts = vec![material_descriptor_set_layout; scene.materials.len()];
            let alloc_info = vk::DescriptorSetAllocateInfo::default()
                .descriptor_pool(descriptor_pool)
                .set_layouts(&layouts);
            unsafe { ctx.device.allocate_descriptor_sets(&alloc_info).unwrap() }
        };

        for (mat_idx, material) in scene.materials.iter().enumerate() {
            let base_color = material
                .base_color_texture
                .and_then(|i| scene.textures.get(i))
                .unwrap_or(&scene.fallback_textures.white_srgb);
            let metallic_roughness = material
                .metallic_roughness_texture
                .and_then(|i| scene.textures.get(i))
                .unwrap_or(&scene.fallback_textures.metallic_roughness_linear);
            let normal = material
                .normal_texture
                .and_then(|i| scene.textures.get(i))
                .unwrap_or(&scene.fallback_textures.normal_linear);
            let occlusion = material
                .occlusion_texture
                .and_then(|i| scene.textures.get(i))
                .unwrap_or(&scene.fallback_textures.white_linear);
            let emissive = material
                .emissive_texture
                .and_then(|i| scene.textures.get(i))
                .unwrap_or(&scene.fallback_textures.black_srgb);

            let image_infos = [
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(base_color.view)
                    .sampler(base_color.sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(metallic_roughness.view)
                    .sampler(metallic_roughness.sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(normal.view)
                    .sampler(normal.sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(occlusion.view)
                    .sampler(occlusion.sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(emissive.view)
                    .sampler(emissive.sampler),
            ];

            let writes: Vec<_> = (0..5)
                .map(|binding| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(material_descriptor_sets[mat_idx])
                        .dst_binding(binding as u32)
                        .dst_array_element(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(std::slice::from_ref(&image_infos[binding]))
                })
                .collect();

            unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
        }

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
            scene,
            global_uniforms,
            global_mapped,
            global_descriptor_set_layout,
            material_descriptor_set_layout,
            descriptor_pool,
            global_descriptor_sets,
            material_descriptor_sets,
            env_map,
        }
    }

    pub fn draw_frame(&mut self, ctx: &VulkanContext, view: Mat4, proj: Mat4, camera_pos: Vec3) {
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

        let light_dir = glam::Vec3::new(-0.5, -1.0, 0.5).normalize();
        let globals = GlobalUniforms {
            view: view.to_cols_array(),
            proj: proj.to_cols_array(),
            camera_pos: [camera_pos.x, camera_pos.y, camera_pos.z],
            _pad0: 0.0,
            light_dir: [light_dir.x, light_dir.y, light_dir.z],
            light_intensity: 4.0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&globals).as_ptr(),
                self.global_mapped[frame],
                std::mem::size_of::<GlobalUniforms>(),
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
            self.global_descriptor_sets[frame],
            &self.material_descriptor_sets,
            &self.scene,
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

            self.scene.destroy(&self.device);

            self.env_map.destroy(&self.device);

            for ub in &self.global_uniforms {
                ub.destroy(&self.device);
            }

            self.device
                .destroy_descriptor_pool(self.descriptor_pool, None);
            self.device
                .destroy_descriptor_set_layout(self.global_descriptor_set_layout, None);
            self.device
                .destroy_descriptor_set_layout(self.material_descriptor_set_layout, None);

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
    global_descriptor_set: vk::DescriptorSet,
    material_descriptor_sets: &[vk::DescriptorSet],
    scene: &Scene,
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
                float32: [0.15, 0.15, 0.17, 1.0],
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
            std::slice::from_ref(&global_descriptor_set),
            &[],
        );

        for mesh in &scene.meshes {
            let pc = PushConstants {
                model: mesh.world_matrix.to_cols_array(),
                material_index: mesh.material_index as u32,
                _pad: [0; 3],
            };
            let pc_bytes = bytemuck::bytes_of(&pc);

            device.cmd_push_constants(
                command_buffer,
                pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                pc_bytes,
            );

            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline_layout,
                1,
                std::slice::from_ref(&material_descriptor_sets[mesh.material_index]),
                &[],
            );

            device.cmd_bind_vertex_buffers(
                command_buffer,
                0,
                std::slice::from_ref(&mesh.vertex_buffer.buffer),
                &[0],
            );
            device.cmd_bind_index_buffer(
                command_buffer,
                mesh.index_buffer.buffer,
                0,
                vk::IndexType::UINT32,
            );
            device.cmd_draw_indexed(command_buffer, mesh.index_count, 1, 0, 0, 0);
        }

        device.cmd_end_render_pass(command_buffer);
        device.end_command_buffer(command_buffer).unwrap();
    }
}
