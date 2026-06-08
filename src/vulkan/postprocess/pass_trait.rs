use ash::vk;

/// Helper: set the project's standard Y-flip viewport and scissor for the
/// given extent, then bind the pipeline. The caller is responsible for
/// binding descriptor sets and issuing the draw call after this function
/// returns. This ensures descriptor sets are always bound before the draw.
pub unsafe fn set_viewport_and_bind_pipeline(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    extent: vk::Extent2D,
    pipeline: vk::Pipeline,
) {
    unsafe {
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

        device.cmd_set_viewport(cmd, 0, std::slice::from_ref(&viewport));
        device.cmd_set_scissor(cmd, 0, std::slice::from_ref(&scissor));
        device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipeline);
    }
}