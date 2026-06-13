#![allow(unsafe_op_in_unsafe_fn)]

use ash::vk;

use crate::vulkan::buffer::create_buffer;
use crate::vulkan::context::VulkanContext;
use crate::vulkan::debug_marker::DebugMarker;
use crate::vulkan::postprocess::descriptors::{
    create_composite_input_layout, create_postprocess_ubo_layout, create_single_input_layout,
};
use crate::vulkan::postprocess::fullscreen::create_fullscreen_pipeline;
use crate::vulkan::postprocess::passes::{
    create_postprocess_color_pass, create_scene_render_pass,
};
use crate::vulkan::postprocess::pyramid::{BLOOM_FORMAT, BLOOM_MIP_COUNT, BloomPyramid};
use crate::vulkan::postprocess::ubo::PostProcessUBO;

/// High-level tonemap operator selection. Mirrors the GLSL `uint tonemap_op`.
/// Kept as a type-safe public API for runtime switching.
#[allow(dead_code)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TonemapOp {
    Linear = 0,
    Reinhard = 1,
    Aces = 2,
}

impl TonemapOp {
    /// Cycle to the next operator in the canonical order
    /// Linear -> Reinhard -> ACES -> Linear. The numeric value of each
    /// variant is what gets written to `PostProcessUBO::tonemap_op` and read
    /// by the composite shader's `if/else` chain.
    pub fn next(self) -> Self {
        match self {
            TonemapOp::Linear => TonemapOp::Reinhard,
            TonemapOp::Reinhard => TonemapOp::Aces,
            TonemapOp::Aces => TonemapOp::Linear,
        }
    }

    /// Numeric value of the operator, as the GLSL `uint` expects.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

impl std::fmt::Display for TonemapOp {
    /// User-facing label used in the window title. The shader-side label
    /// stays in the GLSL source (where the `if/else` chain picks the
    /// operator); this is the spelling shown in the OS window title bar.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TonemapOp::Linear => "Linear",
            TonemapOp::Reinhard => "Reinhard",
            // Capitalised to match the conventional spelling (ACES, not Aces).
            TonemapOp::Aces => "ACES",
        };
        f.write_str(s)
    }
}

/// Runtime-tweakable postprocess settings.
///
/// Wraps the GPU-side `PostProcessUBO` (the canonical representation) and
/// adds a CPU-side `bloom_enabled` flag that zeroes out `bloom_intensity`
/// in the UBO copy when bloom is disabled. This avoids duplicating fields
/// between a CPU struct and a GPU struct — the UBO is the single source of
/// truth for the shader-visible layout.
#[derive(Clone)]
pub struct PostProcessSettings {
    pub ubo: PostProcessUBO,
    pub bloom_enabled: bool,
}

impl Default for PostProcessSettings {
    fn default() -> Self {
        Self {
            ubo: PostProcessUBO::default(),
            bloom_enabled: true,
        }
    }
}

pub struct PostProcessResources {
    pub settings: PostProcessSettings,

    // Render passes
    pub scene_render_pass: vk::RenderPass,
    pub postprocess_color_pass: vk::RenderPass, // reused for bright + blur
    /// Composite render pass — owned by the renderer (so it can outlive
    /// resource recreation on swapchain resize). We just hold the handle.
    pub composite_render_pass: vk::RenderPass,

    // Scene color (per swapchain image)
    pub scene_format: vk::Format,
    pub scene_images: Vec<vk::Image>,
    pub scene_memories: Vec<vk::DeviceMemory>,
    pub scene_views: Vec<vk::ImageView>,
    pub scene_framebuffers: Vec<vk::Framebuffer>,

    // Pre-allocated framebuffers for the postprocess passes. These reference
    // bloom mip and temp images, and are created once at startup so the
    // command buffer recording doesn't need to create/destroy them per frame.
    pub bright_mip0_framebuffer: vk::Framebuffer,        // writes bloom mip 0
    pub blur_temp_framebuffers: Vec<vk::Framebuffer>,    // per mip, writes temp[level]
    pub blur_mip_framebuffers: Vec<vk::Framebuffer>,      // per mip, writes mip[level]

    // Bloom pyramid (stable across swapchain recreation if extent unchanged)
    pub bloom: Option<BloomPyramid>,
    pub bloom_extent: (u32, u32),

    // Descriptor layouts
    pub ubo_layout: vk::DescriptorSetLayout,
    pub single_input_layout: vk::DescriptorSetLayout,
    pub composite_input_layout: vk::DescriptorSetLayout,

    // Pipelines
    pub bright_pipeline: Option<crate::vulkan::pipeline::PipelineData>,
    pub blur_pipeline: Option<crate::vulkan::pipeline::PipelineData>,
    pub composite_pipeline: Option<crate::vulkan::pipeline::PipelineData>,

    // Pipeline layouts
    pub bright_pipeline_layout: vk::PipelineLayout,
    pub blur_pipeline_layout: vk::PipelineLayout,
    pub composite_pipeline_layout: vk::PipelineLayout,

    // Descriptor pool + sets
    pub descriptor_pool: vk::DescriptorPool,
    pub ubo: Vec<crate::vulkan::buffer::GpuBuffer>,
    pub ubo_mapped: Vec<*mut u8>,
    pub ubo_sets: Vec<vk::DescriptorSet>,            // per-frame-in-flight
    pub bright_input_sets: Vec<vk::DescriptorSet>,   // per swapchain image
    pub blur_input_sets: Vec<vk::DescriptorSet>,     // per mip
    pub composite_input_sets: Vec<vk::DescriptorSet>, // per swapchain image

    // Sizes for re-allocation
    pub num_swapchain_images: usize,
}

impl PostProcessResources {
    /// Construct postprocess resources. Must be called after the swapchain has
    /// been created (so we know the swapchain image count and the depth format).
    ///
    /// `composite_render_pass` is owned by the caller (the `Renderer`) and is
    /// shared between the swapchain framebuffers and the composite pipeline.
    /// `PostProcessResources` does not destroy it.
    pub fn new(
        ctx: &VulkanContext,
        command_pool: vk::CommandPool,
        depth_format: vk::Format,
        swapchain_format: vk::Format,
        swapchain_extent: vk::Extent2D,
        swapchain_image_views: &[vk::ImageView],
        depth_view: vk::ImageView,
        max_frames_in_flight: usize,
        composite_render_pass: vk::RenderPass,
    ) -> Self {
        let _ = swapchain_format;
        let device = &ctx.device;
        let num_swapchain_images = swapchain_image_views.len();

        // --- Render passes ---
        let scene_render_pass = create_scene_render_pass(device, BLOOM_FORMAT, depth_format);
        let postprocess_color_pass = create_postprocess_color_pass(device, BLOOM_FORMAT);

        // --- Scene color images / views / memories ---
        let mut scene_images = Vec::with_capacity(num_swapchain_images);
        let mut scene_memories = Vec::with_capacity(num_swapchain_images);
        let mut scene_views = Vec::with_capacity(num_swapchain_images);
        for _ in 0..num_swapchain_images {
            let extent = vk::Extent3D {
                width: swapchain_extent.width,
                height: swapchain_extent.height,
                depth: 1,
            };
            let image_info = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .extent(extent)
                .mip_levels(1)
                .array_layers(1)
                .format(BLOOM_FORMAT)
                .tiling(vk::ImageTiling::OPTIMAL)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .usage(
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .samples(vk::SampleCountFlags::TYPE_1)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let image = unsafe { device.create_image(&image_info, None).unwrap() };
            let mem_reqs = unsafe { device.get_image_memory_requirements(image) };
            let mem_type = crate::vulkan::buffer::find_memory_type(
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

            let view_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(BLOOM_FORMAT)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let view = unsafe { device.create_image_view(&view_info, None).unwrap() };

            scene_images.push(image);
            scene_memories.push(memory);
            scene_views.push(view);
        }

        // --- Scene color framebuffers ---
        let mut scene_framebuffers = Vec::with_capacity(num_swapchain_images);
        for &view in scene_views.iter() {
            let attachments = [view, depth_view];
            let info = vk::FramebufferCreateInfo::default()
                .render_pass(scene_render_pass)
                .attachments(&attachments)
                .width(swapchain_extent.width)
                .height(swapchain_extent.height)
                .layers(1);
            let fb = unsafe { device.create_framebuffer(&info, None).unwrap() };
            scene_framebuffers.push(fb);
        }

        // --- Bloom pyramid ---
        let bloom_extent = (swapchain_extent.width, swapchain_extent.height);
        let bloom = Some(BloomPyramid::new(
            ctx,
            swapchain_extent.width,
            swapchain_extent.height,
        ));

        // --- Pre-initialize all bloom mip + temp images to SHADER_READ_ONLY_OPTIMAL ---
        // Two single-image barriers (one per pyramid image) cover all mip levels
        // via level_count = BLOOM_MIP_COUNT. Previously this was 16 per-image
        // barriers. This is safer because the single-image approach means every
        // level is covered by a single transition — no possibility of a missing
        // mip level causing a DEVICE_LOST read from UNDEFINED.
        let bloom_ref = bloom.as_ref().unwrap();
        let init_barriers = [
            vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(bloom_ref.mip_image())
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: BLOOM_MIP_COUNT as u32,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_READ),
            vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(bloom_ref.temp_image())
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: BLOOM_MIP_COUNT as u32,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::SHADER_READ),
        ];
        crate::vulkan::buffer::with_one_time_command(ctx, command_pool, |cmd| unsafe {
            device.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &init_barriers,
            );
        });

        // --- Pre-allocate framebuffers for bright + blur passes ---
        // These reference bloom mip and temp images, so they must be
        // allocated after the pyramid. Creating them once at startup avoids
        // per-frame create/destroy, which can be a use-after-free hazard if
        // the command buffer is still in flight at destroy time, and is
        // wasteful anyway.
        let (mip0_w, mip0_h) = BloomPyramid::mip_extent(bloom_extent.0, bloom_extent.1, 0);
        let bright_mip0_framebuffer = {
            let attachments = [bloom.as_ref().unwrap().mip_views[0]];
            let info = vk::FramebufferCreateInfo::default()
                .render_pass(postprocess_color_pass)
                .attachments(&attachments)
                .width(mip0_w)
                .height(mip0_h)
                .layers(1);
            unsafe { device.create_framebuffer(&info, None).unwrap() }
        };
        let mut blur_temp_framebuffers = Vec::with_capacity(BLOOM_MIP_COUNT);
        let mut blur_mip_framebuffers = Vec::with_capacity(BLOOM_MIP_COUNT);
        for level in 0..BLOOM_MIP_COUNT {
            let (w, h) = BloomPyramid::mip_extent(bloom_extent.0, bloom_extent.1, level);
            let temp_attachments = [bloom.as_ref().unwrap().temp_views[level]];
            let temp_info = vk::FramebufferCreateInfo::default()
                .render_pass(postprocess_color_pass)
                .attachments(&temp_attachments)
                .width(w)
                .height(h)
                .layers(1);
            let temp_fb = unsafe { device.create_framebuffer(&temp_info, None).unwrap() };
            blur_temp_framebuffers.push(temp_fb);

            let mip_attachments = [bloom.as_ref().unwrap().mip_views[level]];
            let mip_info = vk::FramebufferCreateInfo::default()
                .render_pass(postprocess_color_pass)
                .attachments(&mip_attachments)
                .width(w)
                .height(h)
                .layers(1);
            let mip_fb = unsafe { device.create_framebuffer(&mip_info, None).unwrap() };
            blur_mip_framebuffers.push(mip_fb);
        }

        // --- Descriptor layouts ---
        let ubo_layout = create_postprocess_ubo_layout(device);
        let single_input_layout = create_single_input_layout(device);
        let composite_input_layout = create_composite_input_layout(device);

        // --- Pipeline layouts ---
        // Postprocess shaders declare bindings at `set = 0` (input samplers)
        // and `set = 1` (the postprocess UBO). The two set layouts must be
        // supplied in the pipeline layout in order.
        // Bright: set 0 = scene sampler, set 1 = UBO
        let bright_set_layouts = [single_input_layout, ubo_layout];
        let bright_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&bright_set_layouts),
                    None,
                )
                .unwrap()
        };
        // Blur: set 0 = input, set 1 = UBO + push constants
        let blur_set_layouts = [single_input_layout, ubo_layout];
        let blur_push_range = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(std::mem::size_of::<crate::vulkan::postprocess::ubo::BlurPushConstants>() as u32);
        let blur_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&blur_set_layouts)
                        .push_constant_ranges(std::slice::from_ref(&blur_push_range)),
                    None,
                )
                .unwrap()
        };
        // Composite: set 0 = 9 samplers, set 1 = UBO
        let composite_set_layouts = [composite_input_layout, ubo_layout];
        let composite_pipeline_layout = unsafe {
            device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&composite_set_layouts),
                    None,
                )
                .unwrap()
        };

        // --- Pipelines ---
        let bright_frag = include_bytes!("../../../shaders/postprocess/bright.frag.spv");
        let blur_frag = include_bytes!("../../../shaders/postprocess/blur.frag.spv");
        let composite_frag = include_bytes!("../../../shaders/postprocess/composite.frag.spv");

        let bright_pipeline = Some(create_fullscreen_pipeline(
            device,
            postprocess_color_pass,
            bright_pipeline_layout,
            bright_frag,
        ));
        let blur_pipeline = Some(create_fullscreen_pipeline(
            device,
            postprocess_color_pass,
            blur_pipeline_layout,
            blur_frag,
        ));
        let composite_pipeline = Some(create_fullscreen_pipeline(
            device,
            composite_render_pass,
            composite_pipeline_layout,
            composite_frag,
        ));

        // --- Descriptor pool ---
        let ubo_count = max_frames_in_flight as u32;
        let sampler_count =
            (num_swapchain_images + BLOOM_MIP_COUNT * 2 + num_swapchain_images * 9) as u32;
        let pool_sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: ubo_count,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
                descriptor_count: sampler_count,
            },
        ];
        // Total sets:
        //   - ubo_count UBO sets (per frame in flight)
        //   - num_swapchain_images bright-input sets
        //   - BLOOM_MIP_COUNT * 2 blur-input sets (one per mip per direction)
        //   - num_swapchain_images composite-input sets
        let total_sets = ubo_count
            + 2 * num_swapchain_images as u32
            + 2 * BLOOM_MIP_COUNT as u32;
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .pool_sizes(&pool_sizes)
            .max_sets(total_sets);
        let descriptor_pool = unsafe { device.create_descriptor_pool(&pool_info, None).unwrap() };

        // --- UBO buffers (one per frame in flight) ---
        let ubo_size = std::mem::size_of::<PostProcessUBO>() as vk::DeviceSize;
        let mut ubo = Vec::with_capacity(max_frames_in_flight);
        let mut ubo_mapped = Vec::with_capacity(max_frames_in_flight);
        for _ in 0..max_frames_in_flight {
            let buf = create_buffer(
                device,
                ubo_size,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                &ctx.instance,
                ctx.physical_device,
            );
            let ptr = unsafe {
                device
                    .map_memory(buf.memory, 0, ubo_size, vk::MemoryMapFlags::empty())
                    .unwrap()
            } as *mut u8;
            ubo.push(buf);
            ubo_mapped.push(ptr);
        }

        // --- UBO descriptor sets ---
        // Allocate these now so the UBO buffers can be bound to them.
        let mut ubo_set_layouts = vec![ubo_layout; max_frames_in_flight];
        let ubo_alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(descriptor_pool)
            .set_layouts(&mut ubo_set_layouts);
        let ubo_sets = unsafe { device.allocate_descriptor_sets(&ubo_alloc).unwrap() };
        for (i, &set) in ubo_sets.iter().enumerate() {
            let info = vk::DescriptorBufferInfo::default()
                .buffer(ubo[i].buffer)
                .offset(0)
                .range(ubo_size);
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(std::slice::from_ref(&info));
            unsafe { device.update_descriptor_sets(std::slice::from_ref(&write), &[]) };
        }

        // Defer per-image set allocation to first frame setup. They need
        // scene color views, which exist now.
        let mut me = Self {
            settings: PostProcessSettings::default(),
            scene_render_pass,
            postprocess_color_pass,
            composite_render_pass,
            scene_format: BLOOM_FORMAT,
            scene_images,
            scene_memories,
            scene_views,
            scene_framebuffers,
            bloom,
            bloom_extent,
            bright_mip0_framebuffer,
            blur_temp_framebuffers,
            blur_mip_framebuffers,
            ubo_layout,
            single_input_layout,
            composite_input_layout,
            bright_pipeline,
            blur_pipeline,
            composite_pipeline,
            bright_pipeline_layout,
            blur_pipeline_layout,
            composite_pipeline_layout,
            descriptor_pool,
            ubo,
            ubo_mapped,
            ubo_sets,
            bright_input_sets: Vec::new(),
            blur_input_sets: Vec::new(),
            composite_input_sets: Vec::new(),
            num_swapchain_images,
        };
        me.allocate_input_sets(ctx);
        // Initialize per-mip blur and per-swapchain bright/composite descriptor
        // sets after we have image views. We also do this on resize.
        me
    }

    /// Allocate the bright/blur/composite input sets and write their initial
    /// image bindings. UBO sets are allocated in `new()` directly.
    ///
    /// On resize, call `reset_descriptor_pool` first to free old sets, then
    /// re-allocate the UBO sets and call this.
    fn allocate_input_sets(&mut self, ctx: &VulkanContext) {
        let device = &ctx.device;
        let bloom = self.bloom.as_ref().expect("bloom must exist");
        let bloom_sampler = bloom.sampler;

        // Bright input sets: one per swapchain image, sample scene color.
        let layouts = vec![self.single_input_layout; self.num_swapchain_images];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&layouts);
        self.bright_input_sets = unsafe { device.allocate_descriptor_sets(&alloc).unwrap() };
        for (i, &set) in self.bright_input_sets.iter().enumerate() {
            let img = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(self.scene_views[i])
                .sampler(bloom_sampler);
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&img));
            unsafe { device.update_descriptor_sets(std::slice::from_ref(&write), &[]) };
        }

        // Blur input sets: TWO per mip (horizontal reads mip[i], vertical reads
        // temp[i]). 16 sets total. Each is bound to the same pipeline slot
        // but holds a different image, avoiding the need to update descriptor
        // sets mid-recording.
        // Layout: index 2*i     = horizontal (samples mip[i])
        //         index 2*i + 1 = vertical   (samples temp[i])
        let layouts = vec![self.single_input_layout; BLOOM_MIP_COUNT * 2];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&layouts);
        self.blur_input_sets = unsafe { device.allocate_descriptor_sets(&alloc).unwrap() };
        for i in 0..BLOOM_MIP_COUNT {
            // Horizontal: samples mip[i]
            let img_h = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(bloom.mip_views[i])
                .sampler(bloom_sampler);
            let write_h = vk::WriteDescriptorSet::default()
                .dst_set(self.blur_input_sets[2 * i])
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&img_h));
            unsafe { device.update_descriptor_sets(std::slice::from_ref(&write_h), &[]) };
            // Vertical: samples temp[i]
            let img_v = vk::DescriptorImageInfo::default()
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .image_view(bloom.temp_views[i])
                .sampler(bloom_sampler);
            let write_v = vk::WriteDescriptorSet::default()
                .dst_set(self.blur_input_sets[2 * i + 1])
                .dst_binding(0)
                .dst_array_element(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(std::slice::from_ref(&img_v));
            unsafe { device.update_descriptor_sets(std::slice::from_ref(&write_v), &[]) };
        }

        // Composite input sets: one per swapchain image. 9 bindings: 0 = scene
        // color, 1..8 = bloom mip i-1.
        let layouts = vec![self.composite_input_layout; self.num_swapchain_images];
        let alloc = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.descriptor_pool)
            .set_layouts(&layouts);
        self.composite_input_sets = unsafe { device.allocate_descriptor_sets(&alloc).unwrap() };
        for (i, &set) in self.composite_input_sets.iter().enumerate() {
            let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::with_capacity(9);
            image_infos.push(
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(self.scene_views[i])
                    .sampler(bloom_sampler),
            );
            for mip_view in bloom.mip_views.iter() {
                image_infos.push(
                    vk::DescriptorImageInfo::default()
                        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                        .image_view(*mip_view)
                        .sampler(bloom_sampler),
                );
            }
            let writes: Vec<vk::WriteDescriptorSet> = (0..9)
                .map(|b| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(b)
                        .dst_array_element(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(std::slice::from_ref(&image_infos[b as usize]))
                })
                .collect();
            unsafe { device.update_descriptor_sets(&writes, &[]) };
        }
        let _ = ctx;
    }

    /// Update the postprocess UBO for the given frame. Call after settings
    /// have been updated and before recording the frame's command buffer.
    pub fn update_ubo(&self, frame: usize) {
        let mut ubo = self.settings.ubo;
        if !self.settings.bloom_enabled {
            ubo.bloom_intensity = 0.0;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytemuck::bytes_of(&ubo).as_ptr(),
                self.ubo_mapped[frame],
                std::mem::size_of::<PostProcessUBO>(),
            );
        }
    }

    /// Assign debug-object names to all postprocess resources for RenderDoc
    /// and validation layer diagnostics. Safe to call before any rendering;
    /// unsupported extensions are no-ops.
    pub fn name_debug_objects(&self, dm: &DebugMarker) {
        unsafe {
            // Render passes (composite is owned by the renderer and named there)
            dm.set_object_name(self.scene_render_pass, "Scene HDR Render Pass");
            dm.set_object_name(
                self.postprocess_color_pass,
                "Postprocess Color Render Pass",
            );

            // Scene color images (per swapchain image)
            for (i, (&image, (&view, &memory))) in self
                .scene_images
                .iter()
                .zip(self.scene_views.iter().zip(self.scene_memories.iter()))
                .enumerate()
            {
                dm.set_object_name(image, &format!("Scene Color Image {}", i));
                dm.set_object_name(view, &format!("Scene Color View {}", i));
                dm.set_object_name(memory, &format!("Scene Color Memory {}", i));
                dm.set_object_name(
                    self.scene_framebuffers[i],
                    &format!("Scene Color Framebuffer {}", i),
                );
            }

            // Bloom sampler
            if let Some(bloom) = self.bloom.as_ref() {
                dm.set_object_name(bloom.sampler, "Bloom Sampler");
                dm.set_object_name(bloom.mip_image(), "Bloom Mip Image");
                dm.set_object_name(bloom.temp_image(), "Bloom Temp Image");
                for (i, &view) in bloom.mip_views.iter().enumerate() {
                    dm.set_object_name(view, &format!("Bloom Mip View {}", i));
                }
                for (i, &view) in bloom.temp_views.iter().enumerate() {
                    dm.set_object_name(view, &format!("Bloom Temp View {}", i));
                }
            }

            // Bloom framebuffers
            dm.set_object_name(self.bright_mip0_framebuffer, "Bloom Prefilter Framebuffer");
            for (i, &fb) in self.blur_temp_framebuffers.iter().enumerate() {
                dm.set_object_name(fb, &format!("Bloom Blur Temp Framebuffer {}", i));
            }
            for (i, &fb) in self.blur_mip_framebuffers.iter().enumerate() {
                dm.set_object_name(fb, &format!("Bloom Blur Mip Framebuffer {}", i));
            }

            // Pipelines
            if let Some(ref bp) = self.bright_pipeline {
                dm.set_object_name(bp.pipeline, "Bloom Prefilter Pipeline");
                dm.set_object_name(bp.pipeline_layout, "Bloom Prefilter Pipeline Layout");
            }
            if let Some(ref bp) = self.blur_pipeline {
                dm.set_object_name(bp.pipeline, "Bloom Blur Pipeline");
                dm.set_object_name(bp.pipeline_layout, "Bloom Blur Pipeline Layout");
            }
            if let Some(ref cp) = self.composite_pipeline {
                dm.set_object_name(cp.pipeline, "Composite Pipeline");
                dm.set_object_name(cp.pipeline_layout, "Composite Pipeline Layout");
            }

            // Descriptor layouts
            dm.set_object_name(self.ubo_layout, "Postprocess UBO Desc Layout");
            dm.set_object_name(
                self.single_input_layout,
                "Postprocess Single-Input Desc Layout",
            );
            dm.set_object_name(
                self.composite_input_layout,
                "Postprocess Composite-Input Desc Layout",
            );

            // Descriptor pool
            dm.set_object_name(self.descriptor_pool, "Postprocess Descriptor Pool");

            // UBO buffers (per frame in flight)
            for (i, buf) in self.ubo.iter().enumerate() {
                dm.set_object_name(
                    buf.buffer,
                    &format!("Postprocess UBO Buffer Frame {}", i),
                );
                dm.set_object_name(
                    buf.memory,
                    &format!("Postprocess UBO Memory Frame {}", i),
                );
            }

            // DUBO descriptor sets (per frame in flight)
            for (i, &set) in self.ubo_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("Postprocess UBO Set Frame {}", i));
            }

            // Input descriptor sets
            for (i, &set) in self.bright_input_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("Bloom Prefilter Input Set Image {}", i));
            }
            for (i, &set) in self.blur_input_sets.iter().enumerate() {
                let level = i / 2;
                let dir = if i % 2 == 0 { "H" } else { "V" };
                dm.set_object_name(
                    set,
                    &format!("Bloom Blur Input Set Level {} {}", level, dir),
                );
            }
            for (i, &set) in self.composite_input_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("Composite Input Set Image {}", i));
            }
        }
    }

    /// Destroy all device resources. Call from `Renderer::drop` before the
    /// descriptor pool, scene, and pipelines are destroyed.
    pub unsafe fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            // Pipelines + layouts
            if let Some(p) = self.bright_pipeline.take() {
                device.destroy_pipeline(p.pipeline, None);
            }
            if let Some(p) = self.blur_pipeline.take() {
                device.destroy_pipeline(p.pipeline, None);
            }
            if let Some(p) = self.composite_pipeline.take() {
                device.destroy_pipeline(p.pipeline, None);
            }
            device.destroy_pipeline_layout(self.bright_pipeline_layout, None);
            device.destroy_pipeline_layout(self.blur_pipeline_layout, None);
            device.destroy_pipeline_layout(self.composite_pipeline_layout, None);

            // Render passes (owned)
            device.destroy_render_pass(self.scene_render_pass, None);
            device.destroy_render_pass(self.postprocess_color_pass, None);
            // composite_render_pass is owned by the Renderer; do not destroy.

            // Descriptor pool (frees all sets)
            device.destroy_descriptor_pool(self.descriptor_pool, None);

            // Descriptor layouts
            device.destroy_descriptor_set_layout(self.ubo_layout, None);
            device.destroy_descriptor_set_layout(self.single_input_layout, None);
            device.destroy_descriptor_set_layout(self.composite_input_layout, None);

            // UBOs
            for (buf, mapped) in self.ubo.drain(..).zip(self.ubo_mapped.drain(..)) {
                device.unmap_memory(buf.memory);
                let _ = mapped;
                buf.destroy(device);
            }

            // Scene color images
            for view in self.scene_views.drain(..) {
                device.destroy_image_view(view, None);
            }
            for image in self.scene_images.drain(..) {
                device.destroy_image(image, None);
            }
            for mem in self.scene_memories.drain(..) {
                device.free_memory(mem, None);
            }
            for fb in self.scene_framebuffers.drain(..) {
                device.destroy_framebuffer(fb, None);
            }

            // Postprocess framebuffers (bright + blur). These were pre-allocated
            // at startup; we must destroy them before destroying the bloom
            // pyramid images they reference.
            device.destroy_framebuffer(self.bright_mip0_framebuffer, None);
            for fb in self.blur_temp_framebuffers.drain(..) {
                device.destroy_framebuffer(fb, None);
            }
            for fb in self.blur_mip_framebuffers.drain(..) {
                device.destroy_framebuffer(fb, None);
            }

            // Bloom pyramid
            if let Some(mut b) = self.bloom.take() {
                b.destroy(device);
            }
        }
    }
}
