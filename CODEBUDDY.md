# CODEBUDDY.md

This file provides guidance to CodeBuddy Code when working with code in this repository.

## Project Overview

A Vulkan FPS camera demo written in Rust. It renders a colored cube at the origin and a large ground plane in an 800x600 window using raw Vulkan bindings (`ash`). The camera is a free-fly FPS style with mouse look (pitch/yaw), WASD movement, Space/LShift for vertical movement, and click-to-lock cursor behavior.

- **Renderer**: `ash` 0.38
- **Windowing**: `winit` 0.30 with the `ApplicationHandler` trait (no deprecated APIs)
- **Surface bridge**: `raw-window-handle` 0.6 + `ash-window` 0.13
- **Math**: `glam` 0.32 (left-handed, Y-up coordinate system)
- **Buffer uploads**: `bytemuck` for POD casts
- **Shaders**: GLSL compiled offline to SPIR-V (`.spv`) and embedded with `include_bytes!`

## Build & Run

```bash
# Compile shaders (requires Vulkan SDK glslc on PATH)
cd shaders && ./compile.bat

# Build
cargo build

# Run
cargo run
```

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
1. `draw_frame()` calls `update()` which computes `dt`, applies mouse delta to camera yaw/pitch, applies WASD/Space/Shift movement, and returns `view_projection(aspect)`.
2. `vp` is passed to `renderer.draw_frame(&ctx, vp)` which records command buffers and submits.

Mouse lock:
- Left click while unlocked -> `CursorGrabMode::Locked` with `Confined` fallback, hide cursor
- `Ctrl+Z` -> release lock, show cursor
- Focus loss -> auto-release lock

### Camera (`src/camera.rs`)

Left-handed, Y-up FPS camera.
- `forward()` = `Quat::from_euler(YXZ, yaw, pitch, 0) * Vec3::Z`
- `right()` = `Vec3::Y.cross(forward).normalize()` (LH right vector)
- `view_matrix()` = `Mat4::look_to_lh(position, forward, Vec3::Y)`
- `projection_matrix(aspect)` = `Mat4::perspective_lh(fov_y, aspect, 0.1, 100.0)` — no Y flip
- `apply_mouse_delta(dx, dy)`: `yaw += dx * sens`, `pitch -= dy * sens` (mouse-right turns right, mouse-up looks up)
- Pitch clamped to +/- 89 degrees

Default position: `(0, 1.6, -3)` looking toward +Z.

### Input (`src/input.rs`)

Tracks keyboard state (WASD, Space, LShift, LCtrl) and mouse delta. `drain_mouse_delta()` returns and zeros accumulated deltas.

### Mesh (`src/mesh.rs`)

`Vertex { pos: [f32; 3], color: [f32; 3] }` with bytemuck `Pod + Zeroable`.
- `cube(size)` -> 24 verts (4 per face, no sharing), 36 indices, CCW-from-outside in LH world. Face colors: +X red, -X dark red, +Y green, -Y dark green, +Z blue, -Z dark blue.
- `floor(half, y, color)` -> 4 verts, 6 indices, CCW from above (+Y).

### Vulkan Modules (`src/vulkan/`)

- **`context.rs`**: Creates instance, debug messenger (debug builds only), surface, physical device, logical device, and queues. Validation layer `VK_LAYER_KHRONOS_validation` is enabled in debug builds. `ash::Entry::load()` is used (not `linked()`).
- **`buffer.rs`**: `GpuBuffer` with staging-to-device-local upload via `create_device_local_buffer`. HOST_VISIBLE staging -> DEVICE_LOCAL target with one-shot `cmd_copy_buffer`.
- **`swapchain.rs`**: Swapchain creation, image views, depth image/view/memory, and framebuffers. Uses `MAILBOX` if available, else `FIFO`. Extent is clamped to surface capabilities. Depth format is probed with fallback chain: D32_SFLOAT -> D24_UNORM_S8_UINT -> D32_SFLOAT_S8_UINT.
- **`pipeline.rs`**: Render pass (color + depth attachments), graphics pipeline with vertex input from `mesh::Vertex`, push constant range (64 B, VERTEX stage), depth-stencil (`LESS`), and `CLOCKWISE` front face (compensates for negative viewport height). Viewport and scissor are dynamic state.
- **`renderer.rs`**: Command pool/buffers, sync primitives, and `draw_frame`. Key design choices:
  - `MAX_FRAMES_IN_FLIGHT = 2`
  - `image_available` semaphores are per-frame
  - `render_finished` semaphores are **per-swapchain-image** (not per-frame) to avoid semaphore reuse validation errors
  - `images_in_flight` fences track which frame is using each swapchain image
  - Swapchain is recreated lazily on resize or `SUBOPTIMAL_KHR`/`ERROR_OUT_OF_DATE_KHR`
  - Draws cube then floor with push-constant MVP (`view_proj.to_cols_array()`)
  - Viewport is set dynamically with **negative height**: `y = height`, `height = -height` to match DirectX NDC orientation

## Important Patterns

- **Shader compilation is offline only**. `compile.bat` calls `glslc`. The Rust binary embeds `.spv` bytes. Never compile shaders at runtime.
- **Coordinate system**: Left-handed, Y-up. +Z is forward. `perspective_lh` and `look_to_lh` from glam. No projection Y flip — the flip is done via negative viewport height instead.
- **Viewport flip**: `vk::Viewport.height` is negative and `y` starts at `extent.height`. This makes Vulkan's NDC Y match DirectX so the same MVP produces the same image. Because this reverses framebuffer winding, `front_face` is `CLOCKWISE`.
- **No vertex buffers in shaders**: the old triangle used hard-coded arrays; the new scene uses actual vertex/index buffers uploaded to GPU-local memory.
- **Cleanup order matters**: `Renderer` must be fully dropped (destroying all device-level objects) before `VulkanContext` drops the device. This is enforced by `ManuallyDrop` in `App`.
- **Debug builds**: validation layers are active and will print errors to stdout. A clean shutdown produces no validation errors.
