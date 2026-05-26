# Code Review 2026-05-26: LearnVulkan PBR Renderer

## Summary

The renderer is a well-structured Vulkan learning project with a solid PBR/glTF path, correct resource ownership boundaries, and unusually thorough coordinate-system documentation. The highest-impact glTF and shader correctness issues from the earlier rendering review have already been addressed. The main remaining short-term work is maintenance-oriented: keep the render pass format tied to the actual swapchain format, remove stale legacy shader/pipeline paths, and make persistent UBO mapping cleanup explicit.

## Current strengths

- Clean module split between app/window handling, camera/input, scene loading, and Vulkan resource management.
- Correct `App` drop ordering: `Renderer` is destroyed before `VulkanContext`, so device-level objects are released before the Vulkan device.
- `VK_EXT_debug_utils` is enabled independently of validation layers, so RenderDoc labels and object names work in release captures.
- Synchronization uses per-frame image-available semaphores and per-swapchain-image render-finished semaphores, avoiding semaphore reuse hazards.
- Swapchain recreation handles resize, `ERROR_OUT_OF_DATE_KHR`, and both success/error forms of `SUBOPTIMAL_KHR`.
- The glTF loader handles default-scene traversal, explicit default material fallback, semantic texture formats, and RH-to-LH conversion.
- The shader writes linear tone-mapped color to an sRGB swapchain attachment; no manual gamma correction is applied.

## glTF/PBR correctness status

The high-impact glTF rendering issues from the previous review are fixed:

- `Texture::from_rgba8_with_format` supports explicit Vulkan formats.
- Base-color and emissive textures upload as `R8G8B8A8_SRGB`.
- Normal, metallic-roughness, and occlusion textures upload as `R8G8B8A8_UNORM`.
- Fallback textures preserve material semantics: white sRGB, white linear, black sRGB, flat linear normal, and white linear metallic-roughness.
- Primitives without explicit materials use an explicit glTF default material instead of accidentally using material 0.
- The loader traverses the default scene, or scene 0 when no default scene is declared.
- Missing normals on non-indexed primitives use synthesized indices for normal generation.
- Metallic and roughness are clamped.
- Occlusion strength is applied with glTF `mix(1.0, ao, strength)` semantics.
- TBN construction re-orthogonalizes tangent against normal.
- The simplified environment term uses roughness-aware Fresnel.

The remaining visual limitation is IBL quality. The current environment map is a synthetic 2D LDR gradient, useful as a placeholder but not physically based split-sum IBL.

## Issues fixed in this pass

### 1. Render pass format must match the selected swapchain format

The old code created the render pass with hardcoded `B8G8R8A8_SRGB`, while swapchain creation could fall back to the first advertised surface format. That created a possible framebuffer/render-pass attachment format mismatch on devices that do not expose the preferred format.

Fix direction: select the surface format once and use that same format for both render pass creation and swapchain image/view creation. On swapchain recreation, keep using the existing swapchain image format and color space so the existing render pass remains compatible.

### 2. Swapchain cleanup ordering

Framebuffers reference swapchain image views and the depth view. Cleanup should destroy framebuffers before destroying the views they contain.

Fix direction: destroy framebuffers first, then depth resources, then swapchain image views, then the swapchain.

### 3. Legacy scene pipeline and shader references

The PBR renderer no longer uses the legacy cube/floor graphics pipeline. Keeping the unused `create_pipeline` function also kept stale `scene.vert.spv` and `scene.frag.spv` `include_bytes!` dependencies. The shader sources no longer exist, and the compile script still referenced them.

Fix direction: remove the unused legacy pipeline function, remove stale scene shader compile commands, and update active docs to describe only the PBR shader path.

### 4. Persistently mapped UBO cleanup

The global UBO buffers are persistently mapped and then freed during renderer teardown. Vulkan allows mapped memory to be implicitly unmapped when freed, but explicit `vkUnmapMemory` is clearer and avoids stale raw-pointer state during teardown.

Fix direction: unmap each global UBO memory allocation before destroying the buffer and freeing the memory.

## Findings intentionally not changed

### `SwapchainData.images` should stay

The original review suggested removing `SwapchainData.images`, but that field is useful in this codebase. It is used to size per-swapchain-image semaphores and `images_in_flight`, and to assign RenderDoc names to swapchain images. It should not be removed unless those uses are replaced with an explicit image count and image-name path.

### `queue_present` already handles `SUBOPTIMAL_KHR`

The present path correctly handles both `Ok(suboptimal_present)` and `Err(vk::Result::SUBOPTIMAL_KHR)`. No change is needed.

### Host-coherent UBO writes do not need a manual flush

The per-frame global UBO memory is allocated with `HOST_VISIBLE | HOST_COHERENT`, so the current `memcpy` after the frame fence wait is valid without an explicit flush.

## Future work

### Rendering quality

1. Replace the synthetic LDR environment texture with full HDR split-sum IBL:
   - HDR equirectangular loading.
   - Equirectangular-to-cubemap conversion.
   - Diffuse irradiance convolution.
   - GGX prefiltered specular cubemap.
   - BRDF integration LUT.
2. Replace the shader's hardcoded `MAX_REFLECTION_LOD` with data derived from the actual environment map mip count.
3. Add glTF alpha modes (`MASK`, `BLEND`) and double-sided material support.
4. Add glTF sampler-state mapping and texture coordinate set selection for slots such as occlusion.
5. Consider anisotropic filtering after explicitly querying and enabling `samplerAnisotropy`.

### Scalability and performance

1. Move material data from the fixed-size UBO array to a storage buffer for larger scenes.
2. Integrate a Vulkan memory allocator or suballocator to avoid one raw allocation per buffer/image.
3. Batch startup uploads instead of submitting and waiting for each one-time command buffer.
4. Cache or gate debug label strings if scenes grow to hundreds of meshes.
5. Add GPU timestamp queries for profiling.

### Maintainability

1. Add lightweight tests for pure math and glTF loading assumptions.
2. Add screenshot or render regression tests only if the project grows beyond learning/demo scope.
3. Consider making the glTF asset path configurable once more than one model is needed.

## Overall assessment

The renderer is in good shape for an educational Vulkan PBR project. The glTF correctness pass addressed the most important visual and color-space issues. The remaining work is mostly about robustness, stale-code removal, and future scalability rather than current correctness blockers.
