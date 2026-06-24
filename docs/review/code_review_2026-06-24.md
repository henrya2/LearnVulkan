# Code Review — LearnVulkan

Date: 2026-06-24
Scope: All Rust source (~6,300 LOC), GLSL shaders (10 files), build config, docs.

Methodology: Direct reading of every source file plus four parallel deep-dive
reviews (renderer, scene/IBL/KTX2, postprocess, shaders). Findings
cross-referenced against `CODEBUDDY.md`, `docs/winding_orientation.md`, and the
shader buffer layout rule.

---

## Summary

The codebase is architecturally sound. The RH→LH conversion, PBR shading, IBL,
tonemapping, drop ordering, and shader buffer layout rule are all correct.
All High and Medium-severity issues have been fixed. The remaining items are
Low-severity robustness gaps, doc drift, latent bugs in rarely-exercised code
paths, and style items.

| Severity | Count |
|----------|-------|
| Critical | 0 |
| High     | 0 |
| Medium  | 0 |
| Low      | 22 |

---

## High

### H1. Cross-frame bloom pyramid read-write hazard

**Files:** `renderer.rs:594-749` (draw_frame), `resources.rs:111` (single shared `BloomPyramid`).

The bloom `mip_image` and `temp_image` are allocated **once** and shared across
all frames in flight. With `MAX_FRAMES_IN_FLIGHT = 2`, the `in_flight` fence
waited at the top of `draw_frame` (line 608) only guarantees frame N-2 has
completed — frame N-1 may still be executing on the GPU (Vulkan §7.9.1:
submissions to a single queue may overlap).

The hazard: frame N-1's composite pass **reads** `bloom.mip[0..7]` via
`composite_input_sets`. Frame N's bright pass **writes** `bloom.mip[0]` as a
color attachment with `initial_layout = UNDEFINED` (`passes.rs:85`), which
discards prior contents and performs a layout transition. The
`postprocess_color_pass` subpass dependency (`passes.rs:107-118`) only
synchronizes within the same command buffer — it does **not** provide
cross-command-buffer synchronization.

If the GPU overlaps frame N-1's composite read with frame N's bright write,
frame N-1 can read torn or stale bloom data. This is a write-after-read hazard
per Vulkan sync rules. Synchronization validation (not enabled by default;
GPU-assisted validation alone won't catch it) would flag this.

**Note:** Scene color images are correctly per-swapchain-image
(`resources.rs:168-217`). Only the bloom pyramid is shared.

**Status:** Fixed — BloomPyramid is now per-frame-in-flight (`bloom: Vec<BloomPyramid>`),
with per-frame framebuffers and descriptor sets indexed by `frame` (not `image_index`).
See commit for details.

---

## Medium

### M1. `CODEBUDDY.md` — `gpu-allocator` integration undocumented

The entire `gpu-allocator` 0.28 integration (commit `efb2b86`) is absent from
`CODEBUDDY.md`. The doc still describes the old `Drop for Renderer` pattern.
The codebase now uses `MemoryAllocator`, `OwnedBuffer`/`OwnedImage` with
explicit `destroy(device, allocator)` methods, and `Renderer::destroy` called
from `App::drop`. The "Cleanup order matters" section is stale.

**Status:** Fixed — added `memory.rs` module documentation and updated cleanup section
to reflect `Renderer::destroy` pattern.

### M2. `CODEBUDDY.md` — glam version stale (0.32 → 0.33)

`CODEBUDDY.md`: `glam 0.32`. `Cargo.toml`: `glam = "0.33"`. Commit `dbcb5ce`
bumped the dependency without updating the doc.

**Status:** Fixed — `CODEBUDDY.md` now reads `glam 0.33`.

### M3. `src/scene/gltf_loader.rs:282-286` — R8G8 image decode produces wrong channels

Expands `[R, G]` → `[R, R, R, G]`. The PBR shader (`pbr.frag:87-88`) reads `.b`
for metallic and `.g` for roughness (ORM convention). For an R8G8
metallic-roughness texture, `.g` gets R (metallic) instead of G (roughness).
Not exercised by DamagedHelmet (uses R8G8B8A8) but is a latent correctness bug.

**Status:** Fixed — changed to `[0, chunk[1], chunk[0], 255]` (R→B=metallic, G→G=roughness per ORM).

### M4. `src/vulkan/ktx2_loader.rs:20` — No supercompression scheme check

`header.supercompression_scheme` is never validated. The `ktx2` 0.3 crate does
not auto-decompress. A Zstd-compressed KTX2 would upload compressed bytes as
pixel data. Ennis assets are uncompressed; latent.

**Status:** Fixed — added `assert!(header.supercompression_scheme.is_none())`.

### M5. `src/vulkan/ktx2_loader.rs:86-95` — No bounds check on level data vs face size

`face_size_bytes` is computed but never validated against `level_data.len()`.
A truncated file would read past the staging buffer — GPU reads garbage,
validation error.

**Status:** Fixed — added `assert!(level_data.len() as u64 >= 6 * face_size_bytes)`.

### M6. `src/vulkan/postprocess/pyramid.rs:33` — Bloom mip count not validated against extent

For `max(w,h) < 128` (e.g. `--resolution=64x64`), `floor(log2(64))+1 = 7 < 8`,
causing a validation error at `create_dedicated_image` (VUID
`VUID-VkImageCreateInfo-mipLevels-02294`).

**Status:** Fixed — added assertion requiring `max(w,h) >= 128` with a clear error message.

### M7. `src/vulkan/renderer.rs:773-786` — Aliasing UB in `create_swapchain` call site

```rust
let ctx_ptr: *mut VulkanContext = ctx as *mut VulkanContext;
let surface_loader_ptr: *const ash::khr::surface::Instance =
    &ctx.surface_loader as *const _;
let swapchain = create_swapchain(
    unsafe { &mut *ctx_ptr },
    unsafe { &*surface_loader_ptr },
    ...
);
```

Forms simultaneous `&mut VulkanContext` and `&surface_loader` (which borrows
from the same `ctx`) that alias the same allocation. Rust's aliasing rules
forbid `&mut` coexisting with any other reference to overlapping data — this
is UB even if the fields accessed are disjoint.

**Status:** Fixed — `create_swapchain` now takes sub-borrows (`device`, `instance`, `physical_device`, `allocator`, `surface_loader`, ...). Both call sites in `Renderer::new` and `recreate_swapchain` use simple split borrows without raw pointers.

### M8. `src/vulkan/renderer.rs:663` — `camera_pos.w = 1.0` contradicts documented convention

`pbr_ubo.rs:18-19` documents: "`.w` is a reserved slot ... The CPU leaves it at
0." The renderer writes `1.0`. The shader doesn't read `.w`, so functionally
harmless, but violates the stated 1:1 contract and makes the doc comment false.
`light_dir.w = 0.0` (line 664) is correctly zero.

**Status:** Fixed — changed `camera_pos.w` from `1.0` to `0.0` to match the documented convention.

### M9. `src/vulkan/renderer.rs:751-842` — `recreate_swapchain` doesn't guard against zero extent

If `vkAcquireNextImageKHR` returns `ERROR_OUT_OF_DATE_KHR` when the window is
minimized (surface extent = 0x0), `create_swapchain` will attempt a zero-extent
swapchain, which the spec disallows. The `.unwrap()` would panic. The
`on_resize` handler guards against 0x0, but the acquire-Err path does not.

**Status:** Fixed — added surface capabilities query at the top of `recreate_swapchain`; returns early if extent is zero.

### M10. `src/vulkan/pbr_ubo.rs:42-50` — `GlobalUniforms` doc comment is factually wrong

Claims "the struct is 160 B" and "10 × 16 = 176". The struct is 176 B
(2×Mat4 + 3×Vec4 = 11 × 16). The assertion at line 136
(`assert!(size_of == 176)`) directly contradicts the "160 B" claim.

**Status:** Fixed — rewrote the paragraph to correctly state 176 B = 11 × 16 with no "160 B struct" fiction.

### M11. `shaders/postprocess/composite.frag:93-101` — `reinhardJodie` is non-standard

Documented as "Reinhard-Jodie (2015)" but the implementation uses luminance
Reinhard and a fixed whitepoint scaling, not the canonical per-channel Jodie.
Valid tonemap, but the name and reference are misleading.

**Status:** Fixed — renamed to `reinhardLuminance` with updated doc comment describing luminance Reinhard with per-channel color preservation. Shader recompiled.

---

## Low

### L1. `src/vulkan/descriptors.rs:34` — Env cubemap binding over-permissioned
Binding 5 uses `VERTEX | FRAGMENT` but only fragment shaders sample it. Should
be `FRAGMENT` only. Harmless (over-permissioned).

### L2. `src/app.rs:172-178` — `set_mouse_lock` redundant Confined fallback
The `or_else` already tries Confined; the `if mode.is_err()` branch tries it
again. Second attempt is redundant.

### L3. `src/app.rs:186-189` — `dt` unbounded
No upper bound on `dt`. Window pause (drag, minimize) causes camera jump.
**Fix:** `let dt = dt.min(0.1);`

### L4. `src/scene/model.rs:12-24`, `src/vulkan/postprocess/pyramid.rs:107-110` — Unnecessary raw pointers in `destroy`
`BloomPyramid::destroy` uses raw pointers to call `self.mip.destroy()` when
`self.mip.destroy()` would compile directly. `GpuMesh::destroy` uses raw
pointers to mutate through `&self` — UB-adjacent. Take `&mut self` instead.

### L5. `src/mesh.rs:77-117` — `cube()`/`floor()` use CCW-from-outside, violating project rule
`CODEBUDDY.md` mandates CW-from-outside for code-defined geometry. These use
CCW. Currently unused, but if used with the PBR pipeline, geometry would be
culled. Either delete or convert to CW (swap last two indices).

### L6. `src/vulkan/context.rs:257-292` — No physical device features enabled
`enabled_features` not set. `samplerAnisotropy`, `textureCompressionBC`, etc.
are all disabled. Fine for current scope; limits future extensions.

### L7. `src/vulkan/context.rs:199-200` — Debug callback prints INFO severity
Noisy. Most production code filters to `WARNING | ERROR`.

### L8. `shaders/skybox.vert:6-8` — Missing channel-reuse comments
`cameraPos` and `lightDir` have no channel documentation. `pbr.vert`/`pbr.frag`
correctly document them. Should match.

### L9. `shaders/postprocess/bright.frag:13`, `blur.frag:15` — Inconsistent `.w` documentation
Say `.yzw = 0 (dead)`. `composite.frag:23` correctly says `.w` is "the std140
block round-up". Should use consistent wording per channel-reuse policy.

### L10. `shaders/pbr.frag:134` — Diffuse IBL missing `/PI`
`irradiance * kD_ambient * baseColor` omits `/PI`. Correct **only if** the Ennis
`diffuse.ktx2` bakes `1/PI` into the convolution. Verify the convention; add
`/PI` if not baked.

### L11. `shaders/brdf_lut.frag:80` — Potential div-by-zero in `G_Vis`
`(G * VdotH) / (NdotH * NdotV)` — guarded only by `NdotL > 0`, doesn't prevent
`NdotH = 0` or `NdotV = 0`. Add `+ 1e-6` to denominator.

### L12. `shaders/postprocess/bright.frag:26` — `1e-5` added to knee itself
Canonical Frostbite adds `1e-5` only to the denominator. Adding it to `knee`
inflates the knee width. Negligible but deviates from reference.

### L13. `src/vulkan/ktx2_loader.rs:86-141` — One sync point per mip level (performance)
Each mip uses a separate `with_one_time_command` (submit + `queue_wait_idle`).
10-mip cubemap = 10 synchronous sync points. Batch into one command buffer.

### L14. `src/vulkan/postprocess/descriptors.rs:3` — Comment says "set 2", should be "set 1"

### L15. `src/vulkan/postprocess/resources.rs:716` — Typo "DUBO" should be "UBO"

### L16. `src/vulkan/postprocess/resources.rs:611` — Redundant `let _ = ctx;`
`ctx` is already used at line 515. The trailing `let _ = ctx;` is dead.

### L17. `src/vulkan/renderer.rs:742-744` — `framebuffer_resized` not cleared in present Err branch
Unlike the `Ok` branch (line 738), the Err branch doesn't clear the flag. Next
`draw_frame` hits the early-return and calls `recreate_swapchain` redundantly.

### L18. `src/vulkan/renderer.rs:627-631` — Replacement `image_available` semaphore unnamed
When acquire fails with `ERROR_OUT_OF_DATE`, the recreated semaphore is never
passed to `set_object_name`. RenderDoc shows unnamed object after first failure.

### L19. `src/vulkan/renderer.rs:653-655` — `reset_command_buffer` redundant
`begin_command_buffer` implicitly resets if in executable/invalid state. The
explicit reset between fence wait and begin is unnecessary (though legal — pool
has `RESET_COMMAND_BUFFER` flag).

### L20. `src/vulkan/renderer.rs:1080-1125` — Global descriptor set (set 0) bound twice
Bound once for skybox pipeline layout, once for PBR pipeline layout. Both share
the same set-0 layout, so the second bind is a no-op. `CODEBUDDY.md` says
"bound once per command buffer" — doc/impl disagree.

### L21. `src/vulkan/postprocess/resources.rs:616-628` — `update_ubo(&self)` interior mutability
Writes through `self.ubo_mapped[frame]` via `&self`. Technically safe (raw
pointer), but callers can't tell from the signature that the function mutates
state. Consider `&mut self`.

### L22. `examples/sz.rs` — Undocumented, violates shader buffer layout rule
Debugging utility with `f32`/`[f32; 2]` fields. Deliberate (per commit
`a15636b`), but undocumented in `CODEBUDDY.md` and could confuse contributors.

---

## Verified Correct

- **RH→LH conversion** (`gltf_loader.rs:195-203, 331-346`): vertex Z-negate +
  tangent.w flip + transform conjugation. Matches `winding_orientation.md`.
- **Winding/cull mode**: PBR (`BACK`/`CCW`), skybox (`FRONT`/`CCW`), postprocess
  (`NONE`). Two improper transforms cancel for glTF; one for skybox.
- **Sync primitives** (single-frame scope): per-frame `image_available`,
  per-swapchain-image `render_finished`, per-frame `in_flight` fences,
  per-swapchain-image `images_in_flight`. Fence wait → acquire →
  images_in_flight wait → reset → UBO write → submit → present. Correct
  within a single frame (see H1 for cross-frame issue).
- **`ERROR_OUT_OF_DATE` semaphore handling** (`renderer.rs:621-634`): destroys
  and recreates the indeterminate-state semaphore. Spec-correct.
- **Drop ordering**: `App::drop` → `Renderer::destroy` (device_wait_idle →
  scene → IBL → skybox → UBOs → descriptor pool → sync → command pool →
  pipelines → postprocess → composite RP → swapchain) → `ManuallyDrop::drop`.
  Matches `CODEBUDDY.md`. `Drop` is debug-assert-only.
- **Shader buffer layout rule**: `GlobalUniforms`, `PushConstants`,
  `PostProcessUBO`, `BlurPushConstants`, `GpuMaterial` all comply (Mat4/Vec4/
  [Vec4;N] only, `#[repr(C)]` + Pod, no `_pad`, setters/getters, const size
  assertions, GLSL 1:1 mirror).
- **PBR math**: GGX NDF, Smith geometry (direct + IBL k values), Schlick
  Fresnel, split-sum IBL, normal mapping (Gram-Schmidt TBN). All standard.
- **ACES tonemap**: HLSL→GLSL matrix transpose correctly handled.
- **BRDF LUT**: Hammersley + GGX importance sampling, 1024 samples,
  R16G16_SFLOAT output. Correct.
- **Mipmap blit chain** (`texture.rs`): correct barrier sequence, src/dst
  layouts, final two-subrange transition.
- **KTX2 cubemap face ordering**: `base_array_layer = face` matches Vulkan
  cube face indices.
- **Descriptor pool sizing**: both main and postprocess pools sized correctly.
- **Swapchain recreation**: pipelines not recreated (render-pass-compatible);
  tonemap state correctly re-applied after resize.
- **Debug markers**: command buffer labels balanced; object names
  comprehensive.
