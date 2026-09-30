#![allow(unsafe_op_in_unsafe_fn)]

use ash::vk;
use gpu_allocator::MemoryLocation;

use crate::vulkan::context::VulkanContext;
use crate::vulkan::debug_marker::DebugMarker;
use crate::vulkan::deferred::descriptors::create_gbuffer_input_layout;
use crate::vulkan::deferred::passes::{
    create_deferred_lighting_render_pass, create_gbuffer_render_pass,
};
use crate::vulkan::memory::MemoryAllocator;
use crate::vulkan::pbr_ubo::PushConstants;
use crate::vulkan::pipeline::{PipelineData, create_gbuffer_pipeline};
use crate::vulkan::postprocess::fullscreen::create_fullscreen_pipeline;

/// Number of color targets written by the G-buffer pass.
pub const GBUFFER_COLOR_TARGET_COUNT: usize = 3;

/// Number of G-buffer inputs sampled by the lighting pass: the color targets
/// plus the depth buffer.
pub const GBUFFER_INPUT_COUNT: usize = GBUFFER_COLOR_TARGET_COUNT + 1;

/// Index of each color target inside the per-swapchain-image arrays.
pub const IDX_ALBEDO: usize = 0;
pub const IDX_NORMAL: usize = 1;
pub const IDX_EMISSIVE: usize = 2;

/// `RGBA16F` for every G-buffer target: albedo/emissive stay HDR-safe, the
/// world normal needs signed values, and roughness/metallic/occlusion ride
/// along in the free `.a` channels.
pub const GBUFFER_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// G-buffer visualisation mode of the deferred lighting pass. Mirrors the
/// GLSL `uint debugView` (the `if/else` chain at the end of `deferred.frag`).
/// Kept as a type-safe public API for runtime switching (the `G` keybind).
#[allow(dead_code)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredDebugView {
    Shaded = 0,
    Albedo = 1,
    Normal = 2,
    Roughness = 3,
    Metallic = 4,
    Occlusion = 5,
    Depth = 6,
}

impl DeferredDebugView {
    /// Cycle to the next mode in the canonical order
    /// Shaded -> Albedo -> Normal -> Roughness -> Metallic -> Occlusion ->
    /// Depth -> Shaded. The numeric value is what gets written to
    /// `GlobalUniforms::set_debug_view`.
    pub fn next(self) -> Self {
        match self {
            DeferredDebugView::Shaded => DeferredDebugView::Albedo,
            DeferredDebugView::Albedo => DeferredDebugView::Normal,
            DeferredDebugView::Normal => DeferredDebugView::Roughness,
            DeferredDebugView::Roughness => DeferredDebugView::Metallic,
            DeferredDebugView::Metallic => DeferredDebugView::Occlusion,
            DeferredDebugView::Occlusion => DeferredDebugView::Depth,
            DeferredDebugView::Depth => DeferredDebugView::Shaded,
        }
    }

    /// Numeric value of the mode, as the GLSL `uint` expects.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

impl std::fmt::Display for DeferredDebugView {
    /// User-facing label used in the window title and the console log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DeferredDebugView::Shaded => "Shaded",
            DeferredDebugView::Albedo => "Albedo",
            DeferredDebugView::Normal => "Normal",
            DeferredDebugView::Roughness => "Roughness",
            DeferredDebugView::Metallic => "Metallic",
            DeferredDebugView::Occlusion => "AO",
            DeferredDebugView::Depth => "Depth",
        };
        f.write_str(s)
    }
}

/// Owns every device object of the deferred path: the G-buffer images/views/
/// framebuffers, both render passes, both pipelines, and the lighting pass's
/// descriptor sets.
///
/// The G-buffer is allocated **per swapchain image**, exactly like the
/// postprocess scene color, so the existing `images_in_flight` fence ordering
/// is what prevents two in-flight frames from touching the same G-buffer.
pub struct GBufferResources {
    // Render passes
    pub gbuffer_render_pass: vk::RenderPass,
    pub lighting_render_pass: vk::RenderPass,

    // G-buffer, per swapchain image
    pub images: Vec<[vk::Image; GBUFFER_COLOR_TARGET_COUNT]>,
    pub allocations: Vec<[gpu_allocator::vulkan::Allocation; GBUFFER_COLOR_TARGET_COUNT]>,
    pub views: Vec<[vk::ImageView; GBUFFER_COLOR_TARGET_COUNT]>,
    /// Framebuffers for the G-buffer pass: 3 color targets + depth.
    pub framebuffers: Vec<vk::Framebuffer>,
    /// Framebuffers for the lighting pass: the postprocess scene-color image
    /// of the matching swapchain image. Owned here because the lighting pass
    /// is part of the deferred path, but the image itself belongs to
    /// `PostProcessResources` — so these must be destroyed *before* it.
    pub lighting_framebuffers: Vec<vk::Framebuffer>,

    pub sampler: vk::Sampler,

    // Pipelines
    pub gbuffer_pipeline: Option<PipelineData>,
    pub lighting_pipeline: Option<PipelineData>,

    // Descriptors
    pub input_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    /// One set per swapchain image: the three G-buffer targets + depth.
    pub input_sets: Vec<vk::DescriptorSet>,
}

impl GBufferResources {
    /// Construct the deferred path's resources. Must be called after
    /// `PostProcessResources` (the lighting pass renders into its scene-color
    /// images) and after the swapchain exists (depth view + image count).
    pub fn new(
        ctx: &mut VulkanContext,
        depth_format: vk::Format,
        depth_view: vk::ImageView,
        extent: vk::Extent2D,
        scene_color_views: &[vk::ImageView],
        global_layout: vk::DescriptorSetLayout,
        material_layout: vk::DescriptorSetLayout,
    ) -> Self {
        let num_swapchain_images = scene_color_views.len();

        // --- Render passes ---
        let gbuffer_render_pass =
            create_gbuffer_render_pass(&ctx.device, GBUFFER_FORMAT, depth_format);
        let lighting_render_pass =
            create_deferred_lighting_render_pass(&ctx.device, GBUFFER_FORMAT);

        // --- G-buffer images / views / allocations ---
        let mut images: Vec<[vk::Image; GBUFFER_COLOR_TARGET_COUNT]> =
            Vec::with_capacity(num_swapchain_images);
        let mut allocations: Vec<[gpu_allocator::vulkan::Allocation; GBUFFER_COLOR_TARGET_COUNT]> =
            Vec::with_capacity(num_swapchain_images);
        let mut views: Vec<[vk::ImageView; GBUFFER_COLOR_TARGET_COUNT]> =
            Vec::with_capacity(num_swapchain_images);

        for i in 0..num_swapchain_images {
            let mut images_i = Vec::with_capacity(GBUFFER_COLOR_TARGET_COUNT);
            let mut allocations_i = Vec::with_capacity(GBUFFER_COLOR_TARGET_COUNT);
            let mut views_i = Vec::with_capacity(GBUFFER_COLOR_TARGET_COUNT);

            for target in 0..GBUFFER_COLOR_TARGET_COUNT {
                let name = match target {
                    IDX_ALBEDO => "GBufferAlbedo",
                    IDX_NORMAL => "GBufferNormal",
                    _ => "GBufferEmissive",
                };
                let image_info = vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .extent(vk::Extent3D {
                        width: extent.width,
                        height: extent.height,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .format(GBUFFER_FORMAT)
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
                let owned = ctx.allocator.create_image(
                    &ctx.device,
                    &format!("{}_{}", name, i),
                    &image_info,
                    MemoryLocation::GpuOnly,
                );

                let view_info = vk::ImageViewCreateInfo::default()
                    .image(owned.image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(GBUFFER_FORMAT)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
                    });
                let view = unsafe { ctx.device.create_image_view(&view_info, None).unwrap() };

                allocations_i.push(owned.allocation.expect("g-buffer: allocation missing"));
                images_i.push(owned.image);
                views_i.push(view);
            }

            allocations.push(
                allocations_i
                    .try_into()
                    .ok()
                    .expect("g-buffer: exactly three targets"),
            );
            images.push(
                images_i
                    .try_into()
                    .ok()
                    .expect("g-buffer: exactly three targets"),
            );
            views.push(
                views_i
                    .try_into()
                    .ok()
                    .expect("g-buffer: exactly three targets"),
            );
        }

        // --- Framebuffers ---
        let mut framebuffers = Vec::with_capacity(num_swapchain_images);
        let mut lighting_framebuffers = Vec::with_capacity(num_swapchain_images);
        for i in 0..num_swapchain_images {
            let attachments = [
                views[i][IDX_ALBEDO],
                views[i][IDX_NORMAL],
                views[i][IDX_EMISSIVE],
                depth_view,
            ];
            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(gbuffer_render_pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            framebuffers.push(unsafe { ctx.device.create_framebuffer(&fb_info, None).unwrap() });

            let lighting_attachments = [scene_color_views[i]];
            let lighting_fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(lighting_render_pass)
                .attachments(&lighting_attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            lighting_framebuffers.push(unsafe {
                ctx.device.create_framebuffer(&lighting_fb_info, None).unwrap()
            });
        }

        // --- Sampler: NEAREST so the depth target never needs filter support
        //     (linear filtering of D32_SFLOAT is optional in Vulkan). ---
        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .anisotropy_enable(false)
            .max_anisotropy(1.0)
            .border_color(vk::BorderColor::INT_OPAQUE_BLACK)
            .unnormalized_coordinates(false)
            .compare_enable(false)
            .compare_op(vk::CompareOp::ALWAYS)
            .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
            .mip_lod_bias(0.0)
            .min_lod(0.0)
            .max_lod(0.0);
        let sampler = unsafe { ctx.device.create_sampler(&sampler_info, None).unwrap() };

        // --- Descriptor layout + pool + sets ---
        let input_layout = create_gbuffer_input_layout(&ctx.device);
        let pool_sizes = [vk::DescriptorPoolSize {
            ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
            descriptor_count: (num_swapchain_images * GBUFFER_INPUT_COUNT) as u32,
        }];
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .pool_sizes(&pool_sizes)
            .max_sets(num_swapchain_images as u32);
        let descriptor_pool =
            unsafe { ctx.device.create_descriptor_pool(&pool_info, None).unwrap() };

        let layouts = vec![input_layout; num_swapchain_images];
        let alloc_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(descriptor_pool)
            .set_layouts(&layouts);
        let input_sets = unsafe { ctx.device.allocate_descriptor_sets(&alloc_info).unwrap() };

        for (i, &set) in input_sets.iter().enumerate() {
            let image_infos = [
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(views[i][IDX_ALBEDO])
                    .sampler(sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(views[i][IDX_NORMAL])
                    .sampler(sampler),
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                    .image_view(views[i][IDX_EMISSIVE])
                    .sampler(sampler),
                // The depth buffer is left in DEPTH_STENCIL_READ_ONLY_OPTIMAL
                // by the G-buffer pass, which is a valid sampling layout.
                vk::DescriptorImageInfo::default()
                    .image_layout(vk::ImageLayout::DEPTH_STENCIL_READ_ONLY_OPTIMAL)
                    .image_view(depth_view)
                    .sampler(sampler),
            ];
            let writes: Vec<_> = (0..GBUFFER_INPUT_COUNT)
                .map(|binding| {
                    vk::WriteDescriptorSet::default()
                        .dst_set(set)
                        .dst_binding(binding as u32)
                        .dst_array_element(0)
                        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                        .image_info(std::slice::from_ref(&image_infos[binding]))
                })
                .collect();
            unsafe { ctx.device.update_descriptor_sets(&writes, &[]) };
        }

        // --- Pipelines ---
        let push_constant_size = std::mem::size_of::<PushConstants>() as u32;
        let gbuffer_pipeline = Some(create_gbuffer_pipeline(
            &ctx.device,
            gbuffer_render_pass,
            global_layout,
            material_layout,
            push_constant_size,
        ));

        let lighting_set_layouts = [global_layout, input_layout];
        let lighting_pipeline_layout = unsafe {
            ctx.device
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default()
                        .set_layouts(&lighting_set_layouts),
                    None,
                )
                .unwrap()
        };
        let lighting_frag = include_bytes!("../../../shaders/deferred.frag.spv");
        let lighting_pipeline = Some(create_fullscreen_pipeline(
            &ctx.device,
            lighting_render_pass,
            lighting_pipeline_layout,
            lighting_frag,
        ));

        Self {
            gbuffer_render_pass,
            lighting_render_pass,
            images,
            allocations,
            views,
            framebuffers,
            lighting_framebuffers,
            sampler,
            gbuffer_pipeline,
            lighting_pipeline,
            input_layout,
            descriptor_pool,
            input_sets,
        }
    }

    /// Assign debug-object names for RenderDoc and validation diagnostics.
    pub fn name_debug_objects(&self, dm: &DebugMarker) {
        unsafe {
            dm.set_object_name(self.gbuffer_render_pass, "G-Buffer Render Pass");
            dm.set_object_name(self.lighting_render_pass, "Deferred Lighting Render Pass");
            dm.set_object_name(self.sampler, "G-Buffer Sampler");
            dm.set_object_name(self.input_layout, "G-Buffer Input Desc Layout");
            dm.set_object_name(self.descriptor_pool, "G-Buffer Descriptor Pool");

            for (i, (&images, &views)) in self.images.iter().zip(self.views.iter()).enumerate() {
                for (t, (&image, &view)) in images.iter().zip(views.iter()).enumerate() {
                    let target = match t {
                        IDX_ALBEDO => "Albedo",
                        IDX_NORMAL => "Normal",
                        _ => "Emissive",
                    };
                    dm.set_object_name(image, &format!("G-Buffer {} Image {}", target, i));
                    dm.set_object_name(view, &format!("G-Buffer {} View {}", target, i));
                }
            }
            for (i, allocs) in self.allocations.iter().enumerate() {
                for (t, alloc) in allocs.iter().enumerate() {
                    let target = match t {
                        IDX_ALBEDO => "Albedo",
                        IDX_NORMAL => "Normal",
                        _ => "Emissive",
                    };
                    dm.set_object_name(
                        alloc.memory(),
                        &format!("G-Buffer {} Memory {}", target, i),
                    );
                }
            }

            for (i, &fb) in self.framebuffers.iter().enumerate() {
                dm.set_object_name(fb, &format!("G-Buffer Framebuffer {}", i));
            }
            for (i, &fb) in self.lighting_framebuffers.iter().enumerate() {
                dm.set_object_name(fb, &format!("Deferred Lighting Framebuffer {}", i));
            }

            if let Some(ref p) = self.gbuffer_pipeline {
                dm.set_object_name(p.pipeline, "G-Buffer Graphics Pipeline");
                dm.set_object_name(p.pipeline_layout, "G-Buffer Pipeline Layout");
            }
            if let Some(ref p) = self.lighting_pipeline {
                dm.set_object_name(p.pipeline, "Deferred Lighting Pipeline");
                dm.set_object_name(p.pipeline_layout, "Deferred Lighting Pipeline Layout");
            }

            for (i, &set) in self.input_sets.iter().enumerate() {
                dm.set_object_name(set, &format!("G-Buffer Input Set {}", i));
            }
        }
    }

    /// Destroy all device resources. Call from `Renderer::destroy` and from
    /// `recreate_swapchain` **before** destroying `PostProcessResources`,
    /// whose scene-color image views are referenced by
    /// `lighting_framebuffers`.
    pub unsafe fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe {
            if let Some(p) = self.gbuffer_pipeline.take() {
                device.destroy_pipeline(p.pipeline, None);
                device.destroy_pipeline_layout(p.pipeline_layout, None);
            }
            if let Some(p) = self.lighting_pipeline.take() {
                device.destroy_pipeline(p.pipeline, None);
                device.destroy_pipeline_layout(p.pipeline_layout, None);
            }

            device.destroy_render_pass(self.gbuffer_render_pass, None);
            device.destroy_render_pass(self.lighting_render_pass, None);

            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.input_layout, None);

            for fb in self.framebuffers.drain(..) {
                device.destroy_framebuffer(fb, None);
            }
            for fb in self.lighting_framebuffers.drain(..) {
                device.destroy_framebuffer(fb, None);
            }

            for views in self.views.drain(..) {
                for view in views {
                    device.destroy_image_view(view, None);
                }
            }
            for images in self.images.drain(..) {
                for image in images {
                    device.destroy_image(image, None);
                }
            }
            for allocs in self.allocations.drain(..) {
                for alloc in allocs {
                    allocator
                        .inner
                        .free(alloc)
                        .expect("Failed to free G-buffer allocation");
                }
            }

            device.destroy_sampler(self.sampler, None);
        }
    }
}
