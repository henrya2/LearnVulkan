# LearnVulkan

A Vulkan PBR renderer written in Rust. It loads and renders glTF 2.0 models (DamagedHelmet) with metallic-roughness PBR shading, normal mapping, and image-based lighting from the Ennis glTF sample environment (KTX2 cubemaps under `assets/environment_map/ennis/`), in an 800x600 window using raw Vulkan bindings (`ash`).

## Features

- **glTF 2.0 model loading** via the `gltf` crate (DamagedHelmet from KhronosGroup/glTF-Sample-Models)
- **PBR metallic-roughness shading** with Cook-Torrance BRDF (GGX/Trowbridge-Reitz NDF, Smith geometry, Schlick Fresnel)
- **Normal mapping** with TBN tangent-space construction
- **Per-material textures**: base color, metallic-roughness, normal, occlusion, emissive (with semantic-specific 1x1 fallbacks)
- **glTF texture color-space handling**: base color/emissive are uploaded as sRGB; normal/metallic-roughness/occlusion are uploaded as linear UNORM
- **Image-based lighting (IBL)**: env cubemap + irradiance + GGX prefilter from the Ennis glTF sample environment (KTX2 in `assets/environment_map/ennis/`), plus a procedurally generated BRDF LUT
- **Per-frame uniform buffer** for view/proj/camera/light data (descriptor set, no push constants for globals)
- **Per-draw push constants** for model matrix + material index (80 B)
- **Free-fly FPS camera**:
  - Mouse look (pitch/yaw)
  - WASD movement
  - Space / LShift for vertical movement
  - Click-to-lock cursor behavior
- **Clean Vulkan bring-up** with validation layers in debug builds, plus opt-in validation in non-debug builds with `--validation` / `--validate`
- **RenderDoc-friendly debug markers** via `VK_EXT_debug_utils` in all builds: labeled frame/render-pass/mesh regions and named GPU resources
- **HDR bloom**: 8-mip separable Gaussian blur with Frostbite-style soft knee threshold
- **Runtime-switchable tonemapping**: Linear / Reinhard / ACES with stops-based exposure control
- **Skybox rendering** with the environment cubemap (`LESS_OR_EQUAL` depth, depth writes disabled)
- **Fullscreen-triangle postprocessing framework**: extensible design with shared vertex shader and per-effect fragment shaders

## Tech Stack

| Component | Crate / Tool |
|-----------|-------------|
| Vulkan bindings | `ash` 0.38 |
| Windowing | `winit` 0.30 (ApplicationHandler trait) |
| Surface bridge | `raw-window-handle` 0.6 + `ash-window` 0.13 |
| Math | `glam` 0.32 (left-handed, Y-up) |
| Buffer uploads | `bytemuck` |
| Image loading | `image` 0.25 (PNG + JPEG) |
| Model loading | `gltf` 1.4 (import + utils) |
| KTX2 loading | `ktx2` 0.3 |
| Shaders | GLSL compiled offline to SPIR-V via `glslc` |

## Build & Run

**Prerequisites:**
- [Vulkan SDK](https://vulkan.lunarg.com/) installed so `glslc` is on `PATH`.
- `assets/models/DamagedHelmet/` present with the glTF model and textures.
- `assets/environment_map/ennis/` present with the KTX2 cubemaps (env/irradiance/prefilter).

```bash
# Compile shaders
cd shaders && ./compile.bat
cd ..

# Build & run
cargo run

# Run a release build with Vulkan validation layers enabled
cargo run --release -- --validation
```

## Controls

| Input | Action |
|-------|--------|
| `W` / `A` / `S` / `D` | Move forward / left / back / right |
| `Space` | Move up |
| `LShift` | Move down |
| Mouse | Look around |
| Left click | Lock & hide cursor |
| `Alt` + `Z` | Release cursor lock |
| `Alt`+`Tab` / focus loss | Auto-release cursor lock |

## Project Structure

```
LearnVulkan/
├── Cargo.toml
├── CODEBUDDY.md               # Project guidance for CodeBuddy
├── assets/
│   ├── gen_texture.py         # uv-runnable placeholder PNG generator (PEP-723)
│   ├── texture.png            # Legacy placeholder texture
│   ├── models/
│   │   └── DamagedHelmet/     # glTF model with PBR textures
│   └── environment_map/
│       └── ennis/             # KTX2 cubemaps for IBL (lambertian/, ggx/)
├── docs/
│   ├── learn_vulkan_plan.md   # Original triangle plan
│   ├── vulkan_fps_plan.md     # FPS camera + scene plan
│   ├── textured_cube_plan.md  # Texturing + UBO refactor plan
│   ├── glTF_rendering_plan.md # glTF PBR rendering plan
│   ├── debug_marker_plan.md   # RenderDoc debug marker plan
│   ├── postprocessing_plan.md # Bloom + tonemapping postprocess design
│   ├── winding_orientation.md # Full winding math (glTF -> Vulkan pipeline)
│   └── review/                # Code review notes
├── shaders/
│   ├── pbr.vert / .frag       # PBR vertex + fragment shaders
│   ├── skybox.vert / .frag    # Skybox vertex + fragment shaders
│   ├── brdf_lut.vert / .frag  # BRDF integration LUT generation shaders
│   ├── compile.bat            # Offline shader compile script
│   ├── postprocess/           # Fullscreen-triangle postprocess shaders
│   │   ├── fullscreen.vert    # Shared fullscreen-triangle vertex shader
│   │   ├── bright.frag        # Soft-knee highlight extraction
│   │   ├── blur.frag          # 9-tap separable Gaussian blur
│   │   └── composite.frag     # Scene + bloom composite + tonemapping
│   └── *.spv                  # Compiled SPIR-V binaries
└── src/
    ├── main.rs                # winit ApplicationHandler entry point
    ├── app.rs                 # App: owns window, camera, input, renderer
    ├── camera.rs              # FPS camera (LH, Y-up)
    ├── input.rs               # Keyboard & mouse input state
    ├── mesh.rs                # Vertex types, procedural cube/floor generators
    ├── scene/
    │   ├── mod.rs             # Re-exports (GpuMaterial, PbrMaterial, SceneGraph, SceneNode)
    │   ├── gltf_loader.rs     # glTF loading, RH->LH conversion, GPU upload
    │   ├── material.rs        # PbrMaterial (CPU) + GpuMaterial (GPU POD)
    │   ├── model.rs           # GpuMesh: vb/ib + index_count + material_index + world_matrix
    │   └── scene_graph.rs     # SceneGraph + SceneNode, world transform DFS
    └── vulkan/
        ├── mod.rs
        ├── context.rs         # Instance, device, queues, debug messenger, debug marker loader
        ├── buffer.rs          # GpuBuffer, staging upload, one-time command helper
        ├── swapchain.rs       # Swapchain, depth buffer, framebuffers
        ├── texture.rs         # RGBA8 upload with explicit format, runtime mipmap generation
        ├── descriptors.rs     # Global + material descriptor set layouts, pool
        ├── debug_marker.rs    # VK_EXT_debug_utils labels/object names for RenderDoc
        ├── pipeline.rs        # Render pass and PBR graphics pipeline
        ├── renderer.rs        # Command buffers, sync, per-frame UBOs, draw_frame
        ├── pbr_ubo.rs         # GlobalUniforms + PushConstants structs
        ├── cubemap.rs         # Cubemap Vulkan wrapper
        ├── ktx2_loader.rs     # KTX2 cubemap loader
        ├── brdf_lut.rs        # Procedural BRDF integration LUT generator
        ├── ibl.rs             # IBL resources: env cubemap + irradiance + prefilter + BRDF LUT
        └── postprocess/
            ├── mod.rs          # Re-exports: BloomPyramid, PostProcessResources, etc.
            ├── passes.rs       # Three render passes: HDR scene, color (no depth), composite (sRGB)
            ├── resources.rs    # PostProcessResources: images, views, framebuffers, pipelines, descriptors, UBOs
            ├── ubo.rs          # PostProcessUBO (64B, std140) + BlurPushConstants (16B)
            ├── pyramid.rs      # BloomPyramid: 2 images x 8 mips (R16G16B16A16_SFLOAT)
            ├── descriptors.rs  # Postprocess descriptor set layouts: UBO, single-input, composite-input
            ├── fullscreen.rs   # Fullscreen-triangle pipeline builder
            └── pass_trait.rs   # Shared viewport/scissor helper
```

## Architecture Highlights

- **Coordinate system:** Left-handed, Y-up. `+Z` is forward. No projection Y flip; the flip is done via negative viewport height instead.
- **Viewport flip:** `vk::Viewport.height` is negative with `y = extent.height`. This is a y-axis reflection (det = -1) that flips winding once in framebuffer space. Combined with the Z-negation applied at glTF load time (another det = -1 transform), the two improper transforms cancel, and triangles end up CCW in framebuffer space — matching `front_face = COUNTER_CLOCKWISE` with `cull_mode = BACK`. See `docs/winding_orientation.md` for the full derivation.
- **glTF RH-to-LH conversion:** Negate Z in positions, normals, tangent xyz; flip tangent.w; convert transform matrices via `diag(1,1,-1,1) * M * diag(1,1,-1,1)`.
- **glTF scene loading:** loads the default scene, or scene 0 if no default is declared. Primitives without explicit materials use an explicit glTF default material.
- **Descriptor strategy:** Two descriptor sets — set 0 (per-frame): global UBO (view/proj/camera/light) + material buffer + IBL textures (irradiance, GGX prefilter, BRDF LUT, env cubemap); set 1 (per-material): 5 combined image samplers (base_color, metallic_roughness, normal, occlusion, emissive). No descriptor indexing required.
- **IBL assets:** the renderer reads Ennis KTX2 cubemaps from `assets/environment_map/ennis/` via the `ENV_BASE_PATH` constant in `src/vulkan/renderer.rs` (`lambertian/outputCubeMap.ktx2` for the skybox, `lambertian/diffuse.ktx2` for irradiance, `ggx/specular.ktx2` for the prefiltered specular). The BRDF LUT is generated procedurally on the GPU at startup.
- **Per-frame UBOs:** one `HOST_VISIBLE | HOST_COHERENT` buffer per in-flight frame (176 B), persistently mapped. Written after fence wait, before submit.
- **Per-draw push constants:** model matrix (64 B) + material index (4 B) + padding (12 B) = 80 B, within the 128 B guaranteed minimum.
- **Texture upload:** staging buffer -> device-local `vk::Image` via `cmd_copy_buffer_to_image` for mip level 0. Runtime mipmaps are generated on the GPU via `vk::CmdBlitImage`. `Texture::from_rgba8_with_format` takes the Vulkan format explicitly. glTF base-color/emissive textures use `R8G8B8A8_SRGB`; normal, metallic-roughness, and occlusion textures use `R8G8B8A8_UNORM`.
- **Shader output color:** `pbr.frag` and `skybox.frag` output **linear HDR** (no tonemapping or gamma correction). The postprocess composite pass applies exposure + tonemapping (Linear/Reinhard/ACES) and writes to the sRGB swapchain attachment; Vulkan performs final linear-to-sRGB encoding on store. Tonemapping and gamma correction belong exclusively in the postprocess chain.
- **Postprocessing framework:** A chain of fullscreen-triangle render passes executing after the scene pass, within the same command buffer:
  1. **Scene render pass** — PBR + skybox to HDR `R16G16B16A16_SFLOAT` scene color (with depth)
  2. **Bloom Prefilter** — Soft-knee highlight extraction -> bloom mip 0
  3. **Bloom Pyramid** — 16 render passes (8 horizontal + 8 vertical) for separable Gaussian downsampling across 8 bloom mips
  4. **Composite pass** — Scene color + 8 bloom mips -> `pow(2, exposure)` -> tonemap (Linear/Reinhard/ACES) -> sRGB swapchain
  - Postprocess pipelines use `cull_mode = NONE` (fullscreen triangle is CW under Y-flip viewport).
  - Descriptor sets: set 0 = input samplers (1 for bloom prefilter/blur, 9 for composite), set 1 = postprocess UBO (exposure, bloom parameters, tonemap operator).
  - Adding a new effect: allocate framebuffer + descriptor sets, insert render pass calls in the command buffer.
- **Skybox:** Unit cube rendered with `LESS_OR_EQUAL` depth test and depth writes disabled, so it only appears where no geometry is drawn. The skybox shader strips view translation (`mat3(view)`) to keep the environment infinitely distant. Share the same `front_face = COUNTER_CLOCKWISE` / `cull_mode = BACK` as PBR; the camera-on-inside produces CCW-in-framebuffer windings (one Y-flip viewport reflection). See `docs/winding_orientation.md` §§S1-S8 for the full derivation.
- **Cleanup order:** `Renderer` is dropped before `VulkanContext` via `ManuallyDrop`. Inside the renderer: `device_wait_idle` -> scene -> IBL -> skybox buffers -> skybox pipeline -> global UBOs -> main descriptor pool/layouts -> fences/semaphores -> command pool -> PBR pipeline -> postprocess resources (pipelines, render passes, descriptor pool, bloom pyramid, scene color images) -> swapchain.
- **Sync strategy:** `MAX_FRAMES_IN_FLIGHT = 2`. `render_finished` semaphores are per-swapchain-image to avoid reuse validation errors.
- **RenderDoc debug markers:** `VK_EXT_debug_utils` is enabled in all builds. Command buffers contain frame/render-pass/per-mesh label regions (Scene Pass → PostProcessing group containing Bloom Prefilter, Bloom Pyramid, and Composite Pass), and major Vulkan objects are named for RenderDoc resource inspection.
- **Validation layers:** `VK_LAYER_KHRONOS_validation` is enabled by default in debug builds. In non-debug builds, launch with `--validation` or `--validate` to enable it.

## License

This is a personal learning project.
