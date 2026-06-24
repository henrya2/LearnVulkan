use ash::vk;

use crate::vulkan::memory::{MemoryAllocator, OwnedImage};

pub struct SwapchainData {
    pub swapchain: vk::SwapchainKHR,
    pub swapchain_loader: ash::khr::swapchain::Device,
    pub images: Vec<vk::Image>,
    pub image_views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    pub extent: vk::Extent2D,
    pub image_format: vk::Format,
    pub image_color_space: vk::ColorSpaceKHR,
    pub depth: OwnedImage,
    pub depth_view: vk::ImageView,
    #[allow(dead_code)]
    pub depth_format: vk::Format,
}

pub fn select_surface_format(
    surface_loader: &ash::khr::surface::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> vk::SurfaceFormatKHR {
    let formats = unsafe {
        surface_loader
            .get_physical_device_surface_formats(physical_device, surface)
            .unwrap()
    };

    if formats.len() == 1 && formats[0].format == vk::Format::UNDEFINED {
        return vk::SurfaceFormatKHR {
            format: vk::Format::B8G8R8A8_SRGB,
            color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
        };
    }

    formats
        .iter()
        .find(|f| {
            f.format == vk::Format::B8G8R8A8_SRGB
                && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR
        })
        .copied()
        .unwrap_or(formats[0])
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
    device: &ash::Device,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    allocator: &mut MemoryAllocator,
    surface_loader: &ash::khr::surface::Instance,
    swapchain_loader: &ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    window_width: u32,
    window_height: u32,
    composite_render_pass: vk::RenderPass,
    surface_format: vk::SurfaceFormatKHR,
) -> SwapchainData {
    let caps = unsafe {
        surface_loader
            .get_physical_device_surface_capabilities(physical_device, surface)
            .unwrap()
    };

    let present_modes = unsafe {
        surface_loader
            .get_physical_device_surface_present_modes(physical_device, surface)
            .unwrap()
    };

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

    let depth_info = vk::ImageCreateInfo::default()
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
    // Depth is recreated on every swapchain resize — use a dedicated block
    // so resize churn does not perturb the sub-allocator pool.
    let depth = allocator
        .create_dedicated_image(device, "SwapchainDepth", &depth_info);

    let depth_view = {
        let aspect = if depth_format == vk::Format::D32_SFLOAT {
            vk::ImageAspectFlags::DEPTH
        } else {
            vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
        };
        let info = vk::ImageViewCreateInfo::default()
            .image(depth.image)
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
            // The composite render pass has only a color attachment (the
            // swapchain image), so the framebuffer must have only 1
            // attachment too.
            let attachments = [view];
            unsafe { device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(composite_render_pass)
                    .attachments(&attachments)
                    .width(extent.width)
                    .height(extent.height)
                    .layers(1),
                None,
            ).unwrap() }
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
        image_color_space: surface_format.color_space,
        depth,
        depth_view,
        depth_format,
    }
}

pub fn cleanup_swapchain(device: &ash::Device, data: &mut SwapchainData, allocator: &mut MemoryAllocator) {
    unsafe {
        for &fb in &data.framebuffers {
            device.destroy_framebuffer(fb, None);
        }
        device.destroy_image_view(data.depth_view, None);
        // Depth image + its dedicated allocation.
        data.depth.destroy(device, allocator);
        for &view in &data.image_views {
            device.destroy_image_view(view, None);
        }
        data.swapchain_loader
            .destroy_swapchain(data.swapchain, None);
    }
}
