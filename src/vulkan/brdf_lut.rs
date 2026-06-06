use crate::vulkan::buffer::{find_memory_type, with_one_time_command};
use crate::vulkan::context::VulkanContext;
use ash::vk;

pub struct BrdfLut {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
}

impl BrdfLut {
    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            device.destroy_sampler(self.sampler, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }
}

pub fn generate_brdf_lut(ctx: &VulkanContext, command_pool: vk::CommandPool) -> BrdfLut {
    let device = &ctx.device;
    let size = 512u32;
    let format = vk::Format::R16G16_SFLOAT;

    // Create image
    let image = unsafe {
        device
            .create_image(
                &vk::ImageCreateInfo::default()
                    .image_type(vk::ImageType::TYPE_2D)
                    .format(format)
                    .extent(vk::Extent3D {
                        width: size,
                        height: size,
                        depth: 1,
                    })
                    .mip_levels(1)
                    .array_layers(1)
                    .samples(vk::SampleCountFlags::TYPE_1)
                    .tiling(vk::ImageTiling::OPTIMAL)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
                    .sharing_mode(vk::SharingMode::EXCLUSIVE),
                None,
            )
            .unwrap()
    };

    let mem_reqs = unsafe { device.get_image_memory_requirements(image) };
    let memory = unsafe {
        let mem_type = find_memory_type(
            &ctx.instance,
            ctx.physical_device,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        );
        device
            .allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(mem_reqs.size)
                    .memory_type_index(mem_type),
                None,
            )
            .unwrap()
    };
    unsafe { device.bind_image_memory(image, memory, 0).unwrap() };

    let view = unsafe {
        device
            .create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        base_mip_level: 0,
                        level_count: 1,
                        base_array_layer: 0,
                        layer_count: 1,
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
                    .max_lod(0.0)
                    .max_anisotropy(1.0)
                    .border_color(vk::BorderColor::FLOAT_OPAQUE_WHITE),
                None,
            )
            .unwrap()
    };

    // Create offscreen render pass
    let render_pass = unsafe {
        let color_attachment = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);

        let color_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);

        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_ref));

        device
            .create_render_pass(
                &vk::RenderPassCreateInfo::default()
                    .attachments(std::slice::from_ref(&color_attachment))
                    .subpasses(std::slice::from_ref(&subpass)),
                None,
            )
            .unwrap()
    };

    // Create framebuffer
    let framebuffer = unsafe {
        device
            .create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(std::slice::from_ref(&view))
                    .width(size)
                    .height(size)
                    .layers(1),
                None,
            )
            .unwrap()
    };

    // Create pipeline
    let vert_code = include_bytes!("../../shaders/brdf_lut.vert.spv");
    let frag_code = include_bytes!("../../shaders/brdf_lut.frag.spv");

    let vert_module = create_shader_module(device, vert_code);
    let frag_module = create_shader_module(device, frag_code);

    let shader_stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vert_module)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(frag_module)
            .name(c"main"),
    ];

    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST)
        .primitive_restart_enable(false);

    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state =
        vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);

    let rasterizer = vk::PipelineRasterizationStateCreateInfo::default()
        .depth_clamp_enable(false)
        .rasterizer_discard_enable(false)
        .polygon_mode(vk::PolygonMode::FILL)
        .line_width(1.0)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .depth_bias_enable(false);

    let multisampling = vk::PipelineMultisampleStateCreateInfo::default()
        .sample_shading_enable(false)
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);

    let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .color_write_mask(
            vk::ColorComponentFlags::R
                | vk::ColorComponentFlags::G
                | vk::ColorComponentFlags::B
                | vk::ColorComponentFlags::A,
        )
        .blend_enable(false);

    let color_blending = vk::PipelineColorBlendStateCreateInfo::default()
        .logic_op_enable(false)
        .attachments(std::slice::from_ref(&color_blend_attachment));

    let pipeline_layout = unsafe {
        device
            .create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None)
            .unwrap()
    };

    let pipeline = unsafe {
        device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(
                    &vk::GraphicsPipelineCreateInfo::default()
                        .stages(&shader_stages)
                        .vertex_input_state(&vertex_input)
                        .input_assembly_state(&input_assembly)
                        .viewport_state(&viewport_state)
                        .rasterization_state(&rasterizer)
                        .multisample_state(&multisampling)
                        .color_blend_state(&color_blending)
                        .dynamic_state(&dynamic_state)
                        .layout(pipeline_layout)
                        .render_pass(render_pass)
                        .subpass(0),
                ),
                None,
            )
            .unwrap()[0]
    };

    // Render
    with_one_time_command(ctx, command_pool, |cmd| unsafe {
        // Transition image to COLOR_ATTACHMENT_OPTIMAL
        let barrier = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            })
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            std::slice::from_ref(&barrier),
        );

        device.cmd_begin_render_pass(
            cmd,
            &vk::RenderPassBeginInfo::default()
                .render_pass(render_pass)
                .framebuffer(framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent: vk::Extent2D {
                        width: size,
                        height: size,
                    },
                })
                .clear_values(&[vk::ClearValue {
                    color: vk::ClearColorValue {
                        float32: [0.0, 0.0, 0.0, 1.0],
                    },
                }]),
            vk::SubpassContents::INLINE,
        );

        let viewport = vk::Viewport::default()
            .x(0.0)
            .y(0.0)
            .width(size as f32)
            .height(size as f32)
            .min_depth(0.0)
            .max_depth(1.0);
        let scissor = vk::Rect2D::default().extent(vk::Extent2D {
            width: size,
            height: size,
        });
        device.cmd_set_viewport(cmd, 0, std::slice::from_ref(&viewport));
        device.cmd_set_scissor(cmd, 0, std::slice::from_ref(&scissor));

        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
        device.cmd_draw(cmd, 3, 1, 0, 0);

        device.cmd_end_render_pass(cmd);
    });

    // Cleanup temp resources
    unsafe {
        device.destroy_pipeline(pipeline, None);
        device.destroy_pipeline_layout(pipeline_layout, None);
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);
        device.destroy_framebuffer(framebuffer, None);
        device.destroy_render_pass(render_pass, None);
    }

    BrdfLut {
        image,
        memory,
        view,
        sampler,
    }
}

fn create_shader_module(device: &ash::Device, code: &[u8]) -> vk::ShaderModule {
    assert_eq!(code.len() % 4, 0);
    let code_words: Vec<u32> = code
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    unsafe {
        device
            .create_shader_module(
                &vk::ShaderModuleCreateInfo::default().code(&code_words),
                None,
            )
            .unwrap()
    }
}
