# Code Review — 2026-06-06

## BUGS

### 1. Skybox double-tonemapping (sRGB) — CRITICAL

**Files:** `shaders/skybox.frag:17-21`, `shaders/postprocess/composite.frag:63-74`

The skybox fragment shader applies ACES tonemapping directly:

```glsl
// skybox.frag
vec3 color = texture(uEnvCubemap, vLocalPos).rgb;
color = ACESTonemap(color * 1.0);     // <-- tonemapping here
outColor = vec4(color, 1.0);
```

The skybox is drawn into the **same HDR scene color attachment** as PBR geometry, so its output is "pre-tonemapped." Then `composite.frag` applies tonemapping **again** to the combined scene+bloom:

```glsl
// composite.frag
vec3 color = scene + bloom;
color *= pp.exposure;
if (pp.tonemap_op == 1u) { color = ReinhardTonemap(color); }
else if (pp.tonemap_op == 2u) { color = ACESTonemap(color); }
```

Result: the skybox is tonemapped twice — it appears darker, loses dynamic range, and the HDR bloom cannot interact with it correctly (e.g., bright skybox pixels won't trigger bloom at their true HDR brightness).

**Fix:** Remove tonemapping from `skybox.frag`. The skybox should output **linear HDR** with no tonemapping, consistent with the PBR shader's output convention. The postprocess composite pass is the single authority on tonemapping.

---

### 2. Bloom Y-flip chain is fragile

**Files:** `shaders/postprocess/bright.frag:25`, `shaders/postprocess/blur.frag:34`, `shaders/postprocess/composite.frag:48`

Every postprocess shader applies `vec2 uv = vec2(vUV.x, 1.0 - vUV.y)` when sampling previously-rendered images. This compensates for the Y-flip viewport. The chain works because:

1. Bright pass writes `bloom_mip[0]` with Y-flip viewport → image is "upside down" relative to UV (0,0)
2. Blur reads `bloom_mip[0]` with `1.0 - vUV.y` → correctly maps UVs
3. Blur writes `bloom_temp[0]` with Y-flip viewport → upside down again
4. Blur vertical reads `bloom_temp[0]` with `1.0 - vUV.y` → correct
5. Final `bloom_mip[0]` output orientation is consistent with all other mips

The vulnerability: if **any** pass in the chain accidentally uses a positive-height viewport (e.g., during a future refactor or a new postprocess pass), the bloom or scene color will appear vertically flipped **only in that pass**, producing subtle visual corruption that's hard to diagnose.

**Mitigation:** Consider adopting a uniform convention where Y-flip is handled at the viewport level (already done) and also consistently in all shaders (already done), and document this dependency in each fragment shader header. The current code does comment this in `bright.frag:5-6` but `composite.frag` lacks an equivalent header comment.

---

## API USAGE / SPEC COMPLIANCE

### 3. `#![allow(unsafe_op_in_unsafe_fn)]` is misused

**Files:** `src/vulkan/renderer.rs:1`, `src/vulkan/postprocess/resources.rs:1`

Rust 2024 requires `unsafe` operations inside `unsafe fn` bodies to be wrapped in explicit `unsafe {}` blocks (the `unsafe_op_in_unsafe_fn` lint). These two files suppress this lint globally.

The functions affected — `record_bright_pass`, `record_blur_passes`, `record_composite_pass` — are declared `unsafe fn` because they call Vulkan functions, not because they have preconditions that callers must verify. The distinction matters:

- `unsafe fn` means "the caller must guarantee safety preconditions"  
- `unsafe {}` means "I'm implementing this function and asserting safety internally"

Since callers don't need to uphold anything special before calling these `record_*` functions (beyond having a valid device/command buffer, which is implicit), they should be **regular functions with explicit `unsafe {}` blocks inside**. The `#![allow]` attributes should be removed.

---

### 4. KTX2 cubemap loading: N+2 separate GPU submits

**File:** `src/vulkan/ktx2_loader.rs:82-141`

Each mip level of the cubemap is uploaded with a separate `with_one_time_command`, which does:

```
allocate command buffer → begin → record → end → submit → wait idle → destroy
```

For the Ennis cubemaps (many mip levels), this means dozens of GPU idle stalls. Each `with_one_time_command` blocks the entire graphics queue until the single upload completes.

**Fix (performance, not correctness):** A single command buffer submission can handle all levels. The staging buffers can be kept alive until the single submit completes, then freed. The preexisting `with_one_time_command` infrastructure works because it submits and waits, guaranteeing staging buffer lifetimes. The fix would be:

1. Create one command buffer
2. Record all copies (all levels, all faces, all barriers) 
3. Submit once and wait
4. Destroy all staging buffers

---

### 5. `create_shader_module` triplicated

Three identical implementations:

| File | Line |
|---|---|
| `src/vulkan/pipeline.rs` | 9-18 |
| `src/vulkan/brdf_lut.rs` | 327-341 |
| `src/vulkan/postprocess/fullscreen.rs` | 122-130 |

```rust
pub(crate) fn create_shader_module(device: &ash::Device, code: &[u8]) -> vk::ShaderModule {
    let info = vk::ShaderModuleCreateInfo::default()
        .code_size(code.len())
        .code(unsafe { std::slice::from_raw_parts(code.as_ptr() as *const u32, code.len() / 4) });
    unsafe { device.create_shader_module(&info, None).unwrap() }
}
```

Move to a shared utility module (e.g., `src/vulkan/shader.rs`).

---

### 6. Scene framebuffers share a single depth view across all swapchain images

**File:** `src/vulkan/postprocess/resources.rs:187`

```rust
let attachments = [view, depth_view];
```

All scene framebuffers for different swapchain images share the same `depth_view`. This is correct for traditional forward rendering — the depth buffer is cleared each frame and its contents are not needed across frames. With `MAX_FRAMES_IN_FLIGHT = 2` and proper fence synchronization, there are no hazards. However, increasing `MAX_FRAMES_IN_FLIGHT` beyond the swapchain image count would create a race on the depth buffer.

This is not a bug, but worth documenting.

---

### 7. `update_ubo` takes `&self` but writes through a raw pointer

**File:** `src/vulkan/postprocess/resources.rs:591-603`

```rust
pub fn update_ubo(&self, frame: usize, settings: &PostProcessSettings) {
    unsafe {
        std::ptr::copy_nonoverlapping(
            &settings.ubo as *const PostProcessUBO as *const u8,
            self.ubo_mapped[frame],
            std::mem::size_of::<PostProcessUBO>(),
        );
    }
}
```

The method takes `&self` but mutates memory through `self.ubo_mapped[frame]` — a `*mut u8`. This is technically safe in the current single-threaded winit event loop, but violates Rust's shared-reference guarantees. If usage expands to multi-threading, this would be a data race.

**Fix:** Change to `&mut self` or document the synchronization invariant.

---

## SHADER ISSUES

### 8. PBR shader hardcodes 64-material limit

**File:** `shaders/pbr.frag:22-24`

```glsl
layout(set = 0, binding = 1) uniform MaterialBlock {
    Material materials[64];
};
```

The Rust-side asserts `materials.len() <= 64` (`gltf_loader.rs:244`). This will panic at load time for scenes with >64 materials.

**Fix:** Could be either (a) a runtime fallback that batches materials or uses a different indexing scheme, or (b) a runtime check with a clear error message suggesting the user split the model. The current panic with a custom message is acceptable for now.

---

### 9. Composite shader Y-flip applies to bloom mips unnecessarily

**File:** `shaders/postprocess/composite.frag:48`

```glsl
vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
```

This flips the UV for **all** textures — scene color (binding 0) and all 8 bloom mips (bindings 1-8). Since bloom images were written through the **same** Y-flip viewport and bloom shaders already apply `1.0 - vUV.y` when reading their inputs, the bloom mips have gone through an even number of Y-flips in the chain. Reading them again with `1.0 - vUV.y` maintains an odd number of flips **if and only if** every pass uses the Y-flip viewport.

This works correctly currently, but the compensation chain is brittle (see issue #2).

---

## ARCHITECTURE / DESIGN

### 10. `Renderer` struct has 28 fields

**File:** `src/vulkan/renderer.rs:38-68`

The struct is very large. Consider grouping into sub-structs:

```rust
struct FrameState {
    frames_in_flight: usize,
    current_frame: usize,
    images_in_flight: Vec<vk::Fence>,
    image_available_semaphores: Vec<vk::Semaphore>,
    render_finished_semaphores: Vec<vk::Semaphore>,
    global_uniforms: Vec<GpuBuffer>,
    global_mapped: Vec<*mut u8>,
    postprocess_mapped: Vec<*mut u8>,
}

struct GeometryState {
    skybox_vertices: Option<GpuBuffer>,
    skybox_indices: Option<GpuBuffer>,
    skybox_index_count: u32,
}
```

This would improve readability and make the drop order more explicit.

---

### 11. `record_*_pass` functions should be methods on `PostProcessResources`

**File:** `src/vulkan/renderer.rs:1173-1405`

`record_bright_pass`, `record_blur_passes`, and `record_composite_pass` are free functions in `renderer.rs` that take `&PostProcessResources` as a parameter. They would be more discoverable and encapsulated as methods:

```rust
impl PostProcessResources {
    pub unsafe fn record_bright_pass(&self, ...) { }
    pub unsafe fn record_blur_passes(&self, ...) { }
    pub unsafe fn record_composite_pass(&self, ...) { }
}
```

This would also reduce `renderer.rs` significantly.

---

### 12. Unused parameters

| File | Function | Unused parameter |
|---|---|---|
| `src/vulkan/pipeline.rs:23` | `create_pbr_pipeline` | `_extent` |
| `src/vulkan/pipeline.rs:151` | `create_skybox_pipeline` | `_extent` |
| `src/vulkan/postprocess/fullscreen.rs:85` | `create_fullscreen_pipeline` | `extent` (consumed by `let _ = extent;`) |
| `src/vulkan/renderer.rs:1175` | `record_bright_pass` | `_debug_marker` |
| `src/vulkan/renderer.rs:1359` | `record_composite_pass` | `_debug_marker` |

These are minor distractions. Either remove them or use a consistent prefix convention (`_` for intentionally unused).

---

### 13. `PostProcessPass` trait is unused

**File:** `src/vulkan/postprocess/pass_trait.rs:12`

Marked `#[allow(dead_code)]`. The trait defines a clean abstraction (`name()`, `render_pass()`, `pipeline()`, `pipeline_layout()`, `record()`) but none of the three existing passes — `record_bright_pass`, `record_blur_passes`, `record_composite_pass` — implement it.

Either implement the trait for the existing passes or remove it until needed. The trait is well-designed and implementing it now would enforce the abstraction and make future passes (FXAA, motion blur, etc.) easier to add.

---

## PERFORMANCE

### 14. KTX2 loading stalls GPU per mip level

Covered in issue #4. For the Ennis environment (3 cubemaps, each with ~9-10 mip levels), startup could stall the GPU ~30+ times before the first frame. This is only a startup-time issue, not a per-frame problem.

---

### 15. Bloom pyramid destroyed and recreated on every swapchain resize

**File:** `src/vulkan/renderer.rs:740-758`

When the swapchain is recreated (e.g., after `SUBOPTIMAL_KHR`), `PostProcessResources` is entirely destroyed and recreated. If the window extent didn't change (just a suboptimal present was detected), the bloom pyramid and postprocess pipelines are unnecessarily recycled. The scene color images must be recreated (they depend on the new swapchain images), but the bloom pyramid, pipelines, and descriptor sets don't change at all when the extent is identical.

**Minor optimization:** Check if extent actually changed before destroying bloom resources.

---

## DOCUMENTATION / COMMENTS

### 16. Incorrect comment in descriptor layout

**File:** `src/vulkan/postprocess/descriptors.rs:3`

```rust
/// Create the postprocess UBO descriptor set layout (1 UBO at set 2).
pub fn create_postprocess_ubo_layout(device: &ash::Device) -> vk::DescriptorSetLayout {
```

The UBO is at **set 1**, not set 2. The comment should read `"1 UBO at set 1"`.

---

## CORRECTNESS CHECKS (VERIFIED)

These areas were checked and found to be correct:

### Uniform Layouts

All uniform buffer structs match their GLSL layouts:

| Struct | Rust size | GLSL size | Match? |
|---|---|---|---|
| `GlobalUniforms` | 160 B | 160 B | ✓ |
| `GpuMaterial` | 64 B | 64 B | ✓ |
| `PushConstants` | 80 B | 80 B | ✓ |
| `PostProcessUBO` | 64 B | 64 B | ✓ |
| `BlurPushConstants` | 16 B | 12 B + 4 B overflow | ✓ |

### Viewport Y-flip

```rust
let viewport = vk::Viewport::default()
    .x(0.0)
    .y(extent.height as f32)         // start at bottom
    .width(extent.width as f32)
    .height(-(extent.height as f32)) // grow upward
```

Per Vulkan 1.3 §27.5: with `y = H` and `height = -H`, the framebuffer transform is:
```
y_f = H + (-H) * (y_d + 1) / 2 = H/2 * (1 - y_d)
```

| NDC `y_d` | Framebuffer `y_f` | Screen position |
|---|---|---|
| -1 (top in y-down NDC) | H | bottom |
| +1 (bottom in y-down NDC) | 0 | top |

The Y-axis is reflected (det = -1), inverting winding once. Combined with the glTF Z-negate (det = -1), the two improper transforms cancel. The pipeline's `front_face = CCW` + `cull_mode = BACK` is correct. See `docs/winding_orientation.md` for the full derivation.

### PBR Metallic-Roughness Channel Mapping

`shaders/pbr.frag:93-94`:
```glsl
float metallic = mrSample.b * mat.metallicFactor;   // B = metallic (glTF §5.22)
float roughness = mrSample.g * mat.roughnessFactor;  // G = roughness (glTF §5.22)
```

Matches glTF 2.0 spec. ✓

### Skybox Depth Testing

- Skybox drawn first with `LESS_OR_EQUAL` and depth writes **OFF**
- PBR geometry drawn second with `LESS` and depth writes **ON**
- Depth buffer cleared to 1.0 (far plane)
- Skybox pixels pass `z=1.0 <= 1.0` ✓
- PBR geometry overwrites where it renders ✓

### Postprocess Descriptor Layouts

| Pipeline | Set 0 | Set 1 |
|---|---|---|
| Bright pass | Scene color sampler (1) | Postprocess UBO |
| Blur pass | Input sampler (1) | Postprocess UBO + push constants |
| Composite | Scene color + 8 bloom mips (9) | Postprocess UBO |

Consistent with CODEBUDDY.md documentation. ✓

### Renderer Drop Order

`src/vulkan/renderer.rs:804-873` destroys in order:
1. `device_wait_idle`
2. Postprocess resources (pipelines, render passes, descriptor pool, bloom pyramid, scene color images)
3. Scene (meshes, materials, textures, fallback textures, material buffer)
4. IBL resources (BRDF LUT, cubemaps)
5. Skybox vertex/index buffers
6. PBR pipeline/layout
7. Skybox pipeline/layout
8. Global UBOs (unmap first, then destroy)
9. Main descriptor pool/layouts
10. Fences/semaphores
11. Command pool
12. Swapchain

All device-level objects destroyed before the `VulkanContext` device is dropped (enforced by `ManuallyDrop` in `App`). ✓

---

## SUMMARY

| Severity | Count | Issues |
|---|---|---|
| Bug | 1 | [Skybox double-tonemapping](#1-skybox-double-tonemapping-srgb--critical) |
| API/Spec | 3 | [#3 unsafe lint misuse](#3-allowunsafe_op_in_unsafe_fn-is-misused), [#4 KTX2 perf](#4-ktx2-cubemap-loading-n2-separate-gpu-submits), [#5 shader module triplication](#5-create_shader_module-triplicated) |
| Shader fragility | 1 | [#2 Y-flip compensation chain](#2-bloom-y-flip-chain-is-fragile) |
| Architecture | 4 | [#10 large struct](#10-renderer-struct-has-28-fields), [#11 free functions](#11-record__pass-functions-should-be-methods-on-postprocessresources), [#12 unused params](#12-unused-parameters), [#13 dead trait](#13-postprocesspass-trait-is-unused) |
| Performance | 2 | [#4 KTX2 loads](#4-ktx2-cubemap-loading-n2-separate-gpu-submits), [#15 bloom recreation](#15-bloom-pyramid-destroyed-and-recreated-on-every-swapchain-resize) |
| Minor | 2 | [#6 shared depth view doc](#6-scene-framebuffers-share-a-single-depth-view-across-all-swapchain-images), [#16 comment error](#16-incorrect-comment-in-descriptor-layout) |

The most impactful fix is **issue #1** (skybox tonemapping). All other items are correctness improvements, cleanups, or performance optimizations that don't affect visual output in the current configuration.

---

## AFFECTED FILES

| Issue | File(s) |
|---|---|
| #1 | `shaders/skybox.frag` |
| #2 | `shaders/postprocess/bright.frag`, `blur.frag`, `composite.frag` |
| #3 | `src/vulkan/renderer.rs`, `src/vulkan/postprocess/resources.rs` |
| #4 | `src/vulkan/ktx2_loader.rs` |
| #5 | `src/vulkan/pipeline.rs`, `src/vulkan/brdf_lut.rs`, `src/vulkan/postprocess/fullscreen.rs` |
| #6 | `src/vulkan/postprocess/resources.rs` |
| #7 | `src/vulkan/postprocess/resources.rs` |
| #8 | `shaders/pbr.frag`, `src/scene/gltf_loader.rs` |
| #10-13 | `src/vulkan/renderer.rs`, `src/vulkan/postprocess/resources.rs`, `src/vulkan/postprocess/pass_trait.rs`, `src/vulkan/pipeline.rs`, `src/vulkan/postprocess/fullscreen.rs` |
| #15 | `src/vulkan/renderer.rs` |
| #16 | `src/vulkan/postprocess/descriptors.rs` |
