use ash::vk;

use crate::vulkan::brdf_lut::{BrdfLut, generate_brdf_lut};
use crate::vulkan::context::VulkanContext;
use crate::vulkan::cubemap::Cubemap;
use crate::vulkan::ktx2_loader::load_ktx2_cubemap;

pub struct IblResources {
    pub env_cubemap: Cubemap,
    pub irradiance_map: Cubemap,
    pub prefilter_map: Cubemap,
    pub brdf_lut: BrdfLut,
}

impl IblResources {
    pub fn load(ctx: &VulkanContext, command_pool: vk::CommandPool, env_base_path: &str) -> Self {
        let env_cubemap = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/lambertian/outputCubeMap.ktx2", env_base_path),
        );
        let irradiance_map = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/lambertian/diffuse.ktx2", env_base_path),
        );
        let prefilter_map = load_ktx2_cubemap(
            ctx,
            command_pool,
            &format!("{}/ggx/specular.ktx2", env_base_path),
        );
        let brdf_lut = generate_brdf_lut(ctx, command_pool);

        Self {
            env_cubemap,
            irradiance_map,
            prefilter_map,
            brdf_lut,
        }
    }

    pub unsafe fn destroy(&self, device: &ash::Device) {
        unsafe {
            self.env_cubemap.destroy(device);
            self.irradiance_map.destroy(device);
            self.prefilter_map.destroy(device);
            self.brdf_lut.destroy(device);
        }
    }
}
