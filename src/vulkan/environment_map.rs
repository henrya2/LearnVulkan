use ash::vk;

use crate::vulkan::context::VulkanContext;
use crate::vulkan::texture::Texture;

pub fn create_synthetic_environment_map(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
) -> Texture {
    let width = 256u32;
    let height = 128u32;
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);

    for y in 0..height {
        let v = y as f32 / (height.saturating_sub(1).max(1)) as f32;
        for _x in 0..width {
            // Studio-like soft gradient: brighter warm at top, darker at bottom.
            let intensity = 0.4 + 0.6 * v;
            let r = (intensity.min(1.0) * 255.0) as u8;
            let g = (intensity.min(1.0) * 0.95 * 255.0) as u8;
            let b = (intensity.min(1.0) * 0.9 * 255.0) as u8;
            pixels.push(r);
            pixels.push(g);
            pixels.push(b);
            pixels.push(255);
        }
    }

    Texture::from_rgba8_with_format(
        ctx,
        command_pool,
        &pixels,
        width,
        height,
        vk::Format::R8G8B8A8_UNORM,
    )
}
