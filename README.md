# LearnVulkan

A Vulkan PBR renderer written in Rust. It loads and renders glTF 2.0 models (DamagedHelmet) with metallic-roughness PBR shading, normal mapping, and a synthetic environment map placeholder for image-based lighting, in an 800x600 window using raw Vulkan bindings (`ash`).

## Features

- **glTF 2.0 model loading** via the `gltf` crate (DamagedHelmet from KhronosGroup/glTF-Sample-Models)
- **PBR metallic-roughness shading** with Cook-Torrance BRDF (GGX/Trowbridge-Reitz NDF, Smith geometry, Schlick Fresnel)
- **Normal mapping** with TBN tangent-space construction
- **Per-material textures**: base color, metallic-roughness, normal, occlusion, emissive (with semantic-specific 1x1 fallbacks)
- **glTF texture color-space handling**: base color/emissive are uploaded as sRGB; normal/metallic-roughness/occlusion are uploaded as linear UNORM
- **Synthetic LDR environment map** for simplified placeholder IBL
- **Per-frame uniform buffer** for view/proj/camera/light data (descriptor set, no push constants for globals)
- **Per-draw push constants** for model matrix + material index (80 B)
- **Free-fly FPS camera**:
  - Mouse look (pitch/yaw)
  - WASD movement
  - Space / LShift for vertical movement
  - Click-to-lock cursor behavior
- **Clean Vulkan bring-up** with validation layers in debug builds, plus opt-in validation in non-debug builds with `--validation` / `--validate`
- **RenderDoc-friendly debug markers** via `VK_EXT_debug_utils` in all builds: labeled frame/render-pass/mesh regions and named GPU resources

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
| Shaders | GLSL compiled offline to SPIR-V via `glslc` |

## Build & Run

**Prerequisites:**
- [Vulkan SDK](https://vulkan.lunarg.com/) installed so `glslc` is on `PATH`.
- `assets/models/DamagedHelmet/` present with the glTF model and textures.

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
│   └── models/
│       └── DamagedHelmet/     # glTF model with PBR textures
├── docs/
│   ├── learn_vulkan_plan.md   # Original triangle plan
│   ├── vulkan_fps_plan.md     # FPS camera + scene plan
│   ├── textured_cube_plan.md  # Texturing + UBO refactor plan
│   ├── glTF_rendering_plan.md # glTF PBR rendering plan
│   ├── debug_marker_plan.md   # RenderDoc debug marker plan
│   └── review/                # Code review notes
├── shaders/
│   ├── scene.vert / .frag     # Legacy scene shaders (cube/floor)
│   ├── pbr.vert / .frag       # PBR vertex + fragment shaders
│   ├── compile.bat            # Offline shader compile script
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
        ├── pipeline.rs        # Render pass, legacy + PBR graphics pipelines
        ├── renderer.rs        # Command buffers, sync, per-frame UBOs, draw_frame
        ├── pbr_ubo.rs         # GlobalUniforms + PushConstants structs
        └── environment_map.rs # Synthetic environment map generation
```

## Architecture Highlights

- **Coordinate system:** Left-handed, Y-up. `+Z` is forward. No projection Y flip; the flip is done via negative viewport height instead.
- **Viewport flip:** `vk::Viewport.height` is negative with `y = extent.height`, preserving Y-up orientation from NDC to screen space. `front_face` is `COUNTER_CLOCKWISE` because the negative viewport height preserves (does not reverse) winding order.
- **glTF RH-to-LH conversion:** Negate Z in positions, normals, tangent xyz; flip tangent.w; convert transform matrices via `diag(1,1,-1,1) * M * diag(1,1,-1,1)`.
- **glTF scene loading:** loads the default scene, or scene 0 if no default is declared. Primitives without explicit materials use an explicit glTF default material.
- **Descriptor strategy:** Two descriptor sets — set 0 (per-frame): global UBO (view/proj/camera/light) + material buffer + env map; set 1 (per-material): 5 combined image samplers (base_color, metallic_roughness, normal, occlusion, emissive). No descriptor indexing required.
- **Per-frame UBOs:** one `HOST_VISIBLE | HOST_COHERENT` buffer per in-flight frame (160 B), persistently mapped. Written after fence wait, before submit.
- **Per-draw push constants:** model matrix (64 B) + material index (4 B) + padding (12 B) = 80 B, within the 128 B guaranteed minimum.
- **Texture upload:** staging buffer -> device-local `vk::Image` via `cmd_copy_buffer_to_image` for mip level 0. Runtime mipmaps are generated on the GPU via `vk::CmdBlitImage`. `Texture::from_rgba8_with_format` takes the Vulkan format explicitly. glTF base-color/emissive textures use `R8G8B8A8_SRGB`; normal, metallic-roughness, occlusion, and the synthetic environment lighting texture use `R8G8B8A8_UNORM`.
- **Shader output color:** `pbr.frag` applies ACES tone mapping and outputs linear color; the sRGB swapchain attachment performs final linear-to-sRGB encoding.
- **Cleanup order:** `Renderer` is dropped before `VulkanContext` via `ManuallyDrop`. Inside the renderer, scene -> env map -> UBOs -> descriptor pool/layouts -> sync -> command pool -> pipeline/layout/render pass -> swapchain.
- **Sync strategy:** `MAX_FRAMES_IN_FLIGHT = 2`. `render_finished` semaphores are per-swapchain-image to avoid reuse validation errors.
- **RenderDoc debug markers:** `VK_EXT_debug_utils` is enabled in all builds. Command buffers contain frame/render-pass/per-mesh label regions, and major Vulkan objects are named for RenderDoc resource inspection.
- **Validation layers:** `VK_LAYER_KHRONOS_validation` is enabled by default in debug builds. In non-debug builds, launch with `--validation` or `--validate` to enable it.

## License

This is a personal learning project.
