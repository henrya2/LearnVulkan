//! Deferred rendering path.
//!
//! The forward path shades every surface pixel inside the geometry pass
//! (`pbr.frag`). The deferred path splits that into two passes:
//!
//! 1. **G-buffer pass** (`gbuffer.frag`, vertex stage `pbr.vert`) writes the
//!    surface parameters into three `RGBA16F` attachments plus the existing
//!    depth buffer — no shading at all.
//! 2. **Deferred lighting pass** (`deferred.frag`, fullscreen triangle)
//!    reconstructs the world position from depth via `inv_view_proj`, samples
//!    the G-buffer, and runs the same GGX + IBL shading into the existing HDR
//!    scene-color image. Background pixels (depth == 1.0) are filled from the
//!    environment cubemap, so no skybox geometry draw is needed.
//!
//! Because the lighting pass writes the same HDR scene-color image as the
//! forward path, the bloom + composite postprocess chain is reused verbatim.
//!
//! The pipeline layouts deliberately reuse the renderer's **global** set
//! (set 0: global UBO, material buffer, IBL) so the lighting pass only adds
//! its own set 1 (the G-buffer inputs).

pub mod descriptors;
pub mod passes;
pub mod resources;

pub use resources::{DeferredDebugView, GBufferResources};
