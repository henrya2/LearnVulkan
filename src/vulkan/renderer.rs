#![allow(unsafe_op_in_unsafe_fn)]

use glam::{Mat4, Vec3};

use crate::scene::gltf_loader::{Scene, load_gltf};
use crate::vulkan::buffer::{GpuBuffer, create_buffer, create_device_local_buffer};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::debug_marker::DebugMarker;
use crate::vulkan::descriptors::{
    create_descriptor_pool, create_global_descriptor_set_layout,
    create_material_descriptor_set_layout,
};
use crate::vulkan::ibl::IblResources;
use crate::vulkan::pbr_ubo::{GLOBAL_UBO_BLOCK_SIZE, GlobalUniforms, PushConstants};
use crate::vulkan::pipeline::{
    PipelineData, create_pbr_pipeline, create_skybox_pipeline,
};
use crate::vulkan::postprocess::{
    BloomPyramid, PostProcessResources, TonemapOp,
    pass_trait::set_viewport_and_bind_pipeline,
    passes::create_composite_render_pass,
};
use crate::vulkan::swapchain::{
    SwapchainData, cleanup_swapchain, create_swapchain, find_depth_format, select_surface_format,
};
use ash::vk;

const MAX_FRAMES_IN_FLIGHT: usize = 2;
const FRAME_LABEL_COLOR: [f32; 4] = [0.3, 0.3, 0.3, 1.0];
const RENDER_PASS_LABEL_COLOR: [f32; 4] = [0.2, 0.8, 0.2, 1.0];
const DRAW_LABEL_COLOR: [f32; 4] = [0.3, 0.5, 1.0, 1.0];
const SETUP_LABEL_COLOR: [f32; 4] = [0.8, 0.7, 0.2, 1.0];
const SKYBOX_LABEL_COLOR: [f32; 4] = [0.4, 0.7, 0.9, 1.0];
const BLUR_LABEL_COLOR: [f32; 4] = [0.6, 0.3, 0.9, 1.0];
const POSTPROCESS_LABEL_COLOR: [f32; 4] = [0.8, 0.4, 0.6, 1.0];

const ENV_BASE_PATH: &str = "assets/environment_map/ennis";

pub struct Renderer {
    pub device: ash::Device,
    pub swapchain: SwapchainData,
    pub pipeline: PipelineData,
    pub skybox_pipeline: PipelineData,
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
    pub ibl: IblResources,
    pub skybox_vertex_buffer: GpuBuffer,
    pub skybox_index_buffer: GpuBuffer,
    pub skybox_index_count: u32,
    /// Postprocess chain (scene color, bloom, composite).
    pub postprocess: Option<PostProcessResources>,
    /// Current tonemap operator. Tracked here (not only in the UBO) so it
    /// survives swapchain recreation, which rebuilds `PostProcessResources`
    /// with a default tonemap and would otherwise silently reset the user's
    /// selection.
    pub current_tonemap: TonemapOp,
    /// Composite render pass (color-only, PRESENT_SRC_KHR). Owned here so we
    /// can recreate the swapchain (and pass it to `create_swapchain`).
    pub composite_render_pass: vk::RenderPass,
}

impl Renderer {
    pub fn new(ctx: &VulkanContext, window_width: u32, window_height: u32) -> Self {
        let swapchain_loader = ash::khr::swapchain::Device::new(&ctx.instance, &ctx.device);

        let surface_format =
            select_surface_format(&ctx.surface_loader, ctx.physical_device, ctx.surface);
        let depth_format = find_depth_format(&ctx.instance, ctx.physical_device);
        // The composite pass writes the final image to the swapchain via this
        // color-only render pass.
        let composite_render_pass =
            create_composite_render_pass(&ctx.device, surface_format.format);

        let swapchain = create_swapchain(
            &ctx.instance,
            &ctx.device,
            &ctx.surface_loader,
            &swapchain_loader,
            ctx.physical_device,
            ctx.surface,
            window_width,
            window_height,
            composite_render_pass,
            surface_format,
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

        let ibl = IblResources::load(ctx, command_pool, ENV_BASE_PATH);

        let global_descriptor_set_layout = create_global_descriptor_set_layout(&ctx.device);
        let material_descriptor_set_layout = create_material_descriptor_set_layout(&ctx.device);

        // Create postprocess resources first, so we have the scene render pass
        // to bind the PBR and skybox pipelines against. This avoids creating
        // a second (unusable) render pass and ensures the pipelines and the
        // recorded render pass are compatible.
        let postprocess = PostProcessResources::new(
            ctx,
            command_pool,
            depth_format,
            surface_format.format,
            swapchain.extent,
            &swapchain.image_views,
            swapchain.depth_view,
            MAX_FRAMES_IN_FLIGHT,
            composite_render_pass,
        );
        let scene_render_pass = postprocess.scene_render_pass;

        let push_constant_size = std::mem::size_of::<PushConstants>() as u32;
        let pipeline = create_pbr_pipeline(
            &ctx.device,
            scene_render_pass,
            global_descriptor_set_layout,
            material_descriptor_set_layout,
            push_constant_size,
        );

        let skybox_pipeline = create_skybox_pipeline(
            &ctx.device,
            scene_render_pass,
            global_descriptor_set_layout,
        );

        let ubo_size = std::mem::size_of::<GlobalUniforms>() as vk::DeviceSize;
        // The UBO must be at least GLOBAL_UBO_BLOCK_SIZE bytes. With the
        // `Vec4`-based struct, `size_of::<GlobalUniforms>()` is already
        // 176 (a multiple of 16), so the std140 block size equals the
        // struct size and `max` is a defensive guard in case a future
        // field layout changes that.
        let alloc_size = GLOBAL_UBO_BLOCK_SIZE.max(ubo_size);
        let mut global_uniforms = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        let mut global_mapped = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
        for _ in 0..MAX_FRAMES_IN_FLIGHT {
            let buf = create_buffer(
                &ctx.device,
                alloc_size,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                &ctx.instance,
                ctx.physical_device,
            );
            let ptr = unsafe {
                ctx.device
                    .map_memory(buf.memory, 0, alloc_size, vk::MemoryMapFlags::empty())
                    .unwrap()
            } as *mut u8;
            global_uniforms.push(buf);
            global_mapped.push(ptr);
        }

        let descriptor_pool = create_descriptor_pool(&ctx.device, scene.materials.len() as u32);

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
                .range(GLOBAL_UBO_BLOCK_SIZE);
            let material_buffer_info = vk::DescriptorBufferInfo::default()
                .buffer(scene.material_buffer.buffer)
                .offset(0)
                .range(scene.material_buffer.size);
            let irradiance_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(ibl.irradiance_map.view)
                .sampler(ibl.irradiance_map.sampler);
            let prefilter_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(ibl.prefilter_map.view)
                .sampler(ibl.prefilter_map.sampler);
            let brdf_lut_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(ibl.brdf_lut.view)
                .sampler(ibl.brdf_lut.sampler);
            let env_cubemap_info = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(ibl.env_cubemap.view)
                .sampler(ibl.env_cubemap.sampler);

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
                    .image_info(std::slice::from_ref(&irradiance_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(3)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&prefilter_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(4)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&brdf_lut_info)),
                vk::WriteDescriptorSet::default()
                    .dst_set(global_descriptor_sets[i])
                    .dst_binding(5)
                    .dst_array_element(0)
                    .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                    .image_info(std::slice::from_ref(&env_cubemap_info)),
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

        // Skybox cube geometry (positions only)
        let skybox_vertices: [[f32; 3]; 8] = [
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        // CW-from-outside in LH Y-up model space (consistent with code-defined geometry rule)
        let skybox_indices: [u32; 36] = [
            // Front (+Z): CW from outside
            4, 6, 5, 4, 7, 6, // Back (-Z): CW from outside
            1, 3, 0, 1, 2, 3, // Top (+Y): CW from outside
            3, 6, 7, 3, 2, 6, // Bottom (-Y): CW from outside
            0, 5, 1, 0, 4, 5, // Right (+X): CW from outside
            1, 6, 2, 1, 5, 6, // Left (-X): CW from outside
            0, 7, 4, 0, 3, 7,
        ];
        let skybox_index_count = skybox_indices.len() as u32;
        let skybox_vertex_buffer = create_device_local_buffer(
            ctx,
            command_pool,
            &skybox_vertices,
            vk::BufferUsageFlags::VERTEX_BUFFER,
        );
        let skybox_index_buffer = create_device_local_buffer(
            ctx,
            command_pool,
            &skybox_indices,
            vk::BufferUsageFlags::INDEX_BUFFER,
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

        let renderer = Self {
            device: ctx.device.clone(),
            swapchain,
            pipeline,
            skybox_pipeline,
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
            ibl,
            skybox_vertex_buffer,
            skybox_index_buffer,
            skybox_index_count,
            postprocess: Some(postprocess),
            current_tonemap: TonemapOp::Aces, // matches PostProcessSettings::default
            composite_render_pass,
        };

        renderer.name_debug_objects(ctx);
        renderer
    }

    /// Set the active tonemap operator. Writes the numeric value to the
    /// postprocess UBO settings; the next `draw_frame` call will memcpy the
    /// UBO to the GPU. Safe to call any time — the UBO is `HOST_VISIBLE |
    /// HOST_COHERENT` and is rewritten every frame.
    ///
    /// The state is also stored in `self.current_tonemap` so it survives
    /// swapchain recreation (which rebuilds `PostProcessResources` with
    /// `PostProcessSettings::default()` and would otherwise reset the
    /// selection).
    ///
    /// If postprocess resources have not been initialized yet (impossible
    /// under the current `App` flow but kept defensive), only the
    /// `current_tonemap` field is updated.
    pub fn set_tonemap(&mut self, op: TonemapOp) {
        self.current_tonemap = op;
        if let Some(pp) = self.postprocess.as_mut() {
            pp.settings.ubo.tonemap_op = op.as_u32();
        }
    }

    fn name_debug_objects(&self, ctx: &VulkanContext) {
        let dm = &ctx.debug_marker;

        unsafe {
            // The scene render pass itself is owned and named by
            // PostProcessResources (see `name_debug_objects` below).
            dm.set_object_name(self.pipeline.pipeline_layout, "PBR Pipeline Layout");
            dm.set_object_name(self.pipeline.pipeline, "PBR Graphics Pipeline");
            dm.set_object_name(
                self.skybox_pipeline.pipeline_layout,
                "Skybox Pipeline Layout",
            );
            dm.set_object_name(self.skybox_pipeline.pipeline, "Skybox Graphics Pipeline");
            dm.set_object_name(self.command_pool, "Main Graphics Command Pool");
            dm.set_object_name(
                self.global_descriptor_set_layout,
                "Global Descriptor Set Layout",
            );
            dm.set_object_name(
                self.material_descriptor_set_layout,
                "Material Descriptor Set Layout",
            );
            dm.set_object_name(self.descriptor_pool, "Renderer Descriptor Pool");

            for (i, &cmd) in self.command_buffers.iter().enumerate() {
                dm.set_object_name(cmd, &format!("Frame Command Buffer {}", i));
            }
            for (i, buffer) in self.global_uniforms.iter().enumerate() {
                dm.set_object_name(buffer.buffer, &format!("Global Uniform Buffer Frame {}", i));
                dm.set_object_name(buffer.memory, &format!("Global Uniform Memory Frame {}", i));
            }
            for (i, &set) in self.global_descriptor_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("Global Descriptor Set Frame {}", i));
            }
            for (i, &set) in self.material_descriptor_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("Material Descriptor Set {}", i));
            }
            for (i, &semaphore) in self.image_available.iter().enumerate() {
                dm.set_object_name(semaphore, &format!("Image Available Semaphore Frame {}", i));
            }
            for (i, &semaphore) in self.render_finished.iter().enumerate() {
                dm.set_object_name(
                    semaphore,
                    &format!("Render Finished Semaphore Swapchain Image {}", i),
                );
            }
            for (i, &fence) in self.in_flight.iter().enumerate() {
                dm.set_object_name(fence, &format!("In Flight Fence Frame {}", i));
            }

            dm.set_object_name(self.scene.material_buffer.buffer, "Material Uniform Buffer");
            dm.set_object_name(self.scene.material_buffer.memory, "Material Uniform Memory");
            for (i, mesh) in self.scene.meshes.iter().enumerate() {
                dm.set_object_name(
                    mesh.vertex_buffer.buffer,
                    &format!("Mesh {} Vertex Buffer", i),
                );
                dm.set_object_name(
                    mesh.index_buffer.buffer,
                    &format!("Mesh {} Index Buffer", i),
                );
            }
            for (i, texture) in self.scene.textures.iter().enumerate() {
                name_texture(dm, texture, &format!("Scene Texture {}", i));
            }
            name_texture(
                dm,
                &self.scene.fallback_textures.white_srgb,
                "Fallback White sRGB Texture",
            );
            name_texture(
                dm,
                &self.scene.fallback_textures.white_linear,
                "Fallback White Linear Texture",
            );
            name_texture(
                dm,
                &self.scene.fallback_textures.black_srgb,
                "Fallback Black sRGB Texture",
            );
            name_texture(
                dm,
                &self.scene.fallback_textures.normal_linear,
                "Fallback Normal Linear Texture",
            );
            name_texture(
                dm,
                &self.scene.fallback_textures.metallic_roughness_linear,
                "Fallback Metallic-Roughness Linear Texture",
            );

            // IBL resources
            dm.set_object_name(
                self.ibl.env_cubemap.image,
                "Ennis Environment Cubemap Image",
            );
            dm.set_object_name(self.ibl.env_cubemap.view, "Ennis Environment Cubemap View");
            dm.set_object_name(
                self.ibl.env_cubemap.sampler,
                "Ennis Environment Cubemap Sampler",
            );
            dm.set_object_name(
                self.ibl.irradiance_map.image,
                "Ennis Irradiance Cubemap Image",
            );
            dm.set_object_name(
                self.ibl.irradiance_map.view,
                "Ennis Irradiance Cubemap View",
            );
            dm.set_object_name(
                self.ibl.irradiance_map.sampler,
                "Ennis Irradiance Cubemap Sampler",
            );
            dm.set_object_name(
                self.ibl.prefilter_map.image,
                "Ennis Prefilter Cubemap Image",
            );
            dm.set_object_name(self.ibl.prefilter_map.view, "Ennis Prefilter Cubemap View");
            dm.set_object_name(
                self.ibl.prefilter_map.sampler,
                "Ennis Prefilter Cubemap Sampler",
            );
            dm.set_object_name(self.ibl.brdf_lut.image, "BRDF LUT Image");
            dm.set_object_name(self.ibl.brdf_lut.view, "BRDF LUT View");
            dm.set_object_name(self.ibl.brdf_lut.sampler, "BRDF LUT Sampler");

            // Skybox
            dm.set_object_name(self.skybox_vertex_buffer.buffer, "Skybox Vertex Buffer");
            dm.set_object_name(self.skybox_index_buffer.buffer, "Skybox Index Buffer");

            name_swapchain_objects(dm, &self.swapchain);

            if let Some(ref pp) = self.postprocess {
                pp.name_debug_objects(dm);
            }
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
                // When acquire fails with OUT_OF_DATE, the semaphore is left
                // in an indeterminate state per spec. Destroy and recreate it.
                unsafe {
                    ctx.device.destroy_semaphore(image_available, None);
                }
                self.image_available[frame] = unsafe {
                    ctx.device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        .unwrap()
                };
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
        let prefilter_max_lod = (self.ibl.prefilter_map.mip_levels.saturating_sub(1)) as f32;
        let globals = GlobalUniforms {
            view: view,
            proj: proj,
            camera_pos: glam::Vec4::new(camera_pos.x, camera_pos.y, camera_pos.z, 1.0),
            light_dir: glam::Vec4::new(light_dir.x, light_dir.y, light_dir.z, 0.0),
            light_intensity: 4.0,
            prefilter_max_lod,
            _pad_tail: [0.0, 0.0],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&globals).as_ptr(),
                self.global_mapped[frame],
                std::mem::size_of::<GlobalUniforms>(),
            );
        }

        let extent = self.swapchain.extent;
        let postprocess = self.postprocess.as_ref().expect("postprocess must exist");
        let scene_framebuffer = postprocess.scene_framebuffers[image_index as usize];
        let composite_framebuffer = self.swapchain.framebuffers[image_index as usize];

        // Update postprocess UBO for this frame.
        postprocess.update_ubo(frame);

        record_command_buffer(
            &ctx.device,
            &ctx.debug_marker,
            command_buffer,
            frame,
            image_index,
            scene_framebuffer,
            composite_framebuffer,
            extent,
            postprocess,
            self.pipeline.pipeline,
            self.pipeline.pipeline_layout,
            self.skybox_pipeline.pipeline,
            self.skybox_pipeline.pipeline_layout,
            self.global_descriptor_sets[frame],
            &self.material_descriptor_sets,
            &self.scene,
            self.skybox_vertex_buffer.buffer,
            self.skybox_index_buffer.buffer,
            self.skybox_index_count,
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
        let surface_format = vk::SurfaceFormatKHR {
            format: self.swapchain.image_format,
            color_space: self.swapchain.image_color_space,
        };
        let swapchain = create_swapchain(
            &ctx.instance,
            &ctx.device,
            &ctx.surface_loader,
            &swapchain_loader,
            ctx.physical_device,
            ctx.surface,
            self.swapchain.extent.width,
            self.swapchain.extent.height,
            self.composite_render_pass,
            surface_format,
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

        // Recreate postprocess resources: scene color images, bloom pyramid,
        // and the descriptor sets that point at them. The UBO sets, pipeline
        // layouts, render passes, and pipeline objects are reused.
        let depth_format = self.swapchain.depth_format;
        // Take the old postprocess out so we can destroy it explicitly.
        let old_pp = self.postprocess.take();
        if let Some(mut old) = old_pp {
            unsafe { old.destroy(&self.device); }
        }
        self.postprocess = Some(PostProcessResources::new(
            ctx,
            self.command_pool,
            depth_format,
            self.swapchain.image_format,
            swapchain.extent,
            &swapchain.image_views,
            swapchain.depth_view,
            MAX_FRAMES_IN_FLIGHT,
            self.composite_render_pass,
        ));

        self.swapchain = swapchain;

        // Re-apply the user's selected tonemap. The freshly constructed
        // `PostProcessResources` has `TonemapOp::Aces` (the default) — we
        // must overwrite that with the value the user picked so a window
        // resize doesn't silently switch them back to ACES.
        if let Some(pp) = self.postprocess.as_mut() {
            pp.settings.ubo.tonemap_op = self.current_tonemap.as_u32();
        }

        { let dm = &ctx.debug_marker;
            unsafe {
                name_swapchain_objects(dm, &self.swapchain);
                for (i, &semaphore) in self.render_finished.iter().enumerate() {
                    dm.set_object_name(
                        semaphore,
                        &format!("Render Finished Semaphore Swapchain Image {}", i),
                    );
                }
            }
        }
    }
}

unsafe fn name_texture(dm: &DebugMarker, texture: &crate::vulkan::texture::Texture, name: &str) {
    unsafe {
        dm.set_object_name(texture.image, &format!("{} Image", name));
        dm.set_object_name(texture.memory, &format!("{} Memory", name));
        dm.set_object_name(texture.view, &format!("{} View", name));
        dm.set_object_name(texture.sampler, &format!("{} Sampler", name));
    }
}

unsafe fn name_swapchain_objects(dm: &DebugMarker, swapchain: &SwapchainData) {
    unsafe {
        dm.set_object_name(swapchain.swapchain, "Main Swapchain");
        dm.set_object_name(swapchain.depth_image, "Swapchain Depth Image");
        dm.set_object_name(swapchain.depth_memory, "Swapchain Depth Memory");
        dm.set_object_name(swapchain.depth_view, "Swapchain Depth View");
        for (i, &image) in swapchain.images.iter().enumerate() {
            dm.set_object_name(image, &format!("Swapchain Image {}", i));
        }
        for (i, &view) in swapchain.image_views.iter().enumerate() {
            dm.set_object_name(view, &format!("Swapchain Image View {}", i));
        }
        for (i, &framebuffer) in swapchain.framebuffers.iter().enumerate() {
            dm.set_object_name(framebuffer, &format!("Swapchain Framebuffer {}", i));
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();

            self.scene.destroy(&self.device);

            self.ibl.destroy(&self.device);

            self.skybox_vertex_buffer.destroy(&self.device);
            self.skybox_index_buffer.destroy(&self.device);

            self.device
                .destroy_pipeline(self.skybox_pipeline.pipeline, None);
            self.device
                .destroy_pipeline_layout(self.skybox_pipeline.pipeline_layout, None);

            for (ub, mapped) in self
                .global_uniforms
                .iter()
                .zip(self.global_mapped.iter_mut())
            {
                self.device.unmap_memory(ub.memory);
                *mapped = std::ptr::null_mut();
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
            // The scene render pass that the PBR + skybox pipelines were
            // created against is owned by PostProcessResources and is
            // destroyed by `pp.destroy()` below.

            // Destroy the postprocess resources (descriptor pool, pipelines,
            // scene color images, bloom pyramid, framebuffers, render passes).
            // They must be destroyed before the device is destroyed, but the
            // order with respect to the main descriptor pool / pipelines is
            // independent (we already waited for the device idle at the top
            // of `drop`).
            if let Some(mut pp) = self.postprocess.take() {
                pp.destroy(&self.device);
            }
            self.device
                .destroy_render_pass(self.composite_render_pass, None);

            cleanup_swapchain(&self.device, &mut self.swapchain);
        }
    }
}

fn record_command_buffer(
    device: &ash::Device,
    debug_marker: &DebugMarker,
    command_buffer: vk::CommandBuffer,
    frame: usize,
    image_index: u32,
    scene_framebuffer: vk::Framebuffer,
    composite_framebuffer: vk::Framebuffer,
    extent: vk::Extent2D,
    postprocess: &PostProcessResources,
    pbr_pipeline: vk::Pipeline,
    pbr_pipeline_layout: vk::PipelineLayout,
    skybox_pipeline: vk::Pipeline,
    skybox_pipeline_layout: vk::PipelineLayout,
    global_descriptor_set: vk::DescriptorSet,
    material_descriptor_sets: &[vk::DescriptorSet],
    scene: &Scene,
    skybox_vertex_buffer: vk::Buffer,
    skybox_index_buffer: vk::Buffer,
    skybox_index_count: u32,
) {
    let begin_info = vk::CommandBufferBeginInfo::default();
    unsafe {
        device
            .begin_command_buffer(command_buffer, &begin_info)
            .unwrap();
        { let dm = debug_marker;
            dm.begin_label(
                command_buffer,
                &format!("Frame {} / Swapchain Image {}", frame, image_index),
                FRAME_LABEL_COLOR,
            );
        }
    }

    // The negative-height viewport is used for every render pass. This is
    // required by the project's winding contract (see
    // docs/winding_orientation.md) for the PBR and skybox pipelines, and is
    // also fine for the postprocess fullscreen-triangle passes (whose
    // pipelines use cull_mode = NONE).
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

    let scene_clear_values = [
        vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 1.0],
            },
        },
        vk::ClearValue {
            depth_stencil: vk::ClearDepthStencilValue {
                depth: 1.0,
                stencil: 0,
            },
        },
    ];

    let scene_pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(postprocess.scene_render_pass)
        .framebuffer(scene_framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        })
        .clear_values(&scene_clear_values);

    unsafe {
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "Scene Pass", RENDER_PASS_LABEL_COLOR);
        }
        device.cmd_begin_render_pass(
            command_buffer,
            &scene_pass_begin,
            vk::SubpassContents::INLINE,
        );
        { let dm = debug_marker;
            dm.insert_label(
                command_buffer,
                "Set Dynamic Viewport/Scissor",
                SETUP_LABEL_COLOR,
            );
        }
        device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport));
        device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&scissor));

        // ---- Draw skybox first ----
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "Draw Skybox", SKYBOX_LABEL_COLOR);
        }
        device.cmd_bind_pipeline(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            skybox_pipeline,
        );
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            skybox_pipeline_layout,
            0,
            std::slice::from_ref(&global_descriptor_set),
            &[],
        );
        device.cmd_bind_vertex_buffers(
            command_buffer,
            0,
            std::slice::from_ref(&skybox_vertex_buffer),
            &[0],
        );
        device.cmd_bind_index_buffer(
            command_buffer,
            skybox_index_buffer,
            0,
            vk::IndexType::UINT32,
        );
        device.cmd_draw_indexed(command_buffer, skybox_index_count, 1, 0, 0, 0);
        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }

        // ---- Draw PBR geometry ----
        { let dm = debug_marker;
            dm.insert_label(command_buffer, "Bind PBR Pipeline", SETUP_LABEL_COLOR);
        }
        device.cmd_bind_pipeline(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            pbr_pipeline,
        );

        { let dm = debug_marker;
            dm.insert_label(command_buffer, "Bind Global Descriptors", SETUP_LABEL_COLOR);
        }
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            pbr_pipeline_layout,
            0,
            std::slice::from_ref(&global_descriptor_set),
            &[],
        );

        for (mesh_index, mesh) in scene.meshes.iter().enumerate() {
            { let dm = debug_marker;
                dm.begin_label(
                    command_buffer,
                    &format!(
                        "Draw Mesh {} | Material {} | {} indices",
                        mesh_index, mesh.material_index, mesh.index_count
                    ),
                    DRAW_LABEL_COLOR,
                );
            }

            let pc = PushConstants {
                model: mesh.world_matrix,
                material_index: mesh.material_index as u32,
                _pad: [0; 3],
            };
            let pc_bytes = bytemuck::bytes_of(&pc);

            { let dm = debug_marker;
                dm.insert_label(
                    command_buffer,
                    "Push Constants: model matrix + material index",
                    SETUP_LABEL_COLOR,
                );
            }
            device.cmd_push_constants(
                command_buffer,
                pbr_pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                pc_bytes,
            );

            { let dm = debug_marker;
                dm.insert_label(
                    command_buffer,
                    "Bind Material Descriptor Set",
                    SETUP_LABEL_COLOR,
                );
            }
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                pbr_pipeline_layout,
                1,
                std::slice::from_ref(&material_descriptor_sets[mesh.material_index]),
                &[],
            );

            { let dm = debug_marker;
                dm.insert_label(
                    command_buffer,
                    "Bind Vertex/Index Buffers",
                    SETUP_LABEL_COLOR,
                );
            }
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

            { let dm = debug_marker;
                dm.end_label(command_buffer);
            }
        }

        device.cmd_end_render_pass(command_buffer);
        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }
    }

    // ---- Postprocess passes ----
    // The scene color attachment was transitioned to SHADER_READ_ONLY_OPTIMAL
    // at the end of the scene render pass (because the scene render pass
    // declared that final_layout), so we can sample it directly.
    unsafe {
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "PostProcessing", POSTPROCESS_LABEL_COLOR);
        }

        // Bloom Prefilter: extract highlights (soft-threshold) from scene color
        // and write to bloom mip 0.
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "Bloom Prefilter", RENDER_PASS_LABEL_COLOR);
        }
        record_bright_pass(
            device,
            debug_marker,
            command_buffer,
            postprocess,
            image_index as usize,
            frame,
            extent,
        );
        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }

        // Blur passes: per mip, horizontal then vertical (ping-pong).
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "Bloom Pyramid", RENDER_PASS_LABEL_COLOR);
        }
        record_blur_passes(
            device,
            debug_marker,
            command_buffer,
            postprocess,
            frame,
            extent,
        );
        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }

        // Composite: scene + 8 bloom mips + exposure + tonemap -> swapchain.
        { let dm = debug_marker;
            dm.begin_label(command_buffer, "Composite Pass", RENDER_PASS_LABEL_COLOR);
        }
        record_composite_pass(
            device,
            debug_marker,
            command_buffer,
            postprocess,
            image_index as usize,
            frame,
            composite_framebuffer,
            extent,
        );
        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }

        { let dm = debug_marker;
            dm.end_label(command_buffer);
        }
    }

    { let dm = debug_marker;
        unsafe { dm.end_label(command_buffer); }
    }
    unsafe {
        device.end_command_buffer(command_buffer).unwrap();
    }
}

/// Record the Bloom Prefilter pass. Extracts HDR highlights via soft
/// threshold and writes the result to bloom mip 0.
unsafe fn record_bright_pass(
    device: &ash::Device,
    _debug_marker: &DebugMarker,
    command_buffer: vk::CommandBuffer,
    postprocess: &PostProcessResources,
    image_index: usize,
    frame: usize,
    _extent: vk::Extent2D,
) {
    let (mip_w, mip_h) = BloomPyramid::mip_extent(postprocess.bloom_extent.0, postprocess.bloom_extent.1, 0);
    let pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(postprocess.postprocess_color_pass)
        .framebuffer(postprocess.bright_mip0_framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: mip_w,
                height: mip_h,
            },
        });

    let bright_pipeline = postprocess.bright_pipeline.as_ref().unwrap();

    unsafe {
        device.cmd_begin_render_pass(command_buffer, &pass_begin, vk::SubpassContents::INLINE);
        set_viewport_and_bind_pipeline(
            device,
            command_buffer,
            vk::Extent2D {
                width: mip_w,
                height: mip_h,
            },
            bright_pipeline.pipeline,
        );
        // set 0 = scene color sampler; set 1 = postprocess UBO
        let sets: [vk::DescriptorSet; 2] = [
            postprocess.bright_input_sets[image_index],
            postprocess.ubo_sets[frame],
        ];
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            bright_pipeline.pipeline_layout,
            0,
            &sets,
            &[],
        );
        device.cmd_draw(command_buffer, 3, 1, 0, 0);
        device.cmd_end_render_pass(command_buffer);
    }
}

/// Record horizontal+vertical Gaussian blurs for each bloom mip, ping-ponging
/// between the mip image and the temp image. Framebuffers are pre-allocated.
unsafe fn record_blur_passes(
    device: &ash::Device,
    debug_marker: &DebugMarker,
    command_buffer: vk::CommandBuffer,
    postprocess: &PostProcessResources,
    frame: usize,
    _extent: vk::Extent2D,
) {
    let blur_pipeline = postprocess.blur_pipeline.as_ref().unwrap();

    for level in 0..BloomPyramid::mip_count() {
        let (mip_w, mip_h) = BloomPyramid::mip_extent(
            postprocess.bloom_extent.0,
            postprocess.bloom_extent.1,
            level,
        );
        let mip_extent = vk::Extent2D {
            width: mip_w,
            height: mip_h,
        };
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: mip_extent,
        };

        // ---- Horizontal: read from mip[level], write to temp[level]. ----
        // Use blur_input_sets[2*level] which is pre-bound to mip[level].
        { let dm = debug_marker;
            dm.insert_label(
                command_buffer,
                &format!("Bloom Mip {} Horizontal Blur", level),
                BLUR_LABEL_COLOR,
            );
        }
        let pass_begin = vk::RenderPassBeginInfo::default()
            .render_pass(postprocess.postprocess_color_pass)
            .framebuffer(postprocess.blur_temp_framebuffers[level])
            .render_area(render_area);
        unsafe {
            device.cmd_begin_render_pass(command_buffer, &pass_begin, vk::SubpassContents::INLINE);
            set_viewport_and_bind_pipeline(
                device,
                command_buffer,
                mip_extent,
                blur_pipeline.pipeline,
            );
            let sets: [vk::DescriptorSet; 2] = [
                postprocess.blur_input_sets[2 * level],
                postprocess.ubo_sets[frame],
            ];
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                blur_pipeline.pipeline_layout,
                0,
                &sets,
                &[],
            );
            let pc = crate::vulkan::postprocess::BlurPushConstants {
                texel_size: [1.0 / mip_w as f32, 1.0 / mip_h as f32],
                direction: 0,
            };
            let pc_bytes = bytemuck::bytes_of(&pc);
            device.cmd_push_constants(
                command_buffer,
                blur_pipeline.pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                pc_bytes,
            );
            device.cmd_draw(command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(command_buffer);
        }

        // ---- Vertical: read from temp[level], write to mip[level]. ----
        // Use blur_input_sets[2*level+1] which is pre-bound to temp[level].
        { let dm = debug_marker;
            dm.insert_label(
                command_buffer,
                &format!("Bloom Mip {} Vertical Blur", level),
                BLUR_LABEL_COLOR,
            );
        }
        let pass_begin2 = vk::RenderPassBeginInfo::default()
            .render_pass(postprocess.postprocess_color_pass)
            .framebuffer(postprocess.blur_mip_framebuffers[level])
            .render_area(render_area);
        unsafe {
            device.cmd_begin_render_pass(command_buffer, &pass_begin2, vk::SubpassContents::INLINE);
            set_viewport_and_bind_pipeline(
                device,
                command_buffer,
                mip_extent,
                blur_pipeline.pipeline,
            );
            let sets2: [vk::DescriptorSet; 2] = [
                postprocess.blur_input_sets[2 * level + 1],
                postprocess.ubo_sets[frame],
            ];
            device.cmd_bind_descriptor_sets(
                command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                blur_pipeline.pipeline_layout,
                0,
                &sets2,
                &[],
            );
            let pc2 = crate::vulkan::postprocess::BlurPushConstants {
                texel_size: [1.0 / mip_w as f32, 1.0 / mip_h as f32],
                direction: 1,
            };
            let pc_bytes2 = bytemuck::bytes_of(&pc2);
            device.cmd_push_constants(
                command_buffer,
                blur_pipeline.pipeline_layout,
                vk::ShaderStageFlags::FRAGMENT,
                0,
                pc_bytes2,
            );
            device.cmd_draw(command_buffer, 3, 1, 0, 0);
            device.cmd_end_render_pass(command_buffer);
        }
    }
}

/// Record the final composite render pass: scene + 8 bloom mips + exposure +
/// tonemap -> swapchain. The composite render pass writes the swapchain
/// attachment in PRESENT_SRC_KHR.
unsafe fn record_composite_pass(
    device: &ash::Device,
    _debug_marker: &DebugMarker,
    command_buffer: vk::CommandBuffer,
    postprocess: &PostProcessResources,
    image_index: usize,
    frame: usize,
    composite_framebuffer: vk::Framebuffer,
    extent: vk::Extent2D,
) {
    let composite_pipeline = postprocess.composite_pipeline.as_ref().unwrap();
    let clear_values = [vk::ClearValue {
        color: vk::ClearColorValue {
            float32: [0.0, 0.0, 0.0, 1.0],
        },
    }];
    let pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(postprocess.composite_render_pass)
        .framebuffer(composite_framebuffer)
        .render_area(vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent,
        })
        .clear_values(&clear_values);

    unsafe {
        device.cmd_begin_render_pass(command_buffer, &pass_begin, vk::SubpassContents::INLINE);
        set_viewport_and_bind_pipeline(
            device,
            command_buffer,
            extent,
            composite_pipeline.pipeline,
        );
        let sets: [vk::DescriptorSet; 2] = [
            postprocess.composite_input_sets[image_index],
            postprocess.ubo_sets[frame],
        ];
        device.cmd_bind_descriptor_sets(
            command_buffer,
            vk::PipelineBindPoint::GRAPHICS,
            composite_pipeline.pipeline_layout,
            0,
            &sets,
            &[],
        );
        device.cmd_draw(command_buffer, 3, 1, 0, 0);
        device.cmd_end_render_pass(command_buffer);
    }
}
