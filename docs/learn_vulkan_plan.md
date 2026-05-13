# LearnVulkan Plan: Colored Triangle

A minimal Vulkan program that renders one RGB-interpolated triangle in the center of the window.

- Renderer:  `ash` 0.38 (raw Vulkan bindings)
- Windowing: `winit` 0.30 with the `ApplicationHandler` trait (no deprecated APIs)
- Surface bridge: `raw-window-handle` 0.6 + `ash-window` 0.13
- Shaders: GLSL compiled offline to SPIR-V (`.spv`) and embedded with `include_bytes!`

> Naming note: the user request says "vertex and pixel shaders". In Vulkan/GLSL these are **vertex** and **fragment** shaders, so the files are named `.vert` / `.frag`.

---

## 0. Project layout

```
LearnVulkan/
├─ Cargo.toml
├─ docs/
│  └─ learn_vulkan_plan.md      (this file)
├─ shaders/
│  ├─ triangle.vert              GLSL vertex shader
│  ├─ triangle.frag              GLSL fragment shader
│  ├─ compile.bat                offline compile helper (calls glslc)
│  ├─ triangle.vert.spv          produced by glslc
│  └─ triangle.frag.spv          produced by glslc
└─ src/
   ├─ main.rs                    winit ApplicationHandler entry
   ├─ app.rs                     App owning Window + Vulkan state
   └─ vulkan/
      ├─ mod.rs
      ├─ context.rs              instance, debug, surface, device, queues
      ├─ swapchain.rs            swapchain, image views, framebuffers
      ├─ pipeline.rs             render pass, shader modules, pipeline
      └─ renderer.rs             command pool/buffers, sync, draw_frame
```

---

## 1. `Cargo.toml`

```toml
[package]
name = "LearnVulkan"
version = "0.1.0"
edition = "2024"

[dependencies]
ash            = "0.38"
ash-window     = "0.13"   # bridges raw-window-handle 0.6 <-> ash 0.38 surfaces
winit          = "0.30"
raw-window-handle = "0.6"
```

`ash-window` 0.13 is the version aligned with `ash` 0.38 and `raw-window-handle` 0.6, and provides `enumerate_required_extensions` + `create_surface`.

---

## 2. Shaders

### `shaders/triangle.vert`
Three NDC positions centered on the origin, with a per-vertex color.

```glsl
#version 450
layout(location = 0) out vec3 vColor;

vec2 positions[3] = vec2[](
    vec2( 0.0, -0.5),
    vec2( 0.5,  0.5),
    vec2(-0.5,  0.5)
);
vec3 colors[3] = vec3[](
    vec3(1.0, 0.0, 0.0),
    vec3(0.0, 1.0, 0.0),
    vec3(0.0, 0.0, 1.0)
);

void main() {
    gl_Position = vec4(positions[gl_VertexIndex], 0.0, 1.0);
    vColor      = colors[gl_VertexIndex];
}
```

### `shaders/triangle.frag`

```glsl
#version 450
layout(location = 0) in  vec3 vColor;
layout(location = 0) out vec4 outColor;

void main() {
    outColor = vec4(vColor, 1.0);
}
```

### `shaders/compile.bat` (offline compile via Vulkan SDK `glslc`)

```bat
@echo off
glslc triangle.vert -o triangle.vert.spv
glslc triangle.frag -o triangle.frag.spv
```

The Rust binary embeds the resulting bytes with
`include_bytes!("../shaders/triangle.vert.spv")` so we never compile shaders at runtime.

---

## 3. winit 0.30 entry point (`src/main.rs`)

Use the `ApplicationHandler` trait — the modern, non-deprecated path.

- Hold `Option<App>` in the handler. Create the `Window` and Vulkan state inside `resumed()`, per winit 0.30 guidance.
- Use `EventLoop::new()` + `event_loop.run_app(&mut handler)` (not the old closure-based `EventLoop::run`).
- Drive rendering from `WindowEvent::RedrawRequested` and call `window.request_redraw()` at the end of each frame.
- Do **not** render in `about_to_wait` (winit explicitly recommends against it).

Skeleton:

```rust
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct AppHandler { app: Option<App> }

impl ApplicationHandler for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.app.is_none() {
            let attrs = Window::default_attributes()
                .with_title("LearnVulkan - Triangle")
                .with_inner_size(winit::dpi::LogicalSize::new(800, 600));
            let window = event_loop.create_window(attrs).unwrap();
            self.app = Some(App::new(window));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(app) = self.app.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested      => event_loop.exit(),
            WindowEvent::Resized(size)       => app.on_resize(size),
            WindowEvent::RedrawRequested     => {
                app.draw_frame();
                app.window().request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::new().unwrap();
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut handler = AppHandler { app: None };
    event_loop.run_app(&mut handler).unwrap();
}
```

`App::on_resize` only flags the swapchain dirty; the actual recreate happens lazily in `draw_frame`, so we never rebuild on a zero-sized minimized window.

---

## 4. Vulkan bring-up (`src/vulkan/*`)

Standard hello-triangle path, mapped to ash 0.38 APIs.

### 4.1 Entry & Instance — `context.rs`
- `ash::Entry::linked()`.
- Required extensions = `ash_window::enumerate_required_extensions(display_handle.as_raw())` + `VK_EXT_debug_utils` in debug builds.
- Validation layer `VK_LAYER_KHRONOS_validation` enabled in debug builds only.
- Create `vk::DebugUtilsMessengerEXT` in debug for diagnostics.

### 4.2 Surface
- `ash_window::create_surface(&entry, &instance, display_handle, window_handle, None)`.
- Use `ash::khr::surface::Instance` for capability queries.

### 4.3 Physical device + queue families
- First device with: a graphics queue, presentation support on the surface, and the `VK_KHR_swapchain` extension. Track `graphics_family` and `present_family` (often the same index).

### 4.4 Logical device + queues
- One queue per unique family. Enable the `khr::swapchain` device extension.

### 4.5 Swapchain — `swapchain.rs`
- Prefer surface format `B8G8R8A8_SRGB` + `SRGB_NONLINEAR`. Shaders should output linear color to this sRGB attachment; do not manually gamma-encode in the fragment shader for this path.
- Present mode: `MAILBOX` if available, else `FIFO`.
- Extent clamped to surface caps using the current window inner size.
- Image view per swapchain image.

### 4.6 Render pass + pipeline — `pipeline.rs`
- Single color attachment, `load=CLEAR`, `store=STORE`, final layout `PRESENT_SRC_KHR`.
- One subpass + one `SubpassDependency` from `EXTERNAL` for the layout transition.
- Shader modules from embedded `.spv` bytes. Convert `&[u8]` to `Vec<u32>` after asserting `len % 4 == 0` (copy is alignment-safe).
- Empty `VertexInputState` (positions are baked into the vertex shader).
- `InputAssembly = TRIANGLE_LIST`.
- Viewport + scissor as **dynamic state**, so resize doesn't need a new pipeline; set them per command buffer.
- `PipelineLayout` has no descriptors and no push constants.

### 4.7 Framebuffers
One per swapchain image view, sized to the swapchain extent.

### 4.8 Command pool + buffers — `renderer.rs`
- Pool on the graphics family with `RESET_COMMAND_BUFFER`.
- Allocate `MAX_FRAMES_IN_FLIGHT = 2` primary command buffers.

### 4.9 Sync primitives
Per frame: `image_available` semaphore, `render_finished` semaphore, `in_flight` fence (signaled at creation).

### 4.10 `draw_frame`
1. Wait + reset the current frame's fence.
2. `acquire_next_image` — on `ERROR_OUT_OF_DATE_KHR` mark swapchain dirty and return.
3. Reset and record the command buffer:
   `begin → begin_render_pass(clear=black) → bind pipeline → set_viewport/scissor → draw(3,1,0,0) → end_render_pass → end`.
4. Submit waiting on `image_available` (stage `COLOR_ATTACHMENT_OUTPUT`), signaling `render_finished`.
5. `queue_present` waiting on `render_finished`. On `SUBOPTIMAL_KHR`, `OUT_OF_DATE_KHR`, or the resize flag: `device_wait_idle` then rebuild swapchain + views + framebuffers.
6. Advance `current_frame = (current_frame + 1) % MAX_FRAMES_IN_FLIGHT`.

### 4.11 Shutdown
`Drop` impl: `device_wait_idle`, then destroy in reverse creation order — sync primitives, command pool, framebuffers, pipeline, layout, render pass, image views, swapchain, device, debug messenger, surface, instance.

---

## 5. Build / run

1. Install the Vulkan SDK so `glslc` is on `PATH`.
2. From `shaders/`, run `compile.bat` to produce the `.spv` files.
3. `cargo run` from the project root.

**Acceptance:** an 800×600 window shows a black background with a red/green/blue interpolated triangle in the center, no validation errors in a debug build, clean shutdown on close.

---

## Files to create / modify

- Modify `Cargo.toml` — add the four dependencies.
- Create `shaders/triangle.vert`, `shaders/triangle.frag`, `shaders/compile.bat`.
- Replace `src/main.rs` with the winit `ApplicationHandler` entry point.
- Create `src/app.rs`, `src/vulkan/mod.rs`, `src/vulkan/context.rs`, `src/vulkan/swapchain.rs`, `src/vulkan/pipeline.rs`, `src/vulkan/renderer.rs`.

---

## Risks / open questions

- **ash 0.38 minor API shifts.** Extension wrappers moved under `ash::khr::*` / `ash::ext::*` and a few function-pointer signatures changed vs 0.37. Exact module paths will be confirmed against the installed crate's docs during implementation.
- **ash-window 0.13** is the correct pairing for ash 0.38 + rwh 0.6; pin it explicitly if the resolver picks otherwise.
- The modular `src/vulkan/*` layout is the default. If you'd prefer a single large `main.rs`, say so before implementation begins.
