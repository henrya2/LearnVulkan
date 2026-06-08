# CODEBUDDY.md

This file provides guidance to CodeBuddy Code when working with code in this repository.

## Project Overview

A Vulkan PBR renderer written in Rust. It loads and renders a **glTF 2.0 model** (DamagedHelmet) with metallic-roughness PBR shading and a prefiltered Ennis environment map (KTX2 cubemaps under `assets/environment_map/ennis/`) used for full image-based lighting (env cubemap, irradiance, GGX prefilter, BRDF LUT), in an 800x600 window using raw Vulkan bindings (`ash`). A **postprocessing framework** provides HDR bloom (8-mip separable Gaussian) and runtime-switchable tonemapping (Linear/Reinhard/ACES) with exposure control. The camera is a free-fly FPS style with mouse look (pitch/yaw), WASD movement, Space/LShift for vertical movement, and click-to-lock cursor behavior.

- **Renderer**: `ash` 0.38
- **Windowing**: `winit` 0.30 with the `ApplicationHandler` trait (no deprecated APIs)
- **Surface bridge**: `raw-window-handle` 0.6 + `ash-window` 0.13
- **Math**: `glam` 0.32 (left-handed, Y-up coordinate system)
- **Buffer uploads**: `bytemuck` for POD casts
- **Image loading**: `image` 0.25 (PNG + JPEG features), `gltf` 1.4 (import + utils)
- **Shaders**: GLSL compiled offline to SPIR-V (`.spv`) and embedded with `include_bytes!`

## Build & Run

```bash
# Compile shaders (requires Vulkan SDK glslc on PATH)
cd shaders && ./compile.bat

# Build
cargo build

# Run
cargo run

# Run a release build with Vulkan validation layers enabled
cargo run --release -- --validation
```

`assets/models/DamagedHelmet/` must exist at runtime with the glTF model and its textures. The model is loaded at startup via `load_gltf`. `assets/environment_map/ennis/` must also exist at runtime — it provides the KTX2 cubemaps consumed by `IblResources::load` (see `ENV_BASE_PATH` in `src/vulkan/renderer.rs`).

## Architecture

### Entry Point (`src/main.rs`)

Uses winit 0.30's `ApplicationHandler` trait with `EventLoop::run_app`. Window and Vulkan state are created in `resumed()`. Rendering is driven by `WindowEvent::RedrawRequested`. Do not render in `about_to_wait`.

Handles:
- `WindowEvent::KeyboardInput` -> forwards to `App::on_keyboard`
- `WindowEvent::MouseInput` -> forwards to `App::on_mouse_button`
- `WindowEvent::Focused(false)` -> auto-release cursor lock
- `DeviceEvent::MouseMotion { delta }` -> forwards raw deltas to `App::on_device_mouse_motion` for FPS look

### App (`src/app.rs`)

`App` owns the `Window` (via `Arc`), `VulkanContext`, `Renderer`, `Camera`, `InputState`, and mouse-lock state. **Critical**: `ctx` and `renderer` are wrapped in `ManuallyDrop` with an explicit `Drop` impl that destroys `renderer` before `ctx`. This ensures all device-level objects are destroyed before the Vulkan device itself is destroyed. Do not remove this ordering.

Per-frame flow:
1. `draw_frame()` calls `update()` which computes `dt`, applies mouse delta to camera yaw/pitch, applies WASD/Space/Shift movement, and returns `(view, proj, camera_pos)`.
2. These are passed to `renderer.draw_frame(&ctx, view, proj, camera_pos)` which writes the per-frame global UBO, records command buffers, and submits.

Mouse lock:
- Left click while unlocked -> `CursorGrabMode::Locked` with `Confined` fallback, hide cursor
- `Alt+Z` -> release lock, show cursor
- Focus loss -> auto-release lock

### Camera (`src/camera.rs`)

Left-handed, Y-up FPS camera.
- `calculate_quat(yaw, pitch)` = `Quat::from_euler(YXZ, yaw, pitch, 0)` — cached in the `quat` field after every rotation change.
- `forward()` = `self.quat * Vec3::Z`
- `right()` = `self.quat * Vec3::X`
- `up()` = `self.forward().cross(self.right()).normalize()`
- `view_matrix()` = `Mat4::look_to_lh(position, forward, self.up())`
- `projection_matrix(aspect)` = `Mat4::perspective_lh(fov_y, aspect, 0.1, 100.0)` — no Y flip
- `apply_mouse_delta(dx, dy)`: `yaw += dx * sens`, `pitch += dy * sens` (winit provides negative `dy` for upward motion, so pitch decreases and the camera looks up)
- Pitch clamped to +/- 89 degrees

Default position: `(0, 0, -3)` looking toward +Z.

### Input (`src/input.rs`)

Tracks keyboard state (WASD, Space, LShift, LAlt) and mouse delta. `drain_mouse_delta()` returns and zeros accumulated deltas.

### Mesh (`src/mesh.rs`)

Two vertex types:
- `Vertex { pos: [f32; 3], uv: [f32; 2] }` — legacy procedural mesh vertex type retained for cube/floor helpers. Attribute 0: `R32G32B32_SFLOAT` at offset 0; attribute 1: `R32G32_SFLOAT` at offset 12.
- `PbrVertex { pos: [f32; 3], normal: [f32; 3], tangent: [f32; 4], uv0: [f32; 2] }` — stride 48 B, used by the active PBR pipeline. Attribute 0: `R32G32B32_SFLOAT` at offset 0; attribute 1: `R32G32B32_SFLOAT` at offset 12; attribute 2: `R32G32B32A32_SFLOAT` at offset 24; attribute 3: `R32G32_SFLOAT` at offset 40.

Procedural mesh helpers:
- `cube(size)` -> 24 verts, 36 indices, CCW-from-outside in LH world.
- `floor(half, y, tile)` -> 4 verts, 6 indices, CCW from above (+Y).

### Scene Module (`src/scene/`)

- **`gltf_loader.rs`**: `load_gltf(ctx, pool, path)` -> `Scene`. Parses a glTF file, loads the default scene (or scene 0 fallback), converts RH to LH (negate Z in positions/normals/tangents, flip tangent.w, convert transforms via `diag(1,1,-1,1) * M * diag(1,1,-1,1)`), uploads vertex/index buffers and material buffer to GPU. Texture upload is semantic/color-space aware: base-color and emissive textures use `R8G8B8A8_SRGB`; normal, metallic-roughness, and occlusion textures use `R8G8B8A8_UNORM`. Creates semantic fallback textures: white sRGB, white linear, black sRGB, linear normal blue `[128,128,255,255]`, and linear metallic-roughness white `[255,255,255,255]` so scalar factors are preserved. Primitives without explicit materials use an explicit glTF default material.
- **`material.rs`**: `PbrMaterial` (CPU-side with texture indices) and `GpuMaterial` (bytemuck POD for GPU upload, with padding to 16-byte alignment).
- **`model.rs`**: `GpuMesh { vertex_buffer, index_buffer, index_count, material_index, world_matrix }` with a `destroy` method.
- **`scene_graph.rs`**: `SceneGraph` with `SceneNode` hierarchy; `compute_world_transforms()` via DFS from root nodes.

`Scene` owns all meshes, materials, textures, the material buffer, and fallback textures. It has an explicit `destroy(&self, device)` method called from `Renderer::drop`.

### Vulkan Modules (`src/vulkan/`)

- **`context.rs`**: Creates instance, debug messenger, surface, physical device, logical device, queues, and the device-level debug marker loader (`debug_marker: DebugMarker`). `VK_EXT_debug_utils` is enabled in all builds so RenderDoc markers work in release captures. Validation layer `VK_LAYER_KHRONOS_validation` is enabled by default in debug builds and can be enabled in non-debug builds with `--validation` or `--validate`. `ash::Entry::load()` is used (not `linked()`).
- **`buffer.rs`**: `GpuBuffer` plus low-level helpers (`create_buffer`, `find_memory_type`) exposed for texture and UBO creation. Staging-to-device-local upload via `create_device_local_buffer`. `with_one_time_command(ctx, pool, record)` is the shared one-shot command-buffer helper used by both buffer and image uploads.
- **`swapchain.rs`**: Swapchain creation, image views, depth image/view/memory, and framebuffers. Uses `MAILBOX` if available, else `FIFO`. Extent is clamped to surface capabilities. Depth format is probed with fallback chain: D32_SFLOAT -> D24_UNORM_S8_UINT -> D32_SFLOAT_S8_UINT.
- **`texture.rs`**: `Texture { image, memory, view, sampler }`. `Texture::from_png(ctx, pool, path)` decodes a PNG to RGBA8 and uploads as sRGB. `from_rgba8(ctx, pool, pixels, w, h)` is the sRGB convenience path; `from_rgba8_with_format(ctx, pool, pixels, w, h, format)` is the explicit-format path used by glTF semantic uploads. **Runtime mipmap generation**: `mip_levels = floor(log2(max(w, h))) + 1`. Image usage includes `TRANSFER_SRC` (in addition to `TRANSFER_DST | SAMPLED`) so each mip level can be blit-read. A blit format support check asserts the chosen format supports `BLIT_SRC | BLIT_DST` in optimal tiling. Inside the same one-time command buffer: after `cmd_copy_buffer_to_image` for level 0, a loop blits each level `i` from level `i-1` with `vk::Filter::LINEAR`, separated by `TRANSFER_DST_OPTIMAL -> TRANSFER_SRC_OPTIMAL` barriers. A final two-sub-range barrier transitions source levels from `TRANSFER_SRC_OPTIMAL` and the last level from `TRANSFER_DST_OPTIMAL` to `SHADER_READ_ONLY_OPTIMAL`. If `mip_levels == 1`, the blit loop is skipped. Image view `level_count` is set to `mip_levels`. Sampler `max_lod` is `(mip_levels - 1) as f32`, `mipmap_mode` is `LINEAR`, `REPEAT` addressing, no anisotropy.
- **`descriptors.rs`**: Two descriptor set layouts:
  - Global layout (set 0): binding 0 = `UNIFORM_BUFFER` (vertex+fragment) for `GlobalUniforms`; binding 1 = `UNIFORM_BUFFER` (fragment) for material buffer; binding 2 = `COMBINED_IMAGE_SAMPLER` (fragment) for irradiance map; binding 3 = `COMBINED_IMAGE_SAMPLER` (fragment) for prefilter (GGX) map; binding 4 = `COMBINED_IMAGE_SAMPLER` (fragment) for BRDF LUT; binding 5 = `COMBINED_IMAGE_SAMPLER` (fragment) for environment cubemap (skybox).
  - Material layout (set 1): bindings 0-4 = `COMBINED_IMAGE_SAMPLER` (fragment) for base_color, metallic_roughness, normal, occlusion, emissive textures.
  `create_descriptor_pool(device, num_materials)` sizes the pool for `MAX_FRAMES_IN_FLIGHT` global sets plus one per material.
- **`pipeline.rs`**: Creates the PBR and skybox graphics pipelines as `PipelineData` structs (`pipeline_layout` + `pipeline`). Render pass creation lives in `postprocess/passes.rs` — `pipeline.rs` receives a pre-created `vk::RenderPass` handle. PBR uses `PbrVertex` input, two descriptor set layouts (global + material), and push constants for model matrix + material index. Uses depth-stencil (`LESS`), `COUNTER_CLOCKWISE` front face, `BACK` cull mode, dynamic viewport/scissor. Skybox pipeline uses a 3D position vertex input, one descriptor set layout (global), `LESS_OR_EQUAL` depth with writes disabled, and the same front-face/cull settings.
- **`pbr_ubo.rs`**: `GlobalUniforms { view, proj, camera_pos, _pad0, light_dir, light_intensity, prefilter_max_lod, _pad1 }` (176 B) and `PushConstants { model, material_index, _pad }` (80 B), both bytemuck POD. `prefilter_max_lod` carries `mip_levels - 1` of the prefilter cubemap so the PBR shader maps roughness correctly into the prefilter chain.
- **`ibl.rs`**: `IblResources::load(ctx, pool, env_base_path)` loads the Ennis glTF sample environment from the project-relative `assets/environment_map/ennis/` directory (subfolders `lambertian/` for `outputCubeMap.ktx2` + `diffuse.ktx2`, `ggx/` for `specular.ktx2`) via `load_ktx2_cubemap`, and also generates the BRDF LUT via `generate_brdf_lut`. Owns the environment cubemap (sampled by the skybox), irradiance map, prefilter map, and BRDF LUT.
- **`postprocess/`** — Postprocessing framework with bloom + tonemapping. See `docs/postprocessing_plan.md` for the full design.
  - **`passes.rs`**: Three render passes: `create_scene_render_pass` (HDR `R16G16B16A16_SFLOAT` + depth), `create_postprocess_color_pass` (single HDR color attachment, no depth, used by bright + blur), `create_composite_render_pass` (sRGB swapchain format, final present).
  - **`fullscreen.rs`**: `create_fullscreen_pipeline(device, render_pass, layout, frag_code)` builds a fullscreen-triangle pipeline with `cull_mode = NONE` (fullscreen triangle is CW in framebuffer under Y-flip viewport). Shared vertex shader (`fullscreen.vert`) generates triangle from `gl_VertexIndex`.
  - **`descriptors.rs`**: Three descriptor set layouts: UBO (set 1 for all passes), single-input sampler (set 0 for bright + blur), composite-input (set 0 with 9 bindings: scene color + 8 bloom mips).
  - **`ubo.rs`**: `PostProcessUBO` (64 B, std140): exposure, bloom threshold/knee/intensity, 8 bloom weights, tonemap_op. `BlurPushConstants` (16 B): `[texel_size, direction, _pad]`.
  - **`pyramid.rs`**: `BloomPyramid` — 2 single images (`mip` + `temp`) with `BLOOM_MIP_COUNT=8` mip levels each. Per-level views created with `base_mip_level = i`. One `vk::Sampler` (CLAMP_TO_EDGE, LINEAR, no mip).
  - **`resources.rs`**: `PostProcessResources` — owns all postprocess device objects (images, views, framebuffers, pipelines, descriptor pool/sets, UBOs). The composite render pass is owned by `Renderer` and passed in; PostProcessResources does not create or destroy it. `PostProcessSettings` wraps `PostProcessUBO` + `bloom_enabled: bool`. `name_debug_objects` installs RenderDoc names.
  - **`pass_trait.rs`**: `set_viewport_and_bind_pipeline` helper enforces Y-flip viewport + scissor + pipeline bind in every pass. New effects can use this helper directly when recording their render pass.
  - **`mod.rs`**: Public re-exports for `renderer.rs`: `BloomPyramid`, `PostProcessResources`, `BlurPushConstants`.

  Descriptor numbering for postprocess pipelines:
  | Pipeline | Set 0 | Set 1 |
  |---|---|---|
  | Bloom Prefilter | Scene color sampler (1) | Postprocess UBO |
  | Blur pass | Input sampler (1) | Postprocess UBO + push constants |
  | Composite | Scene color + 8 bloom mips (9) | Postprocess UBO |

  Per-frame recording order (all in one command buffer):
  1. Scene render pass (PBR + skybox → HDR scene color)
  2. Bloom Prefilter (extract highlights → bloom mip 0)
  3. Bloom Pyramid (16 render passes: 8 horizontal + 8 vertical, per mip)
  4. Composite pass (scene + bloom → exposure → tonemap → sRGB swapchain)

  The viewport is the same Y-flip viewport for all passes. Postprocess shaders flip `vUV.y` when sampling previously-rendered images (standard render-to-texture Y-flip). All postprocess pipelines use `cull_mode = NONE`.
- **`debug_marker.rs`**: Thin wrapper over `ash::ext::debug_utils::Device` for `VK_EXT_debug_utils`. Provides command-buffer labels and Vulkan object names for RenderDoc in all builds.
- **`renderer.rs`**: Command pool/buffers, sync primitives, per-frame global UBOs, descriptor sets (global per-frame + per-material), scene, environment map, postprocess resources, debug marker labels/object naming, and `draw_frame`. Key design choices:
  - `MAX_FRAMES_IN_FLIGHT = 2`
  - `image_available` semaphores are per-frame
  - `render_finished` semaphores are **per-swapchain-image** (not per-frame) to avoid semaphore reuse validation errors
  - `images_in_flight` fences track which frame is using each swapchain image
  - Swapchain is recreated lazily on resize or `SUBOPTIMAL_KHR`/`ERROR_OUT_OF_DATE_KHR`
  - **Per-frame global UBO**: one `HOST_VISIBLE | HOST_COHERENT` buffer per frame, 176 B, persistently mapped. `memcpy` of `GlobalUniforms` happens after the frame's `in_flight` fence wait, before submit.
  - **Postprocess UBO**: separate `HOST_VISIBLE | HOST_COHERENT` buffer per frame, 64 B. `memcpy` of `PostProcessUBO` from `PostProcessSettings` happens in the same pre-submit window.
  - Global descriptor set (set 0) bound once per command buffer; per-material descriptor set (set 1) bound per mesh draw call.
  - Postprocess pipelines use their own descriptor sets (set 0 = input samplers, set 1 = UBO), independent of the PBR pipeline layouts.
  - Push constants updated per mesh draw call with model matrix and material index.
  - Viewport is set dynamically with **negative height**: `y = height`, `height = -height` to preserve Y-up NDC orientation. All passes (scene, bright, blur, composite) use the same Y-flip viewport. A shared helper `set_viewport_and_bind_pipeline` in `pass_trait.rs` enforces this.
  - RenderDoc markers: command buffers are labeled as Frame → Scene Pass (skybox + per-mesh PBR draws) → PostProcessing group (Bloom Prefilter → Bloom Pyramid with 16 per-mip labels → Composite Pass). Major resources are named, including scene color images/views/framebuffers, bloom mip/temp images/views, postprocess pipelines/layouts/descriptor sets/UBO buffers, and bloom framebuffers.

## Important Patterns

- **Shader compilation is offline only**. `compile.bat` calls `glslc`. The Rust binary embeds `.spv` bytes. Never compile shaders at runtime.
- **Coordinate system**: Left-handed, Y-up. +Z is forward. `perspective_lh` and `look_to_lh` from glam. No projection Y flip — the flip is done via negative viewport height instead.
- **Viewport flip**: `vk::Viewport.height` is negative and `y` starts at `extent.height`. This is a y-axis reflection (det = −1) that maps NDC y-down to framebuffer y-up, so authored Y-up content displays Y-up on screen. Per Vulkan 1.3 §28.4, front/back is determined from the signed area in **framebuffer coordinates** (after the viewport transform), so this y-reflection inverts winding once more.
- **glTF coordinate conversion**: glTF is RH Y-up; the project is LH Y-up. Conversion: negate Z in positions, normals, tangent xyz; flip tangent.w; convert transform matrices via `diag(1,1,-1,1) * M * diag(1,1,-1,1)`. The vertex Z-negate is improper (det = −1) and flips winding from CCW-from-outside (RH) to CW-from-outside (LH world). Combined with the negative-height viewport's second reflection, the two improper transforms cancel and triangles end up **CCW in framebuffer space**, matching `front_face = COUNTER_CLOCKWISE`. See `docs/winding_orientation.md` for the full end-to-end derivation with spec citations.
- **Descriptor strategy**: Two descriptor sets for PBR (set 0 global UBO/material buffer/IBL, set 1 material textures). Postprocess pipelines use their own independent descriptor pools and layouts (set 0 input samplers, set 1 postprocess UBO). No descriptor indexing required.
- **Shader color output**: `pbr.frag` outputs **linear HDR** radiance (no tonemapping). The composite postprocess pass applies exposure + tonemapping (Linear/Reinhard/ACES) and writes to the sRGB swapchain attachment; Vulkan performs final linear-to-sRGB encoding on store. Do not add tonemapping or gamma correction to PBR or skybox shaders — both belong in the postprocess chain.
- **Postprocessing framework**: Bloom + tonemapping are implemented as a chain of fullscreen-triangle render passes after the scene pass. Adding a new effect means: write a fragment shader, allocate a framebuffer + descriptor set, and insert render pass calls in the command buffer. See `src/vulkan/postprocess/pass_trait.rs` for the shared viewport/scissor helper, and `docs/postprocessing_plan.md` for the full design.
- **Per-draw data**: Push constants for model matrix + material index (80 B, within 128 B guaranteed minimum).
- **Cleanup order matters**: `Renderer` must be fully dropped (destroying all device-level objects) before `VulkanContext` drops the device. This is enforced by `ManuallyDrop` in `App`. Inside `Renderer::drop`, the order is: `device_wait_idle` → postprocess resources (pipelines, render passes, descriptor pool, bloom pyramid, scene color images) → scene → IBL → skybox vertex/index buffers → PBR pipeline/layout → skybox pipeline/layout → global UBOs → main descriptor pool/layouts → fences/semaphores → command pool → swapchain.
- **Assets**:
  - `assets/models/DamagedHelmet/` is a runtime dependency containing the glTF model and its PBR textures (albedo, normal, metallic-roughness, AO, emissive).
  - `assets/environment_map/ennis/` is a runtime dependency containing the IBL cubemap (KTX2). The renderer reads from this project-relative path via `ENV_BASE_PATH` in `src/vulkan/renderer.rs`. Layout: `lambertian/outputCubeMap.ktx2` (env cubemap), `lambertian/diffuse.ktx2` (irradiance), `ggx/specular.ktx2` (prefilter).
- **Debug markers**: RenderDoc labels and object names must work in every build configuration. Keep `VK_EXT_debug_utils` enabled independently of validation layers. Every pass and every resource must have proper debug markers. Postprocess resources are named in `PostProcessResources::name_debug_objects`. The postprocessing chain is wrapped in a "PostProcessing" debug group. Blur passes are labeled `"Bloom Mip N Horizontal/Vertical Blur"`.
- **Validation layers**: active by default in debug builds and enabled in non-debug builds with `--validation` or `--validate`. A clean shutdown produces no validation errors.

# Special notes
@docs/winding_orientation.md

## Rules
- Be honest, do not be afraid to ask for help or clarification.
- Each step must be sound accurate, do not be afraid of complication and complex. Math should be clear and accurate, no vague understanding allowed. If you are confusion, source codes must be read first to clarify. Reference websites or other docs from internet must be authorative, you also must verify that information first before using it. Sound and accurate must be above all.
- If you need to use python, please use 'uv' instead of directly use python or python3.

## Build, Run & Debug
- Must at least capture a frame of successfully rendered window to verify the program is running correctly.
- If the timed running must be done. The counting seconds must start at least one frame rendered successfully. Then at least 16 seconds must pass before stop the program. If you want to ensure the program running, longer running is good.
