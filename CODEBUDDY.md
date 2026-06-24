# CODEBUDDY.md

This file provides guidance to CodeBuddy Code when working with code in this repository.

## Project Overview

A Vulkan PBR renderer written in Rust. It loads and renders a **glTF 2.0 model** (DamagedHelmet) with metallic-roughness PBR shading and a prefiltered Ennis environment map (KTX2 cubemaps under `assets/environment_map/ennis/`) used for full image-based lighting (env cubemap, irradiance, GGX prefilter, BRDF LUT), in a configurable window (default 800x600) using raw Vulkan bindings (`ash`). A **postprocessing framework** provides HDR bloom (8-mip separable Gaussian) and runtime-switchable tonemapping (Linear/Reinhard/ACES) with exposure control. The camera is a free-fly FPS style with mouse look (pitch/yaw), WASD movement, Space/LShift for vertical movement, and click-to-lock cursor behavior.

- **Renderer**: `ash` 0.38
- **Windowing**: `winit` 0.30 with the `ApplicationHandler` trait (no deprecated APIs)
- **Surface bridge**: `raw-window-handle` 0.6 + `ash-window` 0.13
- **Math**: `glam` 0.33 (left-handed, Y-up coordinate system)
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

# Run with custom resolution (default 800x600)
cargo run -- --resolution=1920x1080

# Run a release build with Vulkan validation layers enabled
cargo run --release -- --validation
```

## Testing rules

### Required build targets

- **Every change that modifies code must be verified to compile, run, and shut down cleanly in both debug and release builds.** A change is not considered correct until both build profiles pass. Run `cargo build && cargo run` first, then `cargo build --release && cargo run --release`.
- After modifying code, always build before running — do not assume compilation succeeds based on local reasoning.

### GPU-assisted validation

- GPU-assisted validation is **enabled by default in debug builds** (tied to the validation layer). To enable it in release builds, pass `--gpu-assisted` (also accepted: `--gpu_assisted`, `--vgav`) together with `--validation`.
- The flag is wired through `src/main.rs` → `App::new` → `VulkanContext::new` → `create_instance` in `src/vulkan/context.rs`, where it enables `VK_VALIDATION_FEATURE_ENABLE_GPU_ASSISTED_EXT` plus `GPU_ASSISTED_RESERVE_BINDING_SLOT` on the instance.
- A clean shutdown must produce no validation errors in either build profile.

### Verification

- Verify that at least one frame renders successfully before concluding the program works.
- For timed runs: count seconds starting after the first successful frame. Run for at least 16 seconds before stopping. Longer runs improve confidence.
- The following asset directories must exist at runtime — the program will fail without them:
  - `assets/models/DamagedHelmet/` — glTF model and PBR textures, loaded at startup via `load_gltf`
  - `assets/environment_map/ennis/` — KTX2 cubemaps for IBL, loaded by `IblResources::load` (see `ENV_BASE_PATH` in `src/vulkan/renderer.rs`)

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

Tonemap cycle:
- `T` keypress -> `App::cycle_tonemap` advances `current_tonemap` Linear -> Reinhard -> ACES, pushes the new value to the renderer, and updates the window title to `LearnVulkan - Tonemap: <OP>`.

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
- `cube(size)` -> 24 verts, 36 indices, CW-from-outside in LH world (code-defined geometry rule; currently unused).
- `floor(half, y, tile)` -> 4 verts, 6 indices, CCW from above (+Y).

### Scene Module (`src/scene/`)

- **`gltf_loader.rs`**: `load_gltf(ctx, pool, path)` -> `Scene`. Parses a glTF file, loads the default scene (or scene 0 fallback), converts RH to LH (negate Z in positions/normals/tangents, flip tangent.w, convert transforms via `diag(1,1,-1,1) * M * diag(1,1,-1,1)`), uploads vertex/index buffers and material buffer to GPU. Texture upload is semantic/color-space aware: base-color and emissive textures use `R8G8B8A8_SRGB`; normal, metallic-roughness, and occlusion textures use `R8G8B8A8_UNORM`. Creates semantic fallback textures: white sRGB, white linear, black sRGB, linear normal blue `[128,128,255,255]`, and linear metallic-roughness white `[255,255,255,255]` so scalar factors are preserved. Primitives without explicit materials use an explicit glTF default material.
- **`material.rs`**: `PbrMaterial` (CPU-side with texture indices) and `GpuMaterial` (bytemuck POD for GPU upload, with padding to 16-byte alignment).
- **`model.rs`**: `GpuMesh { vertex_buffer, index_buffer, index_count, material_index, world_matrix }` with a `destroy` method.
- **`scene_graph.rs`**: `SceneGraph` with `SceneNode` hierarchy; `compute_world_transforms()` via DFS from root nodes.

`Scene` owns all meshes, materials, textures, the material buffer, and fallback textures. It has an explicit `destroy(&self, device)` method called from `Renderer::drop`.

### Vulkan Modules (`src/vulkan/`)

- **`memory.rs`**: GPU memory management via `gpu-allocator` 0.28. `MemoryAllocator` wraps `gpu_allocator::vulkan::Allocator` with a `GpuOnly` pool for device-local resources and utility methods: `create_buffer`, `create_image`, `create_host_mapped_ubo`, `create_dedicated_image`. `OwnedBuffer` and `OwnedImage` hold a `vk::Buffer`/`vk::Image`, a `vk::DeviceMemory`, and an optional `Allocation` handle. All long-lived resources (UBOs, vertex/index buffers, textures, bloom images, scene color) are allocated through this allocator. **Destruction convention**: every resource that holds an `Allocation` has an explicit `destroy(device: &ash::Device, allocator: &mut MemoryAllocator)` method — the allocator's `free()` must be called before the `VulkanContext` that owns the `MemoryAllocator` drops. The `Drop for Renderer` is empty (debug-assert-only); all teardown is explicit in `Renderer::destroy`.
- **`context.rs`**: Creates instance, debug messenger, surface, physical device, logical device, queues, and the device-level debug marker loader (`debug_marker: DebugMarker`). `VK_EXT_debug_utils` is enabled in all builds so RenderDoc markers work in release captures. Validation layer `VK_LAYER_KHRONOS_validation` is enabled by default in debug builds and can be enabled in non-debug builds with `--validation` or `--validate`. `ash::Entry::load()` is used (not `linked()`).
- **`buffer.rs`**: `GpuBuffer` plus low-level helpers (`create_buffer`, `find_memory_type`) exposed for texture and UBO creation. Staging-to-device-local upload via `create_device_local_buffer`. `with_one_time_command(ctx, pool, record)` is the shared one-shot command-buffer helper used by both buffer and image uploads.
- **`swapchain.rs`**: Swapchain creation, image views, depth image/view/memory, and framebuffers. Uses `MAILBOX` if available, else `FIFO`. Extent is clamped to surface capabilities. Depth format is probed with fallback chain: D32_SFLOAT -> D24_UNORM_S8_UINT -> D32_SFLOAT_S8_UINT.
- **`texture.rs`**: `Texture { image, memory, view, sampler }`. `Texture::from_png(ctx, pool, path)` decodes a PNG to RGBA8 and uploads as sRGB. `from_rgba8(ctx, pool, pixels, w, h)` is the sRGB convenience path; `from_rgba8_with_format(ctx, pool, pixels, w, h, format)` is the explicit-format path used by glTF semantic uploads. **Runtime mipmap generation**: `mip_levels = floor(log2(max(w, h))) + 1`. Image usage includes `TRANSFER_SRC` (in addition to `TRANSFER_DST | SAMPLED`) so each mip level can be blit-read. A blit format support check asserts the chosen format supports `BLIT_SRC | BLIT_DST` in optimal tiling. Inside the same one-time command buffer: after `cmd_copy_buffer_to_image` for level 0, a loop blits each level `i` from level `i-1` with `vk::Filter::LINEAR`, separated by `TRANSFER_DST_OPTIMAL -> TRANSFER_SRC_OPTIMAL` barriers. A final two-sub-range barrier transitions source levels from `TRANSFER_SRC_OPTIMAL` and the last level from `TRANSFER_DST_OPTIMAL` to `SHADER_READ_ONLY_OPTIMAL`. If `mip_levels == 1`, the blit loop is skipped. Image view `level_count` is set to `mip_levels`. Sampler `max_lod` is `(mip_levels - 1) as f32`, `mipmap_mode` is `LINEAR`, `REPEAT` addressing, no anisotropy.
- **`descriptors.rs`**: Two descriptor set layouts:
  - Global layout (set 0): binding 0 = `UNIFORM_BUFFER` (vertex+fragment) for `GlobalUniforms`; binding 1 = `UNIFORM_BUFFER` (fragment) for material buffer; binding 2 = `COMBINED_IMAGE_SAMPLER` (fragment) for irradiance map; binding 3 = `COMBINED_IMAGE_SAMPLER` (fragment) for prefilter (GGX) map; binding 4 = `COMBINED_IMAGE_SAMPLER` (fragment) for BRDF LUT; binding 5 = `COMBINED_IMAGE_SAMPLER` (fragment) for environment cubemap (skybox).
  - Material layout (set 1): bindings 0-4 = `COMBINED_IMAGE_SAMPLER` (fragment) for base_color, metallic_roughness, normal, occlusion, emissive textures.
  `create_descriptor_pool(device, num_materials)` sizes the pool for `MAX_FRAMES_IN_FLIGHT` global sets plus one per material.
- **`pipeline.rs`**: Creates the PBR and skybox graphics pipelines as `PipelineData` structs (`pipeline_layout` + `pipeline`). Render pass creation lives in `postprocess/passes.rs` — `pipeline.rs` receives a pre-created `vk::RenderPass` handle. PBR uses `PbrVertex` input, two descriptor set layouts (global + material), and push constants for model matrix + material index. Uses depth-stencil (`LESS`), `COUNTER_CLOCKWISE` front face, `BACK` cull mode, dynamic viewport/scissor. Skybox pipeline uses a 3D position vertex input, one descriptor set layout (global), `LESS_OR_EQUAL` depth with writes disabled, `COUNTER_CLOCKWISE` front face, and `FRONT` cull mode (CW-from-outside geometry viewed from inside; cull the outside faces).
- **`pbr_ubo.rs`**: `GlobalUniforms { view, proj, camera_pos, light_dir, lighting_pack }` (176 B) and `PushConstants { model, tail }` (80 B), both bytemuck POD. `camera_pos` and `light_dir` are `Vec4` (the shader reads `.xyz`; the trailing `.w` is dead on both sides). `lighting_pack: Vec4` carries `light_intensity` in `.x` and `prefilter_max_lod` (i.e. `mip_levels - 1` of the prefilter cubemap) in `.y` so the PBR shader maps roughness into the prefilter chain. `tail: Vec4` carries the bit-packed `material_index` in `.x` (`.yzw` dead). No `_pad` fields — the trailing `Vec4` of each struct is what provides the alignment round-up.
- **`ibl.rs`**: `IblResources::load(ctx, pool, env_base_path)` loads the Ennis glTF sample environment from the project-relative `assets/environment_map/ennis/` directory (subfolders `lambertian/` for `outputCubeMap.ktx2` + `diffuse.ktx2`, `ggx/` for `specular.ktx2`) via `load_ktx2_cubemap`, and also generates the BRDF LUT via `generate_brdf_lut`. Owns the environment cubemap (sampled by the skybox), irradiance map, prefilter map, and BRDF LUT.
- **`postprocess/`** — Postprocessing framework with bloom + tonemapping. See `docs/postprocessing_plan.md` for the full design.
  - **`passes.rs`**: Three render passes: `create_scene_render_pass` (HDR `R16G16B16A16_SFLOAT` + depth), `create_postprocess_color_pass` (single HDR color attachment, no depth, used by bright + blur), `create_composite_render_pass` (sRGB swapchain format, final present).
  - **`fullscreen.rs`**: `create_fullscreen_pipeline(device, render_pass, layout, frag_code)` builds a fullscreen-triangle pipeline with `cull_mode = NONE` (fullscreen triangle is CW in framebuffer under Y-flip viewport). Shared vertex shader (`fullscreen.vert`) generates triangle from `gl_VertexIndex`.
  - **`descriptors.rs`**: Three descriptor set layouts: UBO (set 1 for all passes), single-input sampler (set 0 for bright + blur), composite-input (set 0 with 9 bindings: scene color + 8 bloom mips).
  - **`ubo.rs`**: `PostProcessUBO` (64 B, std140): `exposure_pack: Vec4` (`.x` exposure, `.y` bloom_threshold, `.z` bloom_knee, `.w` bloom_intensity), `bloom_weights: [Vec4; 2]` packing 8 logical weights 4-per-`Vec4` channel, and `tonemap_pack: Vec4` carrying the bit-packed `tonemap_op` in `.x` via `f32::from_bits` (the GLSL composite shader's `if/else` chain reads `0 = Linear`, `1 = Reinhard`, `2 = ACES`). Use `set_bloom_weights(&[f32; 8])` to write the logical 8-weight slice. `BlurPushConstants` (16 B): single `params: Vec4` (`.xy` = texel size, `.z` = bit-packed `i32` direction, `.w` = dead). The runtime `TonemapOp` enum (`Linear`/`Reinhard`/`Aces` in `src/vulkan/postprocess/resources.rs`) is the type-safe handle; the `T` keybind in `src/app.rs::cycle_tonemap` cycles it at runtime and the window title reflects the active operator.
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
  - **Postprocess UBO**: separate `HOST_VISIBLE | HOST_COHERENT` buffer per frame, 64 B (`PostProcessUBO`). `memcpy` of `PostProcessUBO` from `PostProcessSettings` happens in the same pre-submit window.
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
- **Code-defined geometry rule**: All code-defined geometry (skybox cube, postprocess fullscreen triangle, procedural meshes) MUST be authored in **LH Y-up model space with CW-from-outside front-face winding**. This is the same convention that glTF models end up in after the loader's Z-negate. For skybox geometry specifically: CW-from-outside indices, viewed from inside the cube, so the pipeline uses `cull_mode = FRONT` (cull outside, keep inside). See `docs/winding_orientation.md` §"Code-Defined Geometry Rule".
- **Descriptor strategy**: Two descriptor sets for PBR (set 0 global UBO/material buffer/IBL, set 1 material textures). Postprocess pipelines use their own independent descriptor pools and layouts (set 0 input samplers, set 1 postprocess UBO). No descriptor indexing required.
- **Shader color output**: `pbr.frag` outputs **linear HDR** radiance (no tonemapping). The composite postprocess pass applies exposure + tonemapping (Linear/Reinhard/ACES) and writes to the sRGB swapchain attachment; Vulkan performs final linear-to-sRGB encoding on store. Do not add tonemapping or gamma correction to PBR or skybox shaders — both belong in the postprocess chain.
- **Postprocessing framework**: Bloom + tonemapping are implemented as a chain of fullscreen-triangle render passes after the scene pass. Adding a new effect means: write a fragment shader, allocate a framebuffer + descriptor set, and insert render pass calls in the command buffer. See `src/vulkan/postprocess/pass_trait.rs` for the shared viewport/scissor helper, and `docs/postprocessing_plan.md` for the full design.
- **Per-draw data**: Push constants for model matrix + material index (80 B, within 128 B guaranteed minimum).
- **Cleanup order matters**: `Renderer` must be fully destroyed before `VulkanContext` drops the device. This is enforced by `ManuallyDrop` in `App`, which calls `Renderer::destroy(ctx.device, ctx.allocator)` in `App::drop` before the `ManuallyDrop::drop`. Inside `Renderer::destroy`, the order is: `device_wait_idle` → scene → IBL → skybox vertex/index buffers → skybox pipeline/layout → global UBOs → main descriptor pool/layouts → fences/semaphores → command pool → PBR pipeline/layout → postprocess resources → composite render pass → swapchain. Every resource that holds a `gpu_allocator` `Allocation` has an explicit `destroy(device, allocator)` method — they are not cleaned up by `Drop` (the renderer's `Drop` is a debug-assert empty stub).
- **Assets**:
  - `assets/models/DamagedHelmet/` is a runtime dependency containing the glTF model and its PBR textures (albedo, normal, metallic-roughness, AO, emissive).
  - `assets/environment_map/ennis/` is a runtime dependency containing the IBL cubemap (KTX2). The renderer reads from this project-relative path via `ENV_BASE_PATH` in `src/vulkan/renderer.rs`. Layout: `lambertian/outputCubeMap.ktx2` (env cubemap), `lambertian/diffuse.ktx2` (irradiance), `ggx/specular.ktx2` (prefilter).
- **Debug markers**: RenderDoc labels and object names must work in every build configuration. Keep `VK_EXT_debug_utils` enabled independently of validation layers. Every pass and every resource must have proper debug markers. Postprocess resources are named in `PostProcessResources::name_debug_objects`. The postprocessing chain is wrapped in a "PostProcessing" debug group. Blur passes are labeled `"Bloom Mip N Horizontal/Vertical Blur"`.
- **Validation layers**: `VK_LAYER_KHRONOS_validation` is enabled by default in debug builds and can be enabled in non-debug builds with `--validation` or `--validate`. GPU-assisted validation is enabled by default in debug builds alongside the validation layer; it can be enabled in release builds with `--gpu-assisted` (also `--gpu_assisted`, `--vgav`). A clean shutdown produces no validation errors.

# Special notes
@docs/winding_orientation.md

## Rules
- Be honest, do not be afraid to ask for help or clarification.
- Each step must be sound accurate, do not be afraid of complication and complex. Math should be clear and accurate, no vague understanding allowed. If you are confusion, source codes must be read first to clarify. Reference websites or other docs from internet must be authorative, you also must verify that information first before using it. Sound and accurate must be above all.
- If you need to use python, please use 'uv' instead of directly use python or python3.
- **Shader buffer layout rule** — every UBO / SSBO / push-constant struct in this project MUST follow this rule end-to-end. This is a non-negotiable project convention; do not introduce exceptions, "convenience" scalar fields, or local deviations.
  - **Struct shape:** `#[repr(C)]` + `#[derive(Clone, Copy, Pod, Zeroable)]` (from `bytemuck`). Every field is **exactly one of**: `glam::Mat4`, `glam::Vec4`, or `[glam::Vec4; N]` (where `N` is a `const` literal). No `f32`, no `u32`, no `i32`, no `bool`, no `[f32; N]`, no `[u32; N]`, no nested tuples, no `glam::Vec3`, no `Mat3`, no enums.
  - **Channel-reuse policy: any free channel of any group-named Vec4 is fair game for scalar bit-packing.** If a `Vec4` in a GLSL block has any channel that is not read by the shader (e.g. the `.w` of a 3D vector, or `.y`/`.z`/`.w` of a single-purpose scalar pack), the Rust struct must mirror that exact channel layout 1:1, and the CPU may put a bit-packed scalar into the free channel(s). The policy is **opportunistic** — pack only when the data is genuinely needed; do not introduce "reserved for future use" fields speculatively. The pack policy is **channel-agnostic**: no canonical "use `.x`" rule. The only preference is "the first free channel of the most thematically-appropriate pack Vec4", which is a judgment call the contributor makes and documents in a comment.
    - **Structural `.w` slots are never pack targets.** When the GLSL block declares a `vec3` followed by another field, std140 rounds the `vec3` to a `vec4` (the trailing 4 B is the **alignment pad**). On the Rust side this slot is a `Vec4` and its `.w` is set to 0. Do not bit-pack into this slot — the GPU will read garbage if the next field's std140 alignment rule ever changes. The `GpuMaterial::emissive_factor` `.w` is the canonical example.
  - **GLSL ↔ Rust must be 1:1.** Each Rust `Mat4` ↔ each GLSL `mat4`. Each Rust `Vec4` ↔ each GLSL `vec4`. Each Rust `[Vec4; N]` ↔ each GLSL `vec4[N]` or `vec4 array_name[N]`. The std140 / push-constant byte layout on the GPU and the `#[repr(C)]` byte layout on the CPU must be **trivially identical** — no `#[repr(C, packed)]`, no manual padding, no `_pad` fields. The trailing `Vec4` of a struct is the **explicit, intentional** round-up; do not append `_pad` / `_pad0` / `_pad1` "safety" fields on top of it. **GLSL must declare every channel that the Rust struct has a field for** (a `vec4 foo;` in GLSL implicitly reserves `.w`; if the Rust struct's `foo.w` is going to carry a bit-packed scalar, the GLSL must say so in a comment, even if it does not read the channel — this keeps the 1:1 mirror honest and prevents a future contributor from thinking `.w` is "free for the taking").
  - **Read/write through named methods only.** All scalar channel access goes through a `set_*(self, v: T)` / `*(&self) -> T` pair on the struct (e.g. `set_tonemap_op(u32)`, `set_material_index(u32)`, `set_direction(i32)`, `set_prefilter_max_lod(f32)`, `set_light_intensity(f32)`, `set_exposure(f32)`, `set_bloom_threshold(f32)`, `set_bloom_knee(f32)`, `set_bloom_intensity(f32)`, `set_texel_size(f32, f32)`, `set_bloom_weights(&[f32; 8])`). Do not write `self.tail.x = f32::from_bits(v)` at the call site; add a setter and use it. Channel fields use names that reflect a **group** of packed scalars (`exposure_pack`, `lighting_pack`, `tail`, `tonemap_pack`, `params`) — not the name of a single scalar.
  - **Bit-packing on the wire.** Real `f32` values stay as `f32` (e.g. `lighting_pack.x = light_intensity`). Bit-packed scalars (any `u32`/`i32`/packed-flag) use `f32::from_bits(v)` on the CPU and `floatBitsToUint` / `uintBitsToFloat` / `floatBitsToInt` / `intBitsToFloat` on the GPU.
  - **Descriptor and push-constant ranges** must come from `std::mem::size_of::<Struct>()` (or a `pub const BLOCK_SIZE: u64` next to the struct that asserts the size with `const _: [(); N] = [(); std::mem::size_of::<Struct>()];`). The shader's std140 block size must round up to the **same** value via the trailing `Vec4` (a struct of `K × Vec4` round-trips at `K × 16` on both sides).
  - **Adding a new shader buffer — checklist (do all of these, in order):**
    1. Read the GLSL block declaration first. Count fields, note the std140 / push-constant block size, note any trailing round-up, and identify every free channel (e.g. `.w` of a 3D vector, or `.y`/`.z`/`.w` of a scalar pack).
    2. Define the Rust struct with `#[repr(C)] #[derive(Clone, Copy, Pod, Zeroable)]`, mirroring the GLSL field-for-field using only `Mat4` / `Vec4` / `[Vec4; N]`. Every free channel in the GLSL block must have a named Rust setter on the struct (even if no CPU code currently calls it — the GLSL ↔ Rust 1:1 contract requires the Rust struct to be able to write every declared channel).
    3. Add a `pub const BLOCK_SIZE: u64 = std::mem::size_of::<Self>() as u64;` (or use `size_of` directly) and a `const _: [(); N] = [(); std::mem::size_of::<Self>()];` assertion so a future layout drift is a compile error.
    4. Add a `set_*(v)` / `*() -> T` pair for every packed scalar channel, using `f32::from_bits` / `.x.to_bits()`.
    5. Use the struct in the descriptor (`range: BLOCK_SIZE`) and/or push-constant range (`size: BLOCK_SIZE as u32`).
    6. After implementing, run `cargo build` and **read the compiler error if `size_of` changes**: the `const _:` assertion will fire. Update the GLSL to match, not the other way around.
  - **Reference examples already in the codebase** (mirror their shape exactly when adding new ones): `GlobalUniforms` and `PushConstants` in `src/vulkan/pbr_ubo.rs`; `PostProcessUBO` and `BlurPushConstants` in `src/vulkan/postprocess/ubo.rs`; `GpuMaterial` in `src/scene/material.rs`. **Free-slot inventory** (the project's "currently free, do not pack into" list — update this when a slot is consumed):
    - `GlobalUniforms.camera_pos.w`, `GlobalUniforms.light_dir.w` — reserved, no consumer yet
    - `GlobalUniforms.lighting_pack.z`, `GlobalUniforms.lighting_pack.w` — declared dead
    - `PushConstants.tail.y`, `.z`, `.w` — declared dead
    - `PostProcessUBO.tonemap_pack.y`, `.z` — declared dead; `.w` is the std140 round-up
    - `BlurPushConstants.params.w` — declared dead
    - `GpuMaterial.emissive_factor.w` — **structural std140 alignment pad for the `vec3` `emissive_factor.rgb`**, NEVER pack here


