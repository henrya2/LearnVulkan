# LearnVulkan

A Vulkan FPS camera demo written in Rust. It renders a textured cube at the origin and a large textured ground plane in an 800x600 window using raw Vulkan bindings (`ash`).

## Features

- **Textured cube** at the origin, sampling a PNG texture via a combined image sampler
- **Ground plane** that tiles the same texture 10x along each axis
- **Per-frame uniform buffer** for the MVP matrix (descriptor set, no push constants)
- **Free-fly FPS camera**:
  - Mouse look (pitch/yaw)
  - WASD movement
  - Space / LShift for vertical movement
  - Click-to-lock cursor behavior
- **Clean Vulkan bring-up** with validation layers in debug builds (zero errors on startup, resize, shutdown)

## Tech Stack

| Component | Crate / Tool |
|-----------|-------------|
| Vulkan bindings | `ash` 0.38 |
| Windowing | `winit` 0.30 (ApplicationHandler trait) |
| Surface bridge | `raw-window-handle` 0.6 + `ash-window` 0.13 |
| Math | `glam` 0.32 (left-handed, Y-up) |
| Buffer uploads | `bytemuck` |
| Image loading | `image` 0.25 (PNG only) |
| Shaders | GLSL compiled offline to SPIR-V via `glslc` |

## Build & Run

**Prerequisites:**
- [Vulkan SDK](https://vulkan.lunarg.com/) installed so `glslc` is on `PATH`.
- `assets/texture.png` present at runtime. A placeholder is checked in; regenerate or replace as you like.

```bash
# Compile shaders
cd shaders && ./compile.bat
cd ..

# (Optional) regenerate the placeholder texture — requires `uv`
uv run assets/gen_texture.py

# Build & run
cargo run
```

To change the textures, simply overwrite `assets/texture.png` with any RGBA PNG.

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
│   └── texture.png            # 256x256 checkerboard placeholder (overwrite to customize)
├── docs/
│   ├── learn_vulkan_plan.md   # Original triangle plan
│   ├── vulkan_fps_plan.md     # FPS camera + scene plan
│   └── textured_cube_plan.md  # Texturing + UBO refactor plan
├── shaders/
│   ├── scene.vert             # Vertex shader (MVP from UBO, passes UV through)
│   ├── scene.frag             # Fragment shader (samples sampler2D)
│   ├── compile.bat            # Offline shader compile script
│   └── *.spv                  # Compiled SPIR-V binaries
└── src/
    ├── main.rs                # winit ApplicationHandler entry point
    ├── app.rs                 # App: owns window, camera, input, renderer
    ├── camera.rs              # FPS camera (LH, Y-up)
    ├── input.rs               # Keyboard & mouse input state
    ├── mesh.rs                # Cube & floor mesh generation (pos + uv)
    └── vulkan/
        ├── mod.rs
        ├── context.rs         # Instance, device, queues, debug messenger
        ├── buffer.rs          # GpuBuffer, staging upload, one-time command helper
        ├── swapchain.rs       # Swapchain, depth buffer, framebuffers
        ├── texture.rs         # PNG load, staging upload, image view, sampler
        ├── descriptors.rs     # Descriptor set layout, pool, sets
        ├── pipeline.rs        # Render pass, graphics pipeline
        └── renderer.rs        # Command buffers, sync, per-frame UBOs, draw_frame
```

## Architecture Highlights

- **Coordinate system:** Left-handed, Y-up. `+Z` is forward. No projection Y flip; the flip is done via negative viewport height instead.
- **Viewport flip:** `vk::Viewport.height` is negative with `y = extent.height`, matching DirectX NDC orientation. `front_face` is `CLOCKWISE` to compensate.
- **Descriptor set:** `set=0, binding=0` is a per-frame UBO (MVP matrix, vertex stage); `set=0, binding=1` is a combined image sampler (fragment stage). Both cube and floor share the same set.
- **Per-frame UBOs:** one `HOST_VISIBLE | HOST_COHERENT` buffer per in-flight frame, persistently mapped. The MVP is `memcpy`'d after waiting on the frame's `in_flight` fence, so the GPU is never reading the memory during the write.
- **Texture upload:** staging buffer → device-local `vk::Image` via `cmd_copy_buffer_to_image` for mip level 0. **Runtime mipmap generation**: the full mip chain (`mip_levels = floor(log2(max(w, h))) + 1`) is generated on the GPU via `vk::CmdBlitImage` inside the same one-time command buffer. Each level `i` is blitted from level `i-1` with `LINEAR` filter, separated by `TRANSFER_DST_OPTIMAL → TRANSFER_SRC_OPTIMAL` barriers. A final barrier transitions all levels to `SHADER_READ_ONLY_OPTIMAL`. Image usage includes `TRANSFER_SRC` for blit reads. Format is `R8G8B8A8_SRGB`, sampler uses `REPEAT` addressing, `mipmap_mode = LINEAR`, `max_lod = (mip_levels - 1) as f32`, no anisotropy.
- **Cleanup order:** `Renderer` is dropped before `VulkanContext` via `ManuallyDrop`. Inside the renderer, texture → UBOs → descriptor pool → descriptor layout are destroyed before pipeline/layout/render pass.
- **Sync strategy:** `MAX_FRAMES_IN_FLIGHT = 2`. `render_finished` semaphores are per-swapchain-image to avoid reuse validation errors.

## License

This is a personal learning project.
