use ash::vk;

/// A postprocess effect rendered as a fullscreen-triangle pass.
///
/// Each pass is self-contained: it holds a pipeline + layout, knows its
/// render pass, and records itself with a standard viewport/scissor preamble.
///
/// The caller provides the framebuffer and descriptor sets; the trait
/// implementation handles the render-pass begin/end, pipeline binding,
/// and draw call.
#[allow(dead_code)]
pub trait PostProcessPass: Send + Sync {
    /// Debug-label name for this pass (e.g. "Bright Pass").
    fn name(&self) -> &'static str;

    /// The render pass this pass writes into.
    fn render_pass(&self) -> vk::RenderPass;

    /// The graphics pipeline for the fullscreen-triangle draw.
    fn pipeline(&self) -> vk::Pipeline;

    /// Pipeline layout matching the `set 0 / set 1` descriptor convention
    /// (set 0 = input samplers, set 1 = postprocess UBO).
    fn pipeline_layout(&self) -> vk::PipelineLayout;

    /// Record the pass into the given command buffer.
    ///
    /// The implementation must:
    /// 1. `cmd_begin_render_pass` with the provided framebuffer.
    /// 2. Set the viewport (Y-flip) and scissor matching `extent`.
    /// 3. Bind the pipeline and `descriptor_sets`.
    /// 4. Push any push constants (optional).
    /// 5. `cmd_draw(3, 1, 0, 0)`.
    /// 6. `cmd_end_render_pass`.
    ///
    /// `descriptor_sets` must contain exactly the sets declared by the
    /// pipeline layout, in order (set 0 first, then set 1).
    unsafe fn record(
        &self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
        extent: vk::Extent2D,
        descriptor_sets: &[vk::DescriptorSet],
    );

    /// Record a clear-less pass. Same as `record` but uses a clear value of
    /// `DONT_CARE` / zero (useful when the attachment already has content and
    /// the pass writes every pixel).
    unsafe fn record_dont_care(
        &self,
        device: &ash::Device,
        cmd: vk::CommandBuffer,
        framebuffer: vk::Framebuffer,
        extent: vk::Extent2D,
        descriptor_sets: &[vk::DescriptorSet],
    ) {
        unsafe {
            self.record(device, cmd, framebuffer, extent, descriptor_sets);
        }
    }
}

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
