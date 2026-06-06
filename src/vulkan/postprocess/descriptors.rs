use ash::vk;

/// 1 UBO at set 2. Used by bright, blur, and composite pipelines.
pub fn create_postprocess_ubo_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .bindings(std::slice::from_ref(&binding));
    unsafe { device.create_descriptor_set_layout(&info, None).unwrap() }
}

/// 1 combined image sampler at set 0. Used by bright and blur pipelines.
pub fn create_single_input_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let info = vk::DescriptorSetLayoutCreateInfo::default()
        .bindings(std::slice::from_ref(&binding));
    unsafe { device.create_descriptor_set_layout(&info, None).unwrap() }
}

/// 1 scene color + 8 bloom mip samples = 9 bindings at set 0. Used by the
/// composite pipeline.
pub fn create_composite_input_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
    let mut bindings = Vec::with_capacity(9);
    for binding in 0..9 {
        bindings.push(
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        );
    }
    let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    unsafe { device.create_descriptor_set_layout(&info, None).unwrap() }
}
