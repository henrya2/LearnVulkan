pub mod descriptors;
pub mod fullscreen;
pub mod pass_trait;
pub mod passes;
pub mod pyramid;
pub mod resources;
pub mod ubo;

// Public API used by renderer.rs
pub use pyramid::BloomPyramid;
pub use resources::PostProcessResources;
pub use ubo::BlurPushConstants;

// Re-export the trait for downstream consumers implementing custom passes.
// The `#[allow(unused_imports)]` is because no implementor exists yet.
#[allow(unused_imports)]
pub use pass_trait::PostProcessPass;
