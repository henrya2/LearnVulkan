#![allow(unsafe_op_in_unsafe_fn)]

use ash::vk;

use crate::vulkan::buffer::find_memory_type;
use crate::vulkan::context::VulkanContext;

/// Number of mip levels in the bloom pyramid.
pub const BLOOM_MIP_COUNT: usize = 8;

/// 16-bit float per-channel color, supporting color attachment + sampling + blit.
pub const BLOOM_FORMAT: vk::Format = vk::Format::R16G16B16A16_SFLOAT;

/// A bloom pyramid built from two single images with mip levels:
/// `mip_image` holds the bloom mip chain; `temp_image` holds the intermediate
/// blur results. Each mip level has its own `vk::ImageView` (alias into the
/// parent image at `base_mip_level = i`), so the rest of the code doesn't
/// change — it just indexes into `mip_views[i]` / `temp_views[i]`.
///
/// Using a single image with mip levels instead of 16 separate images reduces
/// allocation count, memory fragmentation, and layout-transition barriers
/// (one barrier with `level_count = BLOOM_MIP_COUNT` covers the whole chain).
pub struct BloomPyramid {
    pub mip_views: Vec<vk::ImageView>,
    pub temp_views: Vec<vk::ImageView>,
    mip_image: vk::Image,
    mip_memory: vk::DeviceMemory,
    temp_image: vk::Image,
    temp_memory: vk::DeviceMemory,
    pub sampler: vk::Sampler,
}

impl BloomPyramid {
    pub fn new(ctx: &VulkanContext, width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "bloom extent must be positive");
        let device = &ctx.device;

        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
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
        let sampler = unsafe { device.create_sampler(&sampler_info, None).unwrap() };

        let (mip_image, mip_memory) =
            create_mip_image(device, ctx, width, height, BLOOM_MIP_COUNT as u32);
        let (temp_image, temp_memory) =
            create_mip_image(device, ctx, width, height, BLOOM_MIP_COUNT as u32);

        let mut mip_views = Vec::with_capacity(BLOOM_MIP_COUNT);
        let mut temp_views = Vec::with_capacity(BLOOM_MIP_COUNT);
        for level in 0..BLOOM_MIP_COUNT {
            mip_views.push(create_mip_view(device, mip_image, level as u32));
            temp_views.push(create_mip_view(device, temp_image, level as u32));
        }

        Self {
            mip_views,
            temp_views,
            mip_image,
            mip_memory,
            temp_image,
            temp_memory,
            sampler,
        }
    }

    /// Return the `(width, height)` of a given mip level.
    pub fn mip_extent(width: u32, height: u32, level: usize) -> (u32, u32) {
        let w = (width >> level).max(1);
        let h = (height >> level).max(1);
        (w, h)
    }

    pub fn mip_count() -> usize {
        BLOOM_MIP_COUNT
    }

    /// Return the underlying mip image handle, useful for barriers.
    pub fn mip_image(&self) -> vk::Image {
        self.mip_image
    }

    /// Return the underlying temp image handle, useful for barriers.
    pub fn temp_image(&self) -> vk::Image {
        self.temp_image
    }

    pub unsafe fn destroy(&mut self, device: &ash::Device) {
        unsafe {
            for view in self.mip_views.drain(..) {
                device.destroy_image_view(view, None);
            }
            for view in self.temp_views.drain(..) {
                device.destroy_image_view(view, None);
            }
            device.destroy_image(self.mip_image, None);
            device.free_memory(self.mip_memory, None);
            device.destroy_image(self.temp_image, None);
            device.free_memory(self.temp_memory, None);
            device.destroy_sampler(self.sampler, None);
        }
    }
}

fn create_mip_image(
    device: &ash::Device,
    ctx: &VulkanContext,
    width: u32,
    height: u32,
    mip_levels: u32,
) -> (vk::Image, vk::DeviceMemory) {
    let extent = vk::Extent3D {
        width,
        height,
        depth: 1,
    };
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .extent(extent)
        .mip_levels(mip_levels)
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
    let mem_type = find_memory_type(
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

    (image, memory)
}

fn create_mip_view(device: &ash::Device, image: vk::Image, level: u32) -> vk::ImageView {
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(BLOOM_FORMAT)
        .subresource_range(vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: level,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        });
    unsafe { device.create_image_view(&view_info, None).unwrap() }
}
