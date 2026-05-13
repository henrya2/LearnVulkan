# RenderDoc Debug Marker Plan

## Goal

Add Vulkan `VK_EXT_debug_utils` integration so RenderDoc captures show a readable render process instead of anonymous Vulkan commands and unnamed GPU resources.

The debug marker system must work in every build configuration. Vulkan validation layers remain enabled by default in debug builds, and can be enabled in non-debug builds with a command-line flag.

## Runtime Behavior

- `VK_EXT_debug_utils` is always enabled at instance creation.
- `VulkanContext` always creates a device-level `DebugMarker` loader.
- RenderDoc labels and object names are available in debug and release builds.
- Validation layers are enabled when either condition is true:
  - the binary is built with `debug_assertions`
  - the program is launched with `--validation` or `--validate`

Example:

```bash
cargo run --release -- --validation
```

## Implementation Overview

### 1. Debug Marker Wrapper

File: `src/vulkan/debug_marker.rs`

Add a small wrapper around `ash::ext::debug_utils::Device`:

```rust
pub struct DebugMarker {
    loader: ash::ext::debug_utils::Device,
}
```

Responsibilities:

- Begin command-buffer label regions with `cmd_begin_debug_utils_label`.
- End label regions with `cmd_end_debug_utils_label`.
- Insert one-shot event labels with `cmd_insert_debug_utils_label`.
- Assign names to Vulkan handles with `set_debug_utils_object_name`.

The wrapper accepts Rust `&str` names, converts them to `CString`, and uses ash's typed `vk::Handle` support to infer object type and raw object handle.

### 2. Vulkan Context Configuration

File: `src/vulkan/context.rs`

`VulkanContext` stores both:

- `debug_utils: Option<DebugUtils>` for the validation/debug messenger.
- `debug_marker: Option<DebugMarker>` for RenderDoc labels and object names.

The split is intentional:

- The debug messenger is only needed when validation is enabled.
- The marker loader is useful even without validation, especially for RenderDoc release-build captures.

Instance setup:

- Always append `ash::ext::debug_utils::NAME` to instance extensions.
- Append `VK_LAYER_KHRONOS_validation` only when validation is enabled.

### 3. Command-Line Validation Flag

Files: `src/main.rs`, `src/app.rs`, `src/vulkan/context.rs`

`main` computes:

```rust
let enable_validation = std::env::args()
    .any(|arg| arg == "--validation" || arg == "--validate")
    || cfg!(debug_assertions);
```

The flag is passed through:

```text
main.rs -> AppHandler -> App::new -> VulkanContext::new
```

This keeps validation policy at the application boundary while keeping Vulkan instance creation explicit.

### 4. RenderDoc Command Hierarchy

File: `src/vulkan/renderer.rs`

The main command buffer is labeled as:

```text
Frame N / Swapchain Image M
  Main PBR Render Pass
    Set Dynamic Viewport/Scissor
    Bind PBR Pipeline
    Bind Global Descriptors
    Draw Mesh 0 | Material X | Y indices
      Push Constants: model matrix + material index
      Bind Material Descriptor Set
      Bind Vertex/Index Buffers
      vkCmdDrawIndexed
    Draw Mesh 1 | Material X | Y indices
    ...
```

Color scheme:

| Region | Color |
|--------|-------|
| Frame | `[0.3, 0.3, 0.3, 1.0]` gray |
| Render pass | `[0.2, 0.8, 0.2, 1.0]` green |
| Draw call | `[0.3, 0.5, 1.0, 1.0]` blue |
| Setup/event labels | `[0.8, 0.7, 0.2, 1.0]` yellow |

The per-mesh label is the highest-value marker because it groups push constants, material descriptor binding, vertex/index binding, and the actual indexed draw for each glTF primitive.

### 5. GPU Object Naming

File: `src/vulkan/renderer.rs`

Object names are assigned after resource creation and are visible in RenderDoc's resource inspector.

Named renderer objects:

- `Main PBR Render Pass`
- `PBR Pipeline Layout`
- `PBR Graphics Pipeline`
- `Main Graphics Command Pool`
- `Frame Command Buffer N`
- `Global Descriptor Set Layout`
- `Material Descriptor Set Layout`
- `Renderer Descriptor Pool`
- `Global Descriptor Set Frame N`
- `Material Descriptor Set N`
- `Image Available Semaphore Frame N`
- `Render Finished Semaphore Swapchain Image N`
- `In Flight Fence Frame N`

Named scene objects:

- `Global Uniform Buffer Frame N`
- `Material Uniform Buffer`
- `Mesh N Vertex Buffer`
- `Mesh N Index Buffer`
- `Scene Texture N Image/View/Sampler`
- fallback texture image/view/sampler objects
- `Synthetic Environment Map Image/View/Sampler`

Named swapchain objects:

- `Main Swapchain`
- `Swapchain Image N`
- `Swapchain Image View N`
- `Swapchain Framebuffer N`
- `Swapchain Depth Image`
- `Swapchain Depth View`

Swapchain object names are re-applied after swapchain recreation.

## File-by-File Changes

### `src/vulkan/mod.rs`

Export the new debug marker module:

```rust
pub mod debug_marker;
```

### `src/vulkan/debug_marker.rs`

New helper module containing `DebugMarker` and methods for labels/object names.

### `src/vulkan/context.rs`

- Add `debug_marker: Option<DebugMarker>` to `VulkanContext`.
- Always enable `VK_EXT_debug_utils`.
- Create `DebugMarker` after logical device creation.
- Enable `VK_LAYER_KHRONOS_validation` only when requested by build mode or CLI flag.

### `src/main.rs`

- Parse `--validation` and `--validate`.
- Keep validation enabled automatically in debug builds.
- Store the result in `AppHandler`.

### `src/app.rs`

Pass `enable_validation` into `VulkanContext::new`.

### `src/vulkan/renderer.rs`

- Add label colors.
- Add debug marker parameter to command-buffer recording.
- Add frame/render-pass/per-mesh command labels.
- Add object naming helpers for textures and swapchain resources.
- Re-name swapchain resources after swapchain recreation.

## RenderDoc Capture Expectations

When capturing a frame in RenderDoc:

1. The Event Browser should show the frame, render pass, setup events, and each mesh draw as named regions.
2. Each draw should clearly show the material index and index count.
3. Resource Inspector should show named buffers, textures, framebuffers, pipeline objects, descriptor sets, and sync objects.
4. Release-build captures should still include markers and object names.
5. Validation output should appear only in debug builds or when launched with `--validation`/`--validate`.

## Validation Steps

```bash
cargo fmt
cargo check
cargo run -- --validation
cargo run --release -- --validation
```

Expected result:

- Compilation succeeds.
- Existing dead-code warnings may remain.
- Validation layers produce no Vulkan errors during startup, resize, rendering, or shutdown.
- RenderDoc displays the command labels and object names in both debug and release captures.
