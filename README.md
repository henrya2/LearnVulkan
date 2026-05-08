# LearnVulkan

A Vulkan FPS camera demo written in Rust. It renders a colored cube at the origin and a large ground plane in an 800x600 window using raw Vulkan bindings (`ash`).

## Features

- **Colored cube** at the origin with per-face colors
- **Ground plane** extending in all directions
- **Free-fly FPS camera**:
  - Mouse look (pitch/yaw)
  - WASD movement
  - Space / LShift for vertical movement
  - Click-to-lock cursor behavior
- **Clean Vulkan bring-up** with validation layers in debug builds

## Tech Stack

| Component | Crate / Tool |
|-----------|-------------|
| Vulkan bindings | `ash` 0.38 |
| Windowing | `winit` 0.30 (ApplicationHandler trait) |
| Surface bridge | `raw-window-handle` 0.6 + `ash-window` 0.13 |
| Math | `glam` 0.32 (left-handed, Y-up) |
| Buffer uploads | `bytemuck` |
| Shaders | GLSL compiled offline to SPIR-V via `glslc` |

## Build & Run

**Prerequisite:** [Vulkan SDK](https://vulkan.lunarg.com/) installed so `glslc` is on `PATH`.

```bash
# Compile shaders
cd shaders && ./compile.bat

# Build & run
cd ..
cargo run
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
├── CODEBUDDY.md              # Project guidance for CodeBuddy
├── docs/
│   ├── learn_vulkan_plan.md  # Original triangle plan
│   └── vulkan_fps_plan.md    # FPS camera + scene plan
├── shaders/
│   ├── scene.vert            # Vertex shader (MVP + vertex color)
│   ├── scene.frag            # Fragment shader (pass-through color)
│   ├── compile.bat           # Offline shader compile script
│   └── *.spv                 # Compiled SPIR-V binaries
└── src/
    ├── main.rs               # winit ApplicationHandler entry point
    ├── app.rs                # App: owns window, camera, input, renderer
    ├── camera.rs             # FPS camera (LH, Y-up)
    ├── input.rs              # Keyboard & mouse input state
    ├── mesh.rs               # Cube & floor mesh generation
    └── vulkan/
        ├── mod.rs
        ├── context.rs        # Instance, device, queues, debug messenger
        ├── buffer.rs         # GPU buffer upload utilities
        ├── swapchain.rs      # Swapchain, depth buffer, framebuffers
        ├── pipeline.rs       # Render pass, graphics pipeline
        └── renderer.rs       # Command buffers, sync, draw_frame
```

## Architecture Highlights

- **Coordinate system:** Left-handed, Y-up. `+Z` is forward. No projection Y flip; the flip is done via negative viewport height instead.
- **Viewport flip:** `vk::Viewport.height` is negative with `y = extent.height`, matching DirectX NDC orientation. `front_face` is `CLOCKWISE` to compensate.
- **Cleanup order:** `Renderer` is dropped before `VulkanContext` via `ManuallyDrop`, ensuring device-level objects are destroyed before the device itself.
- **Sync strategy:** `MAX_FRAMES_IN_FLIGHT = 2`. `render_finished` semaphores are per-swapchain-image to avoid reuse validation errors.

## License

This is a personal learning project.
