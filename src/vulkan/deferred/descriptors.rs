use ash::vk;

use crate::vulkan::deferred::resources::GBUFFER_INPUT_COUNT;

/// Set 1 of the deferred lighting pipeline: the four G-buffer inputs.
///
/// | Binding | Content                                        |
/// |---------|------------------------------------------------|
/// | 0       | `uGAlbedo`   — `.rgb` albedo, `.a` occlusion    |
/// | 1       | `uGNormal`   — `.xyz` world normal, `.a` roughness |
/// | 2       | `uGEmissive` — `.rgb` emissive, `.a` metallic   |
/// | 3       | `uGDepth`    — depth buffer                    |
pub fn create_gbuffer_input_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let bindings: Vec<_> = (0..GBUFFER_INPUT_COUNT as u32)
        .map(|binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        })
        .collect();
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe { device.create_descriptor_set_layout(&info, None).unwrap() }
}
