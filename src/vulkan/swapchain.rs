use ash::vk;

pub struct SwapchainData {
    pub swapchain: vk::SwapchainKHR,
    pub swapchain_loader: ash::khr::swapchain::Device,
    #[allow(dead_code)]
    pub images: Vec<vk::Image>,
    pub image_views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    pub extent: vk::Extent2D,
    #[allow(dead_code)]
    pub image_format: vk::Format,
    pub depth_image: vk::Image,
    pub depth_memory: vk::DeviceMemory,
    pub depth_view: vk::ImageView,
    pub depth_format: vk::Format,
}

pub fn find_depth_format(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> vk::Format {
    let candidates = [
        vk::Format::D32_SFLOAT,
        vk::Format::D24_UNORM_S8_UINT,
        vk::Format::D32_SFLOAT_S8_UINT,
    ];
    for &format in &candidates {
        let props =
            unsafe { instance.get_physical_device_format_properties(physical_device, format) };
        if props
            .optimal_tiling_features
            .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT)
        {
            return format;
        }
    }
    panic!("Failed to find supported depth format")
}

pub fn create_swapchain(
    instance: &ash::Instance,
    device: &ash::Device,
    surface_loader: &ash::khr::surface::Instance,
    swapchain_loader: &ash::khr::swapchain::Device,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
    window_width: u32,
    window_height: u32,
    render_pass: vk::RenderPass,
) -> SwapchainData {
    let caps = unsafe {
        surface_loader
            .get_physical_device_surface_capabilities(physical_device, surface)
            .unwrap()
    };

    let formats = unsafe {
        surface_loader
            .get_physical_device_surface_formats(physical_device, surface)
            .unwrap()
    };
    let present_modes = unsafe {
        surface_loader
            .get_physical_device_surface_present_modes(physical_device, surface)
            .unwrap()
    };

    let surface_format = formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_SRGB
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .copied()
        .unwrap_or(formats[0]);

    let present_mode = present_modes
        .iter()
        .copied()
        .find(|&m| m == vk::PresentModeKHR::MAILBOX)
        .unwrap_or(vk::PresentModeKHR::FIFO);

    let extent = if caps.current_extent.width != u32::MAX {
        caps.current_extent
    } else {
        vk::Extent2D {
            width: window_width.clamp(caps.min_image_extent.width, caps.max_image_extent.width),
            height: window_height.clamp(caps.min_image_extent.height, caps.max_image_extent.height),
        }
    };

    let image_count = caps.min_image_count + 1;
    let image_count = if caps.max_image_count > 0 {
        image_count.min(caps.max_image_count)
    } else {
        image_count
    };

    let create_info = vk::SwapchainCreateInfoKHR::default()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(surface_format.format)
        .image_color_space(surface_format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .pre_transform(caps.current_transform)
        .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
        .present_mode(present_mode)
        .clipped(true)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE);

    let swapchain = unsafe {
        swapchain_loader
            .create_swapchain(&create_info, None)
            .unwrap()
    };

    let images = unsafe { swapchain_loader.get_swapchain_images(swapchain).unwrap() };

    let image_views: Vec<_> = images
        .iter()
        .map(|&image| {
            let create_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(surface_format.format)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            unsafe { device.create_image_view(&create_info, None).unwrap() }
        })
        .collect();

    let depth_format = find_depth_format(instance, physical_device);

    let depth_image = {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .format(depth_format)
            .tiling(vk::ImageTiling::OPTIMAL)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .samples(vk::SampleCountFlags::TYPE_1);
        unsafe { device.create_image(&info, None).unwrap() }
    };

    let depth_memory = {
        let mem_reqs = unsafe { device.get_image_memory_requirements(depth_image) };
        let mem_props = unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let mut mem_type_index = u32::MAX;
        for i in 0..mem_props.memory_type_count {
            if (mem_reqs.memory_type_bits & (1 << i)) != 0
                && mem_props.memory_types[i as usize]
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            {
                mem_type_index = i;
                break;
            }
        }
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type_index);
        let mem = unsafe { device.allocate_memory(&alloc_info, None).unwrap() };
        unsafe { device.bind_image_memory(depth_image, mem, 0).unwrap() };
        mem
    };

    let depth_view = {
        let aspect = if depth_format == vk::Format::D32_SFLOAT {
            vk::ImageAspectFlags::DEPTH
        } else {
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        };
        let info = vk::ImageViewCreateInfo::default()
            .image(depth_image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(depth_format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: aspect,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        unsafe { device.create_image_view(&info, None).unwrap() }
    };

    let framebuffers: Vec<_> = image_views
        .iter()
        .map(|&view| {
            let attachments = [view, depth_view];
            let create_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(&attachments)
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            unsafe { device.create_framebuffer(&create_info, None).unwrap() }
        })
        .collect();

    SwapchainData {
        swapchain,
        swapchain_loader: swapchain_loader.clone(),
        images,
        image_views,
        framebuffers,
        extent,
        image_format: surface_format.format,
        depth_image,
        depth_memory,
        depth_view,
        depth_format,
    }
}

pub fn cleanup_swapchain(device: &ash::Device, data: &mut SwapchainData) {
    unsafe {
        device.destroy_image_view(data.depth_view, None);
        device.destroy_image(data.depth_image, None);
        device.free_memory(data.depth_memory, None);
        for &fb in &data.framebuffers {
            device.destroy_framebuffer(fb, None);
        }
        for &view in &data.image_views {
            device.destroy_image_view(view, None);
        }
        data.swapchain_loader
            .destroy_swapchain(data.swapchain, None);
    }
}
