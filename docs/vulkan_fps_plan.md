# Plan: Cube + Floor Scene with FPS Fly Camera

Final artifact also written to docs/vulkan_fps_plan.md during implementation.

## 1. Goals
- Replace triangle with a unit colored cube (per-face colors) at origin.
- Large flat ground plane at y = 0.
- Free-fly FPS camera: mouse look (pitch/yaw), WASD, Space/LShift up/down.
- Left click grabs and hides cursor. Alt+Z releases. Initial state: unlocked.

## 2. Coordinate System and Math (critical)

We use a **left-handed (LH), Y-up** world. This matches DirectX / Unity / Unreal conventions and makes "+Z is forward (into the screen)" — which is the most natural mental model for an FPS camera (W moves you toward what you are looking at, in +forward).

### 2.1 World: left-handed, Y-up
+X right, +Y up, +Z forward (into the screen, away from a viewer at the origin looking toward +Z). Meshes are wound **CCW from outside** (as the observer sees them). Outward normals follow the LH right-hand-of-thumb rule mirrored: normal = edge1 x edge2 with the LH cross product convention. In practice we just enumerate face vertices so a viewer outside the face sees CCW; this is coordinate-system independent as a *visual* statement, but the numerical index list differs from an RH cube (every face's vertex order is reversed relative to its RH counterpart, because +Z now points the opposite way).

### 2.2 View
`Mat4::look_to_lh(eye, forward, Vec3::Y)`.

Forward from yaw/pitch — yaw measured around +Y (positive yaw turns from +Z toward +X), pitch around the camera's right axis (positive pitch tilts up toward +Y):

    forward = quat * Vec3::Z  where  quat = Quat::from_euler(YXZ, yaw, pitch, 0)

At yaw=0, pitch=0 this gives (0, 0, +1) — the LH "forward".
At yaw=+90 deg, pitch=0 this gives (+1, 0, 0) — turn right.

Right vector in LH: the implementation uses `self.quat * Vec3::X` for consistency with the cached quaternion.
- glam's `Vec3::cross` is the standard (RH) cross product. In an LH frame, the LH right-hand pair is `up x forward = right` numerically, which is exactly `Vec3::Y.cross(forward)` using the same numerical formula. (Equivalently: in RH, `right = forward x up`; flipping Z to LH negates one factor and one result, so `right = up x forward` in LH.) Verify at yaw=0: forward=(0,0,1), Y=(0,1,0), Y.cross(forward) = (1,0,0). Correct: right is +X.

### 2.3 Projection (Vulkan-friendly Z in [0, 1], no Y flip)
Use `glam::Mat4::perspective_lh(fov_y, aspect, znear, zfar)` directly. **No projection Y flip is applied.**

Verified from glam's source, the matrix is

    [ w   0   0          0  ]
    [ 0   h   0          0  ]
    [ 0   0   r   -r.z_near  ]
    [ 0   0   1          0  ]

with `r = z_far/(z_far - z_near)` and `h = cot(fov_y/2) > 0`. So:
- For a view-space vertex `(x, y, z, 1)` with `z > 0` (in front of the camera), `clip = (w.x, h.y, r.z - r.z_near, z)` and NDC = `(w.x/z, h.y/z, ...)`.
- `view.z = z_near` -> `ndc.z = 0`; `view.z = z_far` -> `ndc.z = 1`. Vulkan-correct depth, no extra work.
- View-space `+y` maps to NDC `+y` (no flip in this matrix).

### 2.4 Winding — choose front_face deliberately

Front-face determination in Vulkan is decided by the **sign of the triangle's 2D area in NDC X-Y**.

Trace the parity for an LH cube wound CCW-from-outside, using the actual matrices from 2.2 and 2.3:

1. World: face vertices wind CCW as seen by an outside observer (LH world: `+X` right, `+Y` up, `+Z` forward).
2. After `look_to_lh` (orientation-preserving — its top-3x3 has det = +1; verified by hand at the canonical `dir=(0,0,1), up=(0,1,0)` pose where it reduces to the identity): a face whose outward normal points toward the camera still winds CCW in view space.
3. After `perspective_lh` (no Y flip applied): the X-Y NDC mapping is `(x, y) -> (w.x/z, h.y/z)` with `w, h, z > 0` for any visible vertex. Positive-scale 2D map -> orientation-preserving. Front faces remain **CCW in NDC**.
4. Viewport transform (negative `height`): `y_f = (-p_y/2).y_d + (p_y + o_y)` is monotonic-decreasing; this reverses orientation. Front faces become **CW in framebuffer coords**.

Result: visible front faces have **CW winding** in framebuffer coordinates (due to the negative viewport height flip).

Pipeline state:

    front_face = vk::FrontFace::CLOCKWISE
    cull_mode  = vk::CullModeFlags::BACK

The negative viewport height is applied to match DirectX NDC orientation so the same MVP produces the same image across APIs.

### 2.5 MVP
Single Mat4 push constant: `mvp = proj * view * model`. 64 bytes, well under the 128-byte minimum.

## 3. Dependencies (Cargo.toml)
- glam = "0.32"
- bytemuck = { version = "1", features = ["derive"] }

## 4. File-by-file changes

### 4.1 Shaders (shaders/)
New `scene.vert` / `scene.frag`; update `compile.bat`. Old `triangle.*` can be deleted.

scene.vert:

    #version 450
    layout(location = 0) in vec3 inPos;
    layout(location = 1) in vec3 inColor;
    layout(push_constant) uniform PC { mat4 mvp; } pc;
    layout(location = 0) out vec3 vColor;
    void main() { gl_Position = pc.mvp * vec4(inPos, 1.0); vColor = inColor; }

scene.frag: passes color through (same as current `triangle.frag`).

### 4.2 src/mesh.rs (new)
- `#[repr(C)] struct Vertex { pos: [f32; 3], color: [f32; 3] }` with bytemuck `Pod + Zeroable` derives.
- `Vertex::binding_description()` -> stride 24, VERTEX input rate.
- `Vertex::attribute_descriptions()` -> loc 0 R32G32B32_SFLOAT pos @0, loc 1 R32G32B32_SFLOAT color @12.
- `pub fn cube(size: f32) -> (Vec<Vertex>, Vec<u32>)`: 24 verts (4 per face for crisp per-face colors), 36 indices, **CCW from outside in LH world**. Face colors: +X red, -X dark red, +Y green, -Y dark green, +Z blue (the far face from a camera at the origin looking forward), -Z dark blue (the near face).

  Implementation uses a helper `fn face(verts, idx, p0..p3, color)` that pushes 4 vertices and 6 indices `(b+0, b+1, b+2, b+0, b+2, b+3)`. A `debug_assert` verifies the face normal `(p1-p0) x (p2-p0)` (standard glam cross) points away from the cube center.

  Corrected vertex table (verified against the debug_assert):

        +X (right, red):    ( h, -h, -h), ( h,  h, -h), ( h,  h,  h), ( h, -h,  h)
        -X (left, dk red):  (-h, -h,  h), (-h,  h,  h), (-h,  h, -h), (-h, -h, -h)
        +Y (top, green):    (-h,  h, -h), (-h,  h,  h), ( h,  h,  h), ( h,  h, -h)
        -Y (bot, dk green): (-h, -h,  h), (-h, -h, -h), ( h, -h, -h), ( h, -h,  h)
        +Z (far, blue):     ( h, -h,  h), ( h,  h,  h), (-h,  h,  h), (-h, -h,  h)
        -Z (near, dk blue): (-h, -h, -h), (-h,  h, -h), ( h,  h, -h), ( h, -h, -h)

- `pub fn floor(half: f32, y: f32, color: [f32; 3]) -> (Vec<Vertex>, Vec<u32>)`: large quad in the XZ plane, 4 verts, 6 indices, **CCW from above (+Y)** in LH. Vertices in order:

        (-half, y, -half), (-half, y, half), (half, y, half), (half, y, -half)

  Indices: (0,1,2, 0,2,3). Verify: looking down from +Y onto the XZ plane in LH (X right, Z away from you / "up" on the projected view), this traversal goes (back-left -> front-left -> front-right -> back-right) which is CCW from above. Outward normal +Y.

### 4.3 src/vulkan/buffer.rs (new)
- `struct GpuBuffer { buffer, memory, size }` with `unsafe destroy(&self, device)`.
- `fn find_memory_type(instance, physical_device, type_filter, props) -> u32`.
- `pub fn create_device_local_buffer<T: Pod>(ctx, command_pool, data, usage) -> GpuBuffer`.

Impl: HOST_VISIBLE|HOST_COHERENT staging -> memcpy -> DEVICE_LOCAL target (`usage | TRANSFER_DST`) -> one-shot cmd `cmd_copy_buffer` -> submit + `queue_wait_idle` -> destroy staging. Four buffers: cube VB/IB, floor VB/IB.

### 4.4 src/vulkan/pipeline.rs (modify)
- `include_bytes!` paths -> `scene.vert.spv` / `scene.frag.spv`.
- Vertex input uses `Vertex::binding_description` + `attribute_descriptions`.
- Push constant range: stage VERTEX, offset 0, size 64.
- Depth-stencil state: `depth_test_enable = true`, `depth_write_enable = true`, `compare_op = LESS` (we use `perspective_lh` with Z in [0,1] mapped znear->0, zfar->1, so nearer = smaller depth, LESS is correct).
- Set `front_face = CLOCKWISE`, `cull_mode = BACK`. The negative viewport height (see 2.4) reverses framebuffer winding, so CLOCKWISE compensates.
- `create_render_pass(depth_format)`: add depth attachment + `depth_stencil_attachment_ref`; extend subpass dependency with `EARLY_FRAGMENT_TESTS | LATE_FRAGMENT_TESTS` and `DEPTH_STENCIL_ATTACHMENT_WRITE`.
- Viewport and scissor are set as dynamic state in the command buffer (not baked into the pipeline), with a negative viewport height to match DirectX NDC orientation.

### 4.5 src/vulkan/swapchain.rs (modify)
`SwapchainData` gains `depth_image, depth_memory, depth_view, depth_format`.
- Format probe via `get_physical_device_format_properties`: D32_SFLOAT -> D24_UNORM_S8_UINT -> D32_SFLOAT_S8_UINT.
- Create depth image (swapchain extent, USAGE_DEPTH_STENCIL_ATTACHMENT, OPTIMAL), DEVICE_LOCAL memory, depth view (aspect DEPTH).
- Framebuffers: `[color_view, depth_view]`.
- `cleanup_swapchain` destroys depth resources first.

### 4.6 src/vulkan/renderer.rs (modify)
- In `Renderer::new` (after `command_pool`): build cube/floor VB+IB. Store index counts.
- `draw_frame(&mut self, ctx: &VulkanContext, view_proj: Mat4)`.
- Per mesh: `mvp = view_proj * model` (model = IDENTITY for v1); `cmd_push_constants(VERTEX, 0, bytes_of(&mvp_cols))` where `mvp_cols = view_proj.to_cols_array()`; `cmd_bind_vertex_buffers`; `cmd_bind_index_buffer(UINT32)`; `cmd_draw_indexed`.
- Clear values: color (0.02, 0.02, 0.04, 1.0) and depth (1.0, 0).
- Viewport is set dynamically with negative height: `y = extent.height`, `height = -extent.height` to match DirectX NDC orientation.
- Drop destroys the 4 `GpuBuffer`s before the command pool.

### 4.7 src/camera.rs (new)
Fields: `position: Vec3`; `yaw, pitch: f32` (rad; pitch clamp +/- 89 deg); `fov_y: f32`; `quat: Quat`; `move_speed: f32`; `mouse_sensitivity: f32`.

Methods:
- `calculate_quat(yaw, pitch)`: `Quat::from_euler(YXZ, yaw, pitch, 0)` — private helper; result is cached in `self.quat` after every rotation change.
- `forward()`: `self.quat * Vec3::Z` where `quat = Quat::from_euler(YXZ, yaw, pitch, 0)` — LH; yaw=pitch=0 -> (0,0,+1).
- `right()`: `self.quat * Vec3::X` — uses the cached quaternion for consistency.
- `up()`: `self.forward().cross(self.right()).normalize()` — LH up. Verify at yaw=pitch=0: forward=(0,0,1), right=(1,0,0), forward.cross(right)=(0,1,0). Correct: up is +Y.
- `view_matrix()`: `Mat4::look_to_lh(self.position, self.forward(), self.up())`.
- `projection_matrix(aspect)`: `Mat4::perspective_lh(fov_y, aspect, znear, zfar)`. **No Y flip** — see 2.3.
- `view_projection(aspect)`: `projection_matrix(aspect) * view_matrix()`.
- `apply_mouse_delta(dx, dy)`:
  - `self.yaw += dx * sensitivity` (mouse-right increases yaw, turns right)
  - `self.pitch += dy * sensitivity` (winit provides negative `dy` for upward motion, so pitch decreases and the camera looks up)
  - pitch clamped to +/- 89 degrees

Defaults: position (0, 1.6, -3) — sit slightly behind the origin in LH so the cube at the origin is in front of us (+Z forward means "behind" is -Z), yaw 0, pitch 0, fov_y 60 deg, move_speed 4.0, mouse_sensitivity 0.0025.

Movement rule: W = +forward, S = -forward (classic fly cam, includes pitch); D = +right, A = -right; Space = +Y world, LShift = -Y world.

### 4.8 src/input.rs (new)
`InputState { forward, back, left, right, up (Space), down (LShift): bool, mouse_delta: (f64, f64), ctrl_down: bool }`.
`drain_mouse_delta()` returns and zeros.

### 4.9 src/app.rs (modify)
New fields: `camera`, `input`, `mouse_locked` (starts false), `last_frame: Instant`.

Methods:
- `on_keyboard(&KeyEvent)`: set booleans; `KeyZ` press with `ctrl_down` -> release lock.
- `on_mouse_button(...)`: left press while unlocked -> acquire lock.
- `on_device_mouse_motion(dx, dy)`: accumulate when locked.
- `set_mouse_lock(locked)`: `set_cursor_grab(Locked)` with `Confined` fallback on `NotSupported`; `set_cursor_visible(!locked)`.
- `update() -> Mat4`: compute `dt`, drain+apply mouse delta, apply movement, return `view_projection(aspect)`.

`draw_frame`: `let vp = self.update(); self.renderer.draw_frame(&self.ctx, vp);`

### 4.10 src/main.rs (modify)
- Title: "LearnVulkan - FPS Camera".
- Handle `WindowEvent::KeyboardInput`, `MouseInput`, `Focused(false)` (auto-release on focus loss).
- Implement `ApplicationHandler::device_event` -> forward `DeviceEvent::MouseMotion { delta }` to app. Raw deltas are correct for FPS look.
- Keep `ControlFlow::Poll` + redraw loop.

### 4.11 Mouse-lock details
- Left click press -> try `CursorGrabMode::Locked`; on `ExternalError::NotSupported` -> `Confined`. Hide cursor.
- Initial: unlocked, cursor visible, no grab at startup.
- Alt+Z (LAlt held + Z pressed) -> release grab + show cursor.
- `DeviceEvent::MouseMotion` is the look-delta source regardless of grab mode.
- On `Focused(false)` -> auto-release so Alt-Tab does not trap the cursor.

### 4.12 Resize
Aspect per-frame from swapchain extent. Depth image recreated inside `recreate_swapchain`; update `cleanup_swapchain` FIRST.

## 5. Order of implementation
1. Deps + shaders; compile SPIR-V.
2. `vulkan/buffer.rs`.
3. `mesh.rs` (LH cube + floor) + vertex input + push constants in `pipeline.rs`. Draw cube with identity MVP. **Sanity check at this step**: with no view/proj, the cube vertex coordinates are interpreted directly as clip space — the +Z face at z=+0.5 will be clipped if znear>0 in the proj path is bypassed. Easier: skip the identity-MVP step and go straight to step 5's MVP wiring with a fixed test camera. Adjust step order accordingly if it's awkward.
4. Depth attachment + pipeline depth state. Cube occludes correctly.
5. `camera.rs`; plumb `view_proj` through `App::update -> Renderer::draw_frame`
6. Floor mesh + second draw call.
7. `input.rs` + keyboard + `DeviceEvent::MouseMotion` + per-frame update.
8. Cursor lock (click to lock, Alt+Z to release, focus-loss auto-release).
9. Polish.

Every step leaves a runnable program (modulo step 3 caveat above).

## 6. Validation
- Zero validation errors on shutdown.
- Six distinct cube faces; no inside-out or missing faces when orbiting. Specifically:
  - At default position (0, 1.6, -3) with yaw=0, pitch=0 (camera at world-Y=1.6 looking toward +Z): the **near face** (-Z, dark blue) should be visible. The far face (+Z, blue) should be hidden.
  - Strafe to world `+X`: red (+X) becomes visible.
  - Look down (pitch < 0, world-up still in the world's +Y): green (+Y) becomes visible from above; dark green (-Y) from below.
- Floor extends in all directions; no z-fighting; cube visibly above floor when the cube is raised in world Y.
- W moves the camera toward `forward` (toward what the camera is *aimed at* in world space, regardless of how it appears on screen).
- Pitch clamps at +/-89 degrees; no gimbal inversion.
- Resize preserves aspect.
- Click locks+hides cursor; Alt+Z releases+shows.
- Initial launch: cursor visible and free.
- Mouse-X changes yaw (turns left/right), mouse-Y changes pitch (looks up/down).

## 7. Files touched / created

Created:
- `src/mesh.rs`, `src/camera.rs`, `src/input.rs`
- `src/vulkan/buffer.rs`
- `shaders/scene.vert`, `shaders/scene.frag`
- `docs/vulkan_fps_plan.md` (this artifact)

Modified:
- `Cargo.toml`
- `src/main.rs`, `src/app.rs`
- `src/vulkan/mod.rs` (add `pub mod buffer;`)
- `src/vulkan/pipeline.rs`, `src/vulkan/renderer.rs`, `src/vulkan/swapchain.rs`
- `shaders/compile.bat`

Optional cleanup: delete `shaders/triangle.{vert,frag,vert.spv,frag.spv}`.

## 8. Key risks and mitigations
- **LH vs RH cross-product confusion**: glam's `cross` is the RH formula. In LH, `right = up x forward` numerically equals `Vec3::Y.cross(forward)` in glam. Mitigation: explicit unit-test-style sanity checks at the default camera pose in `camera.rs` comments.
- `CursorGrabMode::Locked` unsupported on some systems. Mitigation: fallback to `Confined`; `DeviceEvent::MouseMotion` works in both modes.
- Depth format unsupported. Mitigation: probe + fallback list D32_SFLOAT -> D24_UNORM_S8_UINT -> D32_SFLOAT_S8_UINT.
- Push constant >128 B. Mitigation: push exactly 64 B (one Mat4).
- Leaked depth on swapchain recreate. Mitigation: update `cleanup_swapchain` first.
- **Viewport flip and winding**: negative viewport height reverses framebuffer winding. `front_face` must be `CLOCKWISE` to compensate. Documented in 2.4 and pipeline comments.
