# Code Review — LearnVulkan (2026-06-07)

Read-only review of the entire codebase, grouped by severity.

---

## Bugs (correctness — should fix)

### 🔴 1. Skybox shader applies tonemapping but writes into HDR scene color
**File:** `shaders/skybox.frag:18-19`

```glsl
vec3 color = textureLod(uEnvironmentCubemap, vDirection, 0.0).rgb;
color = acesToneMapping(color);     // ← bug
outColor = vec4(color, 1.0);
```

This contradicts the explicit project contract documented in `CODEBUDDY.md`:
> *"`pbr.frag` outputs **linear HDR** radiance (no tonemapping). … Do not add
> tonemapping or gamma correction to PBR or skybox shaders — both belong in
> the postprocess chain."*

Concrete consequences:
1. The skybox is **clamped to [0, 1]** before being written to the
   `R16G16B16A16_SFLOAT` scene color. Bright sky regions can no longer
   contribute to bloom.
2. The composite pass (`composite.frag`) tonemaps **a second time**:
   `clamp(aces(color))` followed by `aces(color * exposure)`. The skybox is
   double-tonemapped.
3. Exposure has no effect on the sky — it's already saturated.

**Fix:** drop `acesToneMapping(color)` from `skybox.frag`. Output the raw
cubemap sample.

---

### 🔴 2. `MAX_PREFILTER_LOD` hardcoded to 10.0
**File:** `shaders/pbr.frag:144-145`

```glsl
const float MAX_PREFILTER_LOD = 10.0; // prefilter_map has 11 mip levels (0..10)
vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * MAX_PREFILTER_LOD).rgb;
```

The actual mip count comes from the loaded KTX2 (`Cubemap.mip_levels` set by
`header.level_count` in `src/vulkan/ktx2_loader.rs:30`). For the Ennis Khronos
sample at 256×256 specular it's 9 levels (0..8), not 11. If you ever swap to
a 512² environment, it's 10 levels (0..9).

This decoupling means the highest-roughness sample is currently going off the
end of the prefilter chain — sampler `max_lod = mip_levels - 1` clamps the
LOD, but the **mapping** between `roughness ∈ [0,1]` and physical mip is
wrong: a perfectly-rough surface (roughness=1) samples LOD 10 (clamped to
whatever the actual top mip is), while moderately rough (roughness=0.5)
samples LOD 5 — which on a 9-level chain is too blurry, and on an 11-level
chain is correct.

**Fix:** pass `prefilter_max_lod` (i.e. `mip_levels - 1` as `f32`) through
the global UBO and use it in the shader.

---

### 🟡 3. `pipeline.render_pass` becomes a dangling handle after `recreate_swapchain`
**File:** `src/vulkan/renderer.rs:699-758` and `src/vulkan/pipeline.rs:141-145, 272-276`

`PipelineData` stores `render_pass: vk::RenderPass`. In `recreate_swapchain`,
`postprocess.destroy()` destroys the old `scene_render_pass`, but
`self.pipeline.render_pass` and `self.skybox_pipeline.render_pass` still
hold the old handles. The pipelines themselves remain usable (Vulkan
render-pass compatibility rules), and the field is never read again at
runtime — but it's a dangling handle in stored data, which is fragile.

This is currently not exercised because nothing reads
`self.pipeline.render_pass` after construction. Recommend either:
- Refresh `self.pipeline.render_pass = postprocess.scene_render_pass;` at the
  end of `recreate_swapchain`, OR
- Remove the field from `PipelineData` since it's unused after creation.

---

### 🟡 4. Two independent `composite_render_pass` objects exist with the same structure
**Files:** `src/vulkan/renderer.rs:79-80` and `src/vulkan/postprocess/resources.rs:124`

Both `Renderer::new` and `PostProcessResources::new` call
`create_composite_render_pass(...)` independently, producing two distinct
VkRenderPass handles for the same logical pass. Swapchain framebuffers are
bound to the renderer's copy; the composite pipeline is bound to the
postprocess copy; `cmd_begin_render_pass` is called with the postprocess
copy and the renderer's framebuffer.

This works because the two render passes are render-pass-compatible — but
it's wasteful and confusing. Pick one owner (probably `PostProcessResources`)
and pass the handle to `create_swapchain`.

---

### 🟡 5. `acquire_next_image` semaphore not refreshed on failure
**File:** `src/vulkan/renderer.rs:572-586`

```rust
let image_index = match unsafe { ... acquire_next_image(...) } {
    Ok(...) => ...,
    Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
        self.recreate_swapchain(ctx);
        return;
    }
    ...
};
```

When `acquire_next_image` returns `ERROR_OUT_OF_DATE_KHR`, the spec leaves
the wait semaphore in an indeterminate state. The robust pattern is to
destroy and re-create `image_available[frame]` (or use timeline semaphores).
In practice, on most drivers the semaphore is left unsignaled and future use
is fine, but validation layers will sometimes flag this. Common minor bug
shared with the official Vulkan tutorial.

---

### 🟡 6. `descriptor_pool` over-allocates when `num_materials == 0`
**File:** `src/vulkan/descriptors.rs:72-90`

If a glTF file has no materials, `material_descriptor_sets` is allocated
with an empty layouts slice — `vkAllocateDescriptorSets` with
`descriptorSetCount = 0` is allowed by the spec but `ash` may panic in
`unwrap`. Not currently exercised (DamagedHelmet has 1 material), but a
latent edge case.

Same direction: if `gpu_materials.is_empty()`, `create_device_local_buffer`
creates a 0-byte buffer (`src/scene/gltf_loader.rs:251-257`) — not legal in
Vulkan (`size > 0` required by `VK_BUFFER_CREATE_INFO`).

---

### 🟢 7. `Option<DebugMarker>` is always `Some`
**File:** `src/vulkan/context.rs:51`

```rust
let debug_marker = Some(DebugMarker::new(&instance, &device));
```

`debug_marker` is unconditionally constructed. The `Option` indirection
(`debug_marker.as_ref()` pattern in renderer) is a dead code path — the
`None` branch never fires. Either keep the `Option` and gate construction
behind a feature flag (e.g. only enable in release with `--debug-markers`),
or remove the wrapper. `CODEBUDDY.md` says "VK_EXT_debug_utils is enabled in
all builds so RenderDoc markers work in release captures" — so the `Option`
is purely vestigial.

---

### 🟢 8. `PostProcessPass` trait has no implementor
**File:** `src/vulkan/postprocess/pass_trait.rs:11`

`#[allow(dead_code)] pub trait PostProcessPass { ... }`. The trait's only
practical export is `set_viewport_and_bind_pipeline`. The trait itself is
unused. The plan was to factor existing passes into `impl PostProcessPass`,
but the renderer still hand-rolls them in `record_bright_pass`/
`record_blur_passes`/`record_composite_pass`. Either implement and use it,
or delete the trait.

---

## Math & rendering correctness

### ✅ Winding chain
Verified end-to-end against `docs/winding_orientation.md`:
- glTF Z-negate (det = −1) at `src/scene/gltf_loader.rs:186-203`
- transform conjugation `M·T·M` (det = +1) at `src/scene/gltf_loader.rs:320-335`
- LH view + LH projection (both proper)
- Y-flip viewport (det = −1) at `src/vulkan/renderer.rs:915-921`
- `front_face = CCW`, `cull_mode = BACK` at `src/vulkan/pipeline.rs:70-71`

Two improper transforms cancel; helmet ends up CCW in framebuffer. Skybox
cube uses CCW-from-outside indexing with no Z-negate, plus the same Y-flip
viewport. The cube triangles end up CCW in framebuffer when viewed from
inside, so `cull_mode = BACK` is correct (matches the very thorough §S1–S8
derivation in `docs/winding_orientation.md`).

### ✅ Camera math
`Camera::up()` = `forward × right` (`src/camera.rs:44`). With
`quat = identity`, `forward = +Z`, `right = +X`,
`forward.cross(right) = Z × X = +Y` ✓ (algebraic cross is basis-agnostic;
the result happens to be +Y).

`Quat::from_euler(YXZ, yaw, pitch, 0)` is correct for an FPS-style camera
with no roll, with yaw applied first around world Y, then pitch around the
rotated X. ✓

### ✅ Tangent space in `pbr.frag`
Standard re-orthogonalization `T = normalize(T - N·dot(N,T))` followed by
`B = normalize(cross(N, T)) * tangent.w`. ✓

### ⚠️ Per-vertex `transpose(inverse(mat3(model)))` (cost only)
`shaders/pbr.vert:33` recomputes the cofactor matrix per vertex. For
non-skinned meshes this is constant per draw call and could be passed via
push constants/UBO. Negligible at 12k-vertex DamagedHelmet but worth noting.

### ⚠️ `compute_tangents` fallback ignores UVs
`src/scene/gltf_loader.rs:434-449` builds tangents from an arbitrary
perpendicular vector. If a primitive both lacks tangents and has a normal
map, normal-mapped lighting will be wrong. Acceptable as a safety net for
DamagedHelmet (which provides tangents), but noting it.

### ⚠️ `compute_normals` weighting
`src/scene/gltf_loader.rs:407-432` per-triangle-normalizes before summation,
giving uniform weights instead of area weights. Standard practice is area
weighting; current behavior is fine for clean triangle meshes but biases
toward thin slivers.

### ⚠️ `geometrySchlickGGX` k parameter mismatch (intentional)
`shaders/pbr.frag:62-64` uses direct-light k = `(r+1)²/8` (UE4 remap for
direct lighting). `shaders/brdf_lut.frag:43-46` uses IBL k = `α²/2`.

Both are correct **for their respective contexts** — the UE4 paper gives
separate k values for direct lighting vs IBL. Just confirm intent: the
analytic specular path in `pbr.frag` uses `geometrySchlickGGX` with the
direct-light k, and the BRDF LUT integrates with the IBL k. ✓ matches
Karis 2013.

### ✅ glTF metallic-roughness channel mapping
`shaders/pbr.frag:93-94`: `mrSample.b → metallic`, `mrSample.g → roughness`.
Matches glTF 2.0 spec. ✓

### ⚠️ `clamp(occlusion_strength, 0, 1)` in `shaders/pbr.frag:108`
glTF 2.0 doesn't actually require occlusion strength to be ≤ 1 (default is
1, but artists can over-drive). Minor.

---

## Resource management & cleanup

### ✅ Drop ordering
`App` Drop drops `renderer` then `ctx` via `ManuallyDrop`
(`src/app.rs:25-32`). ✓
`Renderer::drop` calls `device_wait_idle`, then destroys
scene → IBL → skybox → main pipelines → UBOs → descriptors → sync →
command pool → postprocess → composite render pass → swapchain. ✓

### ⚠️ Repeated `queue_wait_idle` during asset load
`with_one_time_command` (`src/vulkan/buffer.rs:160-168`) does a full
`queue_wait_idle` per call. KTX2 loader (`src/vulkan/ktx2_loader.rs`) calls
it three times per cubemap (initial layout, per-mip upload loop, final
layout) — 3 cubemaps × ~9 mips each = ~30 queue waits. Texture loader
(`src/vulkan/texture.rs`) does one per texture. For a 5-texture model + 3
cubemaps + BRDF LUT generation, this is ~50+ blocking waits at startup.
Slow but acceptable for one-time load.

### ⚠️ KTX2 staging buffer per mip level
`src/vulkan/ktx2_loader.rs:88-141` creates a fresh staging buffer per mip
level, even when total size is small. A single staging buffer for the full
`level_data` of all mips would be cleaner.

### ✅ Texture mipmap generation
`src/vulkan/texture.rs:185-316` correctly:
- transitions UNDEFINED → TRANSFER_DST_OPTIMAL for the whole image
- copies level 0
- per level: barriers level i-1 to TRANSFER_SRC_OPTIMAL, blits with LINEAR
- final barrier: two sub-ranges (levels 0..n-1 from TRANSFER_SRC_OPTIMAL,
  level n-1 from TRANSFER_DST_OPTIMAL) → SHADER_READ_ONLY_OPTIMAL
- `mip_levels == 1` branch correctly skipped

The blit format check at `src/vulkan/texture.rs:62-74` (asserts
`BLIT_SRC | BLIT_DST` features) is also correctly placed.

### ⚠️ `Cubemap::create_empty` sampler not configurable
The same Cubemap struct is used for the env cubemap (sampled by skybox at
LOD 0), the irradiance map (sampled by `samplerCube`), and the prefilter
map (sampled with explicit LOD). All get the same sampler with
`mipmap_mode = LINEAR` and `max_lod = mip_levels - 1`. Working as intended,
but the sampler is bundled with the image — sharing samplers across
different maps is more efficient if/when this scales.

---

## Validation & robustness

### 🟡 No vertex/index `assert!(buffer != null)` checks before use
Trust-the-loader pattern is fine; just noting that empty-mesh primitives
would panic deep inside Vulkan.

### 🟡 `decode_image` panics on unsupported formats
`src/scene/gltf_loader.rs:282`:
`_ => panic!("Unsupported image format: {:?}", image.format)` covers `R16`
and other rare formats. DamagedHelmet uses RGBA8 ones, so OK.

### 🟢 `descriptor_pool` capacity
`src/vulkan/descriptors.rs:72-90`: `frames * 4` global samplers +
`num_materials * 5` material samplers. For DamagedHelmet (1 mat), pool
fits exactly. No headroom for overflow but matches actual demand.

### 🟢 Postprocess pool sizing math
`src/vulkan/postprocess/resources.rs:373-396`:
- `ubo_count = MAX_FRAMES_IN_FLIGHT (= 2)` ✓
- `sampler_count = num_swapchain_images + 16 + num_swapchain_images * 9`
  - bright: `num_swapchain_images × 1`
  - blur: `BLOOM_MIP_COUNT × 2 × 1 = 16`
  - composite: `num_swapchain_images × 9`
  - sum matches ✓
- `total_sets = ubo_count + 2 × num_swapchain_images + 2 × BLOOM_MIP_COUNT`
  - i.e. `2 + 2N + 16`. With 3 swapchain images: `2+6+16 = 24` ✓

### ⚠️ `recreate_swapchain` re-allocates the bloom pyramid every time
Even when window size doesn't change (`SUBOPTIMAL_KHR` round trip), the
bloom pyramid is destroyed and rebuilt. `bloom_extent` could be compared
and the pyramid retained when unchanged. Minor.

---

## Style & API surface

### 🟢 Inconsistent `unsafe fn destroy(&self, ...)` vs `&mut self`
- `BloomPyramid::destroy` takes `&mut self` and drains its Vecs ✓
- `Cubemap::destroy(&self, ...)` does not invalidate fields. After call,
  the struct still has handles to destroyed objects. Same pattern in
  `BrdfLut`, `Texture`. Convention isn't disastrous because Drop isn't
  auto-called and the structs are typically owned by `Scene`/`IblResources`
  which are then themselves dropped, but it would be safer for `destroy`
  to consume `self` (`fn destroy(self, device: &ash::Device)`).

### 🟢 Many unused trailing parameters
`src/vulkan/postprocess/fullscreen.rs:85` `let _ = extent;` — extent is
unused.
`src/vulkan/renderer.rs:1180, 1233, 1359` — `_extent` parameters threaded
through. Minor cleanup opportunity.

### 🟢 `render_pass` field on `PipelineData` could be removed
Not consumed after construction; see Bug §3 above.

### 🟢 `mod.rs` re-exports
`src/scene/mod.rs:6` re-exports
`GpuMaterial`/`PbrMaterial`/`SceneGraph`/`SceneNode`. Consumers (primarily
`gltf_loader.rs`) all sit inside `scene/`, so the re-exports are unused
outside. Could simplify or remove.

### 🟢 Per-mesh push constants set after material descriptor
`src/vulkan/renderer.rs:1037-1073` pushes constants then binds material
set. Either order is valid; just noting.

### 🟢 Skybox is drawn first
`record_command_buffer` draws skybox before PBR
(`src/vulkan/renderer.rs:969-1001`). Standard rendering practice puts the
skybox last with `LESS_OR_EQUAL` so it only fills holes, saving overdraw.
Currently the depth buffer is fully overwritten by PBR meshes after — fine,
just less optimal.

---

## Performance opportunities (non-issues, but flagging)

1. Bloom mip 0 = full screen size. Half-resolution mip 0 saves ~50% memory
   and bandwidth on the most expensive blur pair.
2. `update_descriptor_sets` is called many times in a loop in
   `Renderer::new` (one write per binding) — could be batched into a
   single call.
3. `record_blur_passes` issues 16 separate `cmd_begin_render_pass` calls
   with the same render pass. A multi-attachment pass + subpasses, or
   tighter batching, would help on tiled GPUs (irrelevant on desktop).
4. `with_one_time_command` does `queue_wait_idle` per call. A single shared
   upload command buffer + final fence wait would reduce startup time
   meaningfully.

---

## Summary of recommended actions

| Priority | Item |
|---|---|
| 🔴 High | Remove ACES tonemap from `shaders/skybox.frag` (Bug §1) |
| 🔴 High | Pass `prefilter_max_lod` via UBO instead of hardcoded `10.0` (Bug §2) |
| 🟡 Med | Refresh `pipeline.render_pass` after `recreate_swapchain` (Bug §3) |
| 🟡 Med | Consolidate the duplicate `composite_render_pass` (Bug §4) |
| 🟡 Med | Recreate `image_available` semaphore on `OUT_OF_DATE` (Bug §5) |
| 🟢 Low | Drop the `Option<DebugMarker>` wrapper or gate construction (Bug §7) |
| 🟢 Low | Use or delete `PostProcessPass` trait (Bug §8) |
| 🟢 Low | Asset-time guards: empty materials/empty mesh primitives (Bug §6) |

Overall, the project is solidly architected: cleanup ordering is correct,
the winding/coordinate documentation is excellent and matches the
implementation, postprocess descriptor-set lifetime management is clean,
and the IBL split-sum implementation matches the Khronos reference. The
two real correctness bugs (skybox tonemapping, hardcoded prefilter LOD)
are localized and easy to fix.
