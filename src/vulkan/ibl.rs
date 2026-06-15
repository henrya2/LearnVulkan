use ash::vk;

use crate::vulkan::brdf_lut::{BrdfLut, generate_brdf_lut};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::cubemap::Cubemap;
use crate::vulkan::ktx2_loader::load_ktx2_cubemap;
use crate::vulkan::memory::MemoryAllocator;

pub struct IblResources {
    pub env_cubemap: Cubemap,
    pub irradiance_map: Cubemap,
    pub prefilter_map: Cubemap,
    pub brdf_lut: BrdfLut,
}

impl IblResources {
    pub fn load(ctx: &mut VulkanContext, command_pool: vk::CommandPool, env_base_path: &str) -> Self {
        // Env + prefilter are large cubemaps; use dedicated blocks so the
        // sub-allocator pool is not perturbed. Irradiance is small enough
        // for managed sub-allocation.
        let env_cubemap = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/lambertian/outputCubeMap.ktx2", env_base_path),
            "EnnisEnvCubemap",
            true,
        );
        let irradiance_map = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/lambertian/diffuse.ktx2", env_base_path),
            "EnnisIrradianceMap",
            false,
        );
        let prefilter_map = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/ggx/specular.ktx2", env_base_path),
            "EnnisPrefilterMap",
            true,
        );
        let brdf_lut = generate_brdf_lut(ctx, command_pool);

        Self {
            env_cubemap,
            irradiance_map,
            prefilter_map,
            brdf_lut,
        }
    }

    pub unsafe fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe {
            self.env_cubemap.destroy(device, allocator);
            self.irradiance_map.destroy(device, allocator);
            self.prefilter_map.destroy(device, allocator);
            self.brdf_lut.destroy(device, allocator);
        }
    }
}
