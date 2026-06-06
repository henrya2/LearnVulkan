# Postprocessing Framework Plan

A framework that adds a postprocess chain (Bloom + Tonemapping at minimum) to the
existing PBR renderer. The chain runs after the main PBR render pass and before
present, with a clean extension path for future effects (FXAA, vignette, color
grading, etc.).

## 0. Goals and constraints

The renderer currently produces a final image inside the main PBR render pass
(skybox + helmet) directly into the swapchain image. ACES tonemapping is
hard-coded inside `pbr.frag` (line 85-92, 163) and there is no bloom, no exposure
control, no color grading, no temporal stability, and no way to apply
screen-space effects at all. The aim is a framework that adds bloom +
tonemapping (and a path for future effects), without breaking the existing IBL
pipeline, the winding invariants in `docs/winding_orientation.md`, or the
cleanup ordering in `App::drop` and `Renderer::drop`.

Three design constraints come from the existing project:

1. **Winding contract is fragile.** `docs/winding_orientation.md` §6 and §S7
   establish that both the PBR and skybox pipelines rely on a single Y-flip
   viewport to make outside-facing triangles CCW in framebuffer. Any fullscreen
   pass we add must use the **same** negative-height viewport; otherwise its
   triangles flip front/back and either become invisible or visibly mirrored.
2. **Cleanup order matters.** `app.rs:25-32` drops `renderer` before `ctx`
   because all device objects must be released before the device. Postprocessing
   resources are device objects and must drop inside `Renderer::drop`, before
   pipelines/layouts are destroyed.
3. **Tonemapping is currently inside the PBR shader.** It needs to move out so
   that bloom can be added on linear HDR color, and so the tonemapper and
   exposure can be runtime-tweakable.

The framework satisfies these constraints.

## 1. Architectural overview

A single fullscreen-quad postprocessing chain that runs **after** the main PBR
render pass and **before** present. The chain is data-driven from a
`PostProcessSettings` struct so we can add effects later without re-plumbing.

```
                ┌────────────────────────────────────┐
                │     Main PBR Render Pass           │
                │  (skybox + helmet)                 │
                │  renders to HDR scene color        │
                │  + depth (kept)                    │
                └──────────┬─────────────────────────┘
                           │  vkImage: HDR scene color
                           │  (R16G16B16A16_SFLOAT)
                           ▼
                ┌────────────────────────────────────┐
                │  Pass 1: Bright-pass / threshold   │
                │  (fullscreen, separable input)     │
                │  -> downsample chain → bloom mips  │
                └──────────┬─────────────────────────┘
                           │
                           ▼
                ┌────────────────────────────────────┐
                │  Pass 2: Gaussian blur (separable)│
                │  horizontal + vertical             │
                │  per mip level, ping-pong          │
                └──────────┬─────────────────────────┘
                           │
                           ▼
                ┌────────────────────────────────────┐
                │  Pass 3: Composite                 │
                │  sceneColor + bloom + tonemap      │
                │  + exposure + color grading        │
                │  -> swapchain (sRGB encoding)      │
                └────────────────────────────────────┘
```

Three conceptual "slots" in the chain, each a fullscreen pass. The first two are
computed; the third is the final present.

## 2. Color space, format, and the move to HDR scene color

This is the single most important change. Today `pbr.frag:163` writes
ACES-applied color directly into the sRGB swapchain. We have to remove that and
write **linear HDR** into an intermediate target so that bloom and tonemapping
can work in linear space.

### 2.1 What changes in `pbr.frag`

- Delete the call to `acesToneMapping` at line 163.
- Keep `color` as raw linear HDR radiance. The PBR accumulation
  `color = ambient + Lo + emissive` is unbounded — `lightIntensity` alone is 4.0
  and `Lo` can reach large values.
- The emissive term and the IBL specular term are the main sources of
  "bloom-able" highlights. With the current light intensity of 4.0 and the Ennis
  environment prefilter, peaks around 8–20 are common in linear space. Anything
  above the bloom threshold (default 1.0) will bleed.

### 2.2 New intermediate "scene color" image

A new `vk::Image` per swapchain image, sized to the swapchain extent, format
**`R16G16B16A16_SFLOAT`** (16-bit float per channel), sample count 1, usage:

```
COLOR_ATTACHMENT | SAMPLED | TRANSFER_SRC | TRANSFER_DST
```

- `COLOR_ATTACHMENT` so the main PBR pass can render into it.
- `SAMPLED` so the bright-pass, blur, and composite can read it.
- `TRANSFER_SRC`/`TRANSFER_DST` is reserved for future debug capture / blit-to-host
  reads.

Why not `R32G32B32A32_SFLOAT`? 16-bit float has ~3 decimal digits of mantissa,
dynamic range up to ~65000, and is supported on every desktop GPU as a color
attachment and as a sampled image. 32-bit is overkill for our 4.0 light intensity
and wastes 2× the bandwidth at 800×600 (3.2 MB vs 1.6 MB for 800×600). We may
upgrade to R32 if future HDR work demands it.

Why not `R11G11B10_UFLOAT`? It saves bandwidth, but its 5-bit exponent and no
alpha make tonemapping operators like ACES and especially AgX a touch banded in
the shadows. For this project the 16-bit format is the right balance.

### 2.3 Main PBR render pass change

The current `create_render_pass` (`pipeline.rs:9-73`) uses
`image_format = surface_format.format` (sRGB swapchain) and
`final_layout = PRESENT_SRC_KHR`. We replace it with **two render passes**:

1. **Scene pass**: format = `R16G16B16A16_SFLOAT`, `final_layout = SHADER_READ_ONLY_OPTIMAL`.
   Depth attachment is the existing depth image (kept from the swapchain). The
   framebuffer is `scene_color_view + depth_view` (depth is borrowed from the
   swapchain, not duplicated).
2. **Composite pass** (the postprocess pass): format = `surface_format.format`
   (sRGB), `final_layout = PRESENT_SRC_KHR`, **no depth attachment**, color-only.

The framebuffer for the scene pass is sized to the swapchain extent. Because
the depth image is already allocated per swapchain, we need a scene color view
**per swapchain image** (just like the existing `image_views` in `SwapchainData`).

A `SubpassDependency` is required to transition the scene color from
`COLOR_ATTACHMENT_OPTIMAL` (after the scene pass) to `SHADER_READ_ONLY_OPTIMAL`
(when the composite pass samples it). The same render pass can do this with two
subpasses if we use `vk::SubpassContents::INLINE` with a self-dependency and a
pipeline barrier, but the cleanest approach is **two separate render passes**
chained in the same command buffer — the second pass's initial layout is
`SHADER_READ_ONLY_OPTIMAL` and the dependency is implicit because the barrier
is created by ending the first pass and beginning the second.

The scene pass framebuffers will be created in `create_swapchain` (or in a new
`recreate_postprocess` step that mirrors swapchain recreation) as one new
`Vec<vk::Framebuffer>` alongside the existing `framebuffers`. We will keep the
existing `framebuffers` for the composite pass and the existing `depth_image`/
`depth_view` shared by both passes. The main PBR pipeline is unchanged except
its render-pass handle now points at the new scene pass.

### 2.4 Why we keep the depth image

We keep the existing `depth_image`/`depth_view` (`swapchain.rs:155-216`) as the
scene pass's depth attachment. The composite pass has no depth. The depth image
is still per-swapchain-image and still recreated in `recreate_swapchain`.

## 3. Bloom implementation

Bloom is "extract highlights from HDR scene color, blur them, add back". We'll
use **progressive downsampling** with a **separable Gaussian** per level.

### 3.1 Bloom pyramid layout

8 mip levels, each half the previous dimension, clamped at 1×1:

| Mip | Resolution (for 800×600) |
|-----|--------------------------|
| 0   | 800×600 (input)          |
| 1   | 400×300                  |
| 2   | 200×150                  |
| 3   | 100×75                   |
| 4   | 50×37                    |
| 5   | 25×18                    |
| 6   | 12×9                     |
| 7   | 6×4                      |

8 levels is the sweet spot for soft glow on a 800×600 window. For 1080p or 4K
we may bump this to 10.

Each mip is its own `vk::Image` (not a single image with multiple mip levels)
because:
- We need independent `vk::ImageView`s per mip for sampling in the blur and
  composite shaders.
- We want each mip's image to use a different layout per pass (color attachment,
  sampled, blit src/dst).
- Independent allocation lets us choose a different format if needed and lets
  RenderDoc show clear resources.

All 8 bloom mip images are allocated as `R16G16B16A16_SFLOAT` (matches the
scene color) with usage
`COLOR_ATTACHMENT | SAMPLED | TRANSFER_SRC | TRANSFER_DST`. We don't need
storage image access — every step is a render pass with a fullscreen quad.

### 3.2 The bright-pass

A single fullscreen quad pass that:
1. Samples the scene color (linear HDR).
2. Computes luminance via `dot(color, vec3(0.2126, 0.7152, 0.0722))` (Rec. 709).
3. Subtracts `softThreshold = threshold - knee/4` and multiplies by a smooth
   knee factor to avoid hard banding. The exact formula from Call of Duty /
   Frostbite (公开 literature, "Thresholded Bloom"):
   ```
   float knee = threshold * knee_factor; // knee_factor = 0.5 by default
   float soft = brightness - threshold + knee;
   soft = clamp(soft, 0.0, 2.0 * knee);
   soft = soft * soft / (4.0 * knee + 1e-5);
   float contribution = max(soft, brightness - threshold) / max(brightness, 1e-5);
   vec3 bloom = color * contribution;
   ```
4. Outputs to mip 0 of the bloom pyramid.

The result is bloom-colored highlights only, in linear HDR space, with a soft
transition across the threshold. Default threshold = 1.0 (in linear, before
tonemapping).

### 3.3 The blur (separable Gaussian, 9-tap)

For each mip `i` from 0 to 7 (we blur each mip; we don't just blur mip 0):
1. **Horizontal pass**: bind bloom_mip_i as color attachment, sample from
   bloom_mip_i (or the bright-pass output for i=0), apply a 9-tap Gaussian in
   the X direction, output to a temporary image.
2. **Vertical pass**: bind bloom_mip_i as color attachment, sample the temp
   image, apply a 9-tap Gaussian in Y, output back to bloom_mip_i.

The 9-tap weights (σ ≈ 2.0) are precomputed once on the CPU and uploaded as a
small UBO (one float[9]) per pass, or hard-coded as a constant in the shader.
The offsets are dynamic from a `vec2 uTexelSize` uniform:
`offset[i] = float(i - 4) * uTexelSize.x` for X, `* uTexelSize.y` for Y.

We use a separable blur, not a single 2D pass, because a 9×9 two-pass is 18
taps, a 9×9 single pass is 81 taps. Separable wins by 4.5× on a fullscreen quad.

Why blur each mip level rather than just blurring the input and downsampling the
blur? Because downsampling an already-blurred image produces a "smearing" tail
— the lowest mips have no high-frequency content, and the wider blur at lower
mips gives the characteristic soft glow that distinguishes good bloom from a
cheap one. This matches Unreal's bloom and Call of Duty's "Multi-scale Bloom".

For a 9-tap kernel, each level's cost is 2× fullscreen quads at that mip's
resolution. Total:
`2 * sum(1/4^i) for i in 0..7 ≈ 2 * 1.333 = 2.67` fullscreen-equivalent quads.
Negligible.

### 3.4 The composite (and where tonemapping actually lives)

The composite pass samples:
- `sceneColor` (the HDR scene after the PBR pass).
- `bloomMip[i]` for `i in 0..8`, each with its own intensity weight (a
  `vec2 uBloomWeights[8]` uniform — we can use the actual Gaussian weight of
  that mip, or simpler, a per-mip multiplier for the artist).

The composite shader does:
1. `vec3 bloomed = vec3(0); for i in 0..8: bloomed += texture(bloomMip[i], uv).rgb * uBloomWeights[i];`
2. `vec3 color = sceneColor.rgb + bloomed;`
3. Apply **exposure**: `color *= uExposure;` where `uExposure` is a runtime
   value in stops (so exposure 0 = 1.0×, exposure +1 = 2×, exposure −1 = 0.5×).
4. Apply **tonemapping operator** (see §3.5).
5. Apply **color grading** (optional, future): simple saturation/contrast/gain/
   offset/gamma in linear space.
6. Write to the sRGB swapchain image. The swapchain attachment's sRGB encoding
   is the final gamma step (no manual `pow(color, 1/2.2)` in the shader).

### 3.5 Tonemapping

The tonemapper is now a uniform `int uTonemapOperator` (or a fixed choice) and a
switch in the composite shader. The default is ACES (already implemented in
`pbr.frag:85-92` — we move that exact function to the composite shader). We can
also offer:

- **Linear / none** (1.0, just clamps). Useful for debugging.
- **Reinhard** (`color / (color + 1)`). Cheap, slightly desaturated.
- **ACES filmic** (the existing one). Good default.
- **AgX** (open-source, recently popular). A future addition; requires a LUT or
  the polynomial fit.

The exposure control is the real new feature. Even with bloom off, exposure
alone is a major improvement — the user can navigate from the bright outside of
the Ennis cube to the helmet's dark interior without either blowing out or
crushing to black.

### 3.6 Fullscreen-quad pipeline

We need one vertex shader (fullscreen triangle or quad) reused by every
postprocess pass. Two options:

- **Three-vertex fullscreen triangle** (a single oversized triangle that covers
  the screen; faster, no overdraw on edges). This is the modern Vulkan-tutorial
  standard.
- **NDC quad** with 4 vertices and 6 indices. Simpler, slightly more vertex
  work.

I'll use the **three-vertex triangle** because it has zero overdraw and is one
line of vertex shader. The vertex shader does:

```glsl
vec2 pos = vec2((gl_VertexIndex & 1) * 4.0 - 1.0,
                (gl_VertexIndex & 2) * 2.0 - 1.0);
gl_Position = vec4(pos, 0.0, 1.0);
vUV = pos * 0.5 + 0.5;
// If a Y-flip is required for the composite sampler (it isn't in our case, see §6),
// vUV.y = 1.0 - vUV.y;
// We will keep vUV.y as-is and let the Y-flip viewport handle framebuffer space.
```

No vertex buffer is required. The pipeline uses `vertex_input_state` set to
`None` and the triangle is generated from `gl_VertexIndex`. This is cleaner
than allocating a static vertex buffer.

### 3.7 Pipeline count

For the framework we need:
- 1 fullscreen-quad pipeline (vertex shader = fullscreen triangle, fragment
  shader = one of the postprocess shaders).
- Each postprocess pass changes the fragment shader and possibly the render
  pass / framebuffer / sampler.

Either we use a pipeline per (vertex_shader, fragment_shader) combination, or
we use **pipeline derivatives / dynamic state** to share. Cleanest is one
pipeline per fragment shader: 3 fragment shaders (bright, blur, composite) + 1
vertex shader = 3 pipelines. The blur pipeline has two entry points (horizontal
and vertical passes) controlled by a uniform `int uDirection` so we don't need
two pipelines for that.

That gives us 3 pipelines total, each with a separate `vk::PipelineLayout`
because the descriptor set layouts differ (composite samples 1 + 8 images;
bright samples 1; blur samples 1).

Actually, simpler: 1 vertex shader + 3 fragment shaders + 3 separate
`PipelineLayout`s. The pipeline cache is not required but recommended.

## 4. Frame graph and per-frame data

### 4.1 Resources that need to scale with the swapchain

| Resource | Per swapchain image? | Lifetime |
|---|---|---|
| Scene color image + view | yes | recreated with swapchain |
| Scene color framebuffer | yes | recreated with swapchain |
| Composite framebuffer (existing) | yes | unchanged |
| Bloom mip 0..7 images + views | **no** (depends only on extent) | recreated when extent changes |
| Bloom ping-pong temp image (per mip) | **no** | recreated when extent changes |

Because the bloom mip images depend only on the swapchain extent, they don't
need to be 1:1 with swapchain images. But the scene color image **does** need
to be 1:1 with swapchain images, because the PBR pass writes to it and the
composite pass reads from the specific one that was just written.

### 4.2 Synchronization between passes

Within a single command buffer recorded for one frame:

```
vkCmdBeginRenderPass(scene_pass, ...)    // bind scene framebuffer
  // PBR draws (skybox + helmet)
vkCmdEndRenderPass()

// Implicit barrier from COLOR_ATTACHMENT_WRITE → SHADER_READ on the scene color image,
// because we transition its layout to SHADER_READ_ONLY_OPTIMAL at end-of-pass.

vkCmdBeginRenderPass(composite_pass, ...) // bind swapchain framebuffer
  // Composite fullscreen quad
vkCmdEndRenderPass()

// Present.
```

The implicit pipeline barrier at the end of a render pass transitions the
attachment from `COLOR_ATTACHMENT_OPTIMAL` to the layout we declared as
`final_layout` in the render pass. We declare
`scene_color.final_layout = SHADER_READ_ONLY_OPTIMAL`, so when the composite
pass begins and binds the scene color as a sampled image, no extra
`vkCmdPipelineBarrier` is required.

For the bloom passes, each one is its own render pass (because we need different
color attachments and different shaders). Between the bright-pass end and the
first blur pass we also use the layout-transition-from-final-layout trick:
declare `bloomMip_i.final_layout = SHADER_READ_ONLY_OPTIMAL` after the bright
pass, declare its `initial_layout = SHADER_READ_ONLY_OPTIMAL` for the blur pass.
This keeps things simple.

### 4.3 Descriptors

We need new descriptor set layouts:

- **Bright-pass layout** (set 0): 1 `COMBINED_IMAGE_SAMPLER` for the scene color.
- **Blur layout** (set 0): 1 `COMBINED_IMAGE_SAMPLER` for the input (the
  previous mip or the ping-pong temp).
- **Composite layout** (set 0): 1 `COMBINED_IMAGE_SAMPLER` for scene color + 1
  for each bloom mip (8 total). All `uvec2` array of samplers — actually 8
  separate bindings is fine and matches the existing "explicit bindings" style
  of this project.

We don't need push constants in any of these — the per-frame parameters
(exposure, threshold, intensity, knee, operator choice) are small and go in a
UBO. The existing project's UBO style is bytemuck POD pushed to a host-visible
buffer; we extend that.

A new uniform buffer `PostProcessUBO` (32 bytes, host-coherent, one per frame
in flight, mirrors `global_uniforms`):

```rust
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PostProcessUBO {
    exposure: f32,           // stops
    bloom_threshold: f32,    // linear
    bloom_knee: f32,         // linear
    bloom_intensity: f32,    // multiplier
    bloom_weights: [f32; 8], // per-mip intensity
    tonemap_op: u32,         // 0=linear, 1=reinhard, 2=aces
    _pad: [u32; 3],
}
```

This is part of a **new descriptor set, set 2** (the global set is set 0,
material is set 1, so postprocess is set 2). Each fullscreen pass binds set 2
and the relevant input set. Wait — fullscreen passes don't need set 0 (the
existing global UBO + IBL + env cubemap) at all. We can keep the existing global
UBO bound for the scene pass (it needs view/proj for the PBR draw and the IBL
textures for sampling), and for postprocess passes we bind a fresh `set 0` (or
just `set 1`, doesn't matter) with a new layout that has only the inputs we need.

To avoid messing with the PBR pipeline's layout (it currently uses sets 0 and
1, with specific bindings), the cleanest thing is: **postprocess pipelines use
a single descriptor set at set 0, with a brand-new layout per pass type**. The
PBR pipeline's set 0 (the global layout) and set 1 (the material layout) are
untouched.

To share the per-frame UBO with the existing `GlobalUniforms`, we can use **a
third descriptor set at index 2 in the postprocess pipelines** — that doesn't
conflict with the PBR pipeline's set 0/1. This is the cleanest choice. So:

- PBR pipeline: set 0 (global), set 1 (material). Unchanged.
- Postprocess pipeline (bright/blur): set 0 (input sampler), set 2 (postprocess
  UBO with the 4 floats needed for that pass).
- Postprocess pipeline (composite): set 0 (scene + 8 bloom samplers), set 2
  (postprocess UBO with exposure, weights, operator, etc.).

The descriptor pool needs to grow: add a pool size for `UNIFORM_BUFFER` of
`MAX_FRAMES_IN_FLIGHT` more, and add `COMBINED_IMAGE_SAMPLER` count of 10 ×
`MAX_FRAMES_IN_FLIGHT` more (1 for bright, 1 for blur, 8 for composite, all
per-frame).

### 4.4 What about the descriptor strategy for the per-mip bloom images?

Each frame, we update the **composite descriptor set** with the current bloom
image views. The bloom image views are stable across frames (they're not
per-swapchain-image), so we can update once at swapchain creation. The
composite set is per-frame in flight, so we have `MAX_FRAMES_IN_FLIGHT`
composite sets, all referencing the same 8 bloom views. The
`update_descriptor_sets` calls happen once at startup, not every frame.

This is important for the **`DescriptorSet` lifetime** — descriptor sets
reference image views, and image views are device objects. As long as the bloom
image views outlive the descriptor pool, we're fine. The bloom images are owned
by the `Renderer` and destroyed in `Renderer::drop` before the descriptor pool
is destroyed, so the ordering is correct.

## 5. Sampler selection

The existing project's samplers are mostly `REPEAT` / `LINEAR` mip with no
anisotropy (see `texture.rs`). For the postprocess samplers we need a different
choice:

- **Bright-pass and composite**: `CLAMP_TO_EDGE` (so we don't wrap the bloom
  into the screen edges), `LINEAR` filter, no mips (single-level sampling),
  `min_lod = 0`, `max_lod = 0`.
- **Blur horizontal and vertical**: same `CLAMP_TO_EDGE`, `LINEAR`, no mips.

We allocate one sampler per use case (3 total: input, blur, composite-input),
or just one shared sampler for all postprocess inputs. Sharing is fine; the
requirements are identical. So we add 1 new `vk::Sampler` to the project.

## 6. Winding invariants — what changes and what doesn't

The fullscreen triangle's vertex shader produces NDC positions `(-1,-1)`,
`(3,-1)`, `(-1,3)`. With our existing negative-height viewport
(`y = extent.height`, `height = -extent.height`), the framebuffer y of each
vertex is:

- NDC y = -1 → framebuffer y = `H` (bottom)
- NDC y = 3 → framebuffer y = `0` (top)

The triangle's index order (0,1,2) traces from bottom-left to bottom-right to
top-left. The 2D signed area in framebuffer coordinates is:

```
A = (1/2) * |x_0(y_1 - y_2) + x_1(y_2 - y_0) + x_2(y_0 - y_1)|
sign = (x_1 - x_0) * (y_2 - y_0) - (x_2 - x_0) * (y_1 - y_0)
```

For NDC, with vertices in the order (-1,-1), (3,-1), (-1,3): the sign is
**positive** (CCW in NDC). After the Y-flip viewport the sign is **negative**
(CW in framebuffer).

So with `cull_mode = BACK` and `front_face = CCW`, the triangle is **culled**
in framebuffer space. We must set `cull_mode = NONE` for the fullscreen
pipelines. The PBR and skybox pipelines are unchanged (they use cull_mode =
BACK, and that's correct for their geometry per `docs/winding_orientation.md`).

This is a critical detail. The fullscreen triangle is the first piece of
geometry in the project that has only one valid cull-mode, and it must be
`NONE`.

### 6.1 Sampling the scene color — UV direction

When sampling the scene color in the composite pass, what is the correct UV?
The scene color is a regular `vk::Image`, not a swapchain image. Its image
layout is `SHADER_READ_ONLY_OPTIMAL`. Vulkan's `ImageView` swizzles are
identity by default. UV (0,0) corresponds to the image's (0,0) texel which is
the top-left of the image **in the image's coordinate system**, not the
framebuffer's.

The scene color image has no framebuffer orientation; it is sampled as a
texture, and texture coordinates (0,0) is the first texel (top-left in the
Vulkan image layout sense). When we blit from the scene color to the composite
output, the Y axis is just the Y axis of the image.

The composite pass renders to the swapchain framebuffer, with the negative-
height viewport. The composite fragment shader receives `vUV` from the vertex
shader. If `vUV = pos * 0.5 + 0.5` (where `pos` is the clip-space position
from the vertex shader), then for the bottom-left vertex of the screen,
`vUV = (0, 0)`. For the top-right, `vUV = (1, 1)`.

But the "bottom" of the screen in framebuffer is `y = H` (high framebuffer y)
and the "top" is `y = 0` (low framebuffer y). After the Y-flip viewport, NDC
y = -1 maps to framebuffer y = H (screen bottom). So the vertex at NDC
(-1, -1) becomes framebuffer (0, H) and its vUV is (0, 0).

When we sample the scene color at vUV (0, 0), we get the texel at image-(0, 0).
The image-(0, 0) texel was written by the PBR fragment at framebuffer (0, 0),
which is the **top-left of the framebuffer**.

This is a Y mismatch! The framebuffer's bottom-left maps to vUV (0, 0), but the
image's top-left (image-(0, 0)) is the framebuffer's top-left. So sampling
sceneColor at vUV (0, 0) gives us the **top** of the scene when we wanted the
**bottom**.

The fix: **flip the vUV in the composite shader** —
`vec2 sampleUV = vec2(vUV.x, 1.0 - vUV.y);`. Or equivalently, flip it in the
vertex shader: `vUV.y = 1.0 - vUV.y;`.

When rendering into an image with the Y-flip viewport, the image texel that
gets written is at image `(x, H - y)`, not `(x, y)`. So when the PBR shader
writes to the scene color at framebuffer (0, H), it writes to image texel
(0, H), which is the **bottom** of the image. When the composite shader samples
at vUV (0, 0) and that vUV came from a vertex at framebuffer (0, H), we want
to read image (0, H) — but sampling at image (0, 0) gives the **top** of the
image. To get image row H at vUV (0, 0), flip:
`texture(sceneColor, vec2(vUV.x, 1.0 - vUV.y))`. With vUV=(0,0) we sample
image (0, 1) → (0, H) which is the bottom. ✓

**Conclusion**: yes, the composite shader must flip vUV.y when sampling the
scene color. This is the standard "render-to-texture then sample" Y-flip that
every postprocess pass has to do.

**And the bloom mip images?** They're written with the same Y-flip viewport
(they're fullscreen quads too). So when we sample them in the composite, we use
the same `1.0 - vUV.y` flip. Same rule.

What about the bright-pass and blur passes? They sample the **scene color or
the previous blur output**, written with the Y-flip viewport. So they also
flip vUV.y. Same rule.

**Conclusion**: every postprocess pass that samples a previously-rendered image
must do `vec2(vUV.x, 1.0 - vUV.y)`. We bake this into a small helper or just
remember it in every fragment shader.

We keep the Y-flip viewport and apply the UV flip in the postprocess samplers.
This is consistent with the existing `docs/winding_orientation.md` analysis and
does not require changing the existing PBR/skybox viewport or pipeline.

## 7. Module layout

New files:

```
src/vulkan/
  postprocess/
    mod.rs              // public re-exports
    bloom.rs            // BloomPyramid struct + new() + destroy() + record() helpers
    composite.rs        // CompositePass struct, postprocess UBO
    fullscreen.rs       // create_fullscreen_pipeline() — vertex + (frag, layout) factory
  renderer.rs           // modified: holds PostProcessResources, calls them in draw_frame
```

`PostProcessResources` struct (owned by `Renderer`):

```rust
struct PostProcessResources {
    settings: PostProcessSettings, // current values, modified at runtime
    ubo: Vec<GpuBuffer>,           // one per frame in flight
    ubo_mapped: Vec<*mut u8>,
    ubo_layout: vk::DescriptorSetLayout, // binding 0 = UBO
    descriptor_pool: vk::DescriptorPool, // separate from main pool, or shared with grown size
    descriptor_sets: Vec<vk::DescriptorSet>, // MAX_FRAMES_IN_FLIGHT, bind to set 2

    scene_color_images: Vec<vk::Image>,
    scene_color_memories: Vec<vk::DeviceMemory>,
    scene_color_views: Vec<vk::ImageView>,
    scene_color_framebuffers: Vec<vk::Framebuffer>,
    scene_render_pass: vk::RenderPass,

    bloom: BloomPyramid,           // 8 mip images + views
    bright_pipeline: PipelineData, // fullscreen vert + bright frag
    blur_pipeline: PipelineData,   // fullscreen vert + blur frag
    composite_pipeline: PipelineData, // fullscreen vert + composite frag
    composite_framebuffers: Vec<vk::Framebuffer>, // reuses existing depth

    input_sampler: vk::Sampler,    // CLAMP_TO_EDGE, LINEAR, no mip

    bloom_descriptor_set_layout: vk::DescriptorSetLayout, // 1 sampler
    composite_descriptor_set_layout: vk::DescriptorSetLayout, // 1 + 8 samplers
}
```

Each postprocess pipeline binds:
- **Set 0**: input sampler(s) (1 for bright/blur, 1+8 for composite).
- **Set 2**: postprocess UBO (small uniform with params).

`set 0` references per-swapchain-image views for the bright and composite
passes (the scene color changes per swapchain image), so we allocate the
bright and composite input sets one per swapchain image. The blur passes
always operate on the bloom mip images (which are stable), so the blur input
sets reference the stable mip views and are written once at bloom pyramid
creation.

Revised resource layout:

```rust
struct PostProcessResources {
    settings: PostProcessSettings,

    // Per-swapchain-image (recreated with swapchain)
    scene_color_images: Vec<vk::Image>,
    scene_color_memories: Vec<vk::DeviceMemory>,
    scene_color_views: Vec<vk::ImageView>,
    scene_color_framebuffers: Vec<vk::Framebuffer>,

    // Stable across swapchain (recreated only on extent change)
    bloom: BloomPyramid, // 8 mip images + views
    input_sampler: vk::Sampler,

    // Pipelines + layouts
    scene_render_pass: vk::RenderPass,
    bright_pipeline: PipelineData,
    blur_pipeline: PipelineData,
    composite_pipeline: PipelineData,
    postprocess_ubo_layout: vk::DescriptorSetLayout,
    bright_input_layout: vk::DescriptorSetLayout, // 1 sampler
    blur_input_layout: vk::DescriptorSetLayout,    // 1 sampler
    composite_input_layout: vk::DescriptorSetLayout, // 1 scene + 8 bloom = 9 samplers

    // Descriptor pool (separate from the main descriptor_pool in Renderer)
    descriptor_pool: vk::DescriptorPool,

    // UBO: one per frame in flight (modified every frame)
    ubo: Vec<GpuBuffer>,
    ubo_mapped: Vec<*mut u8>,
    ubo_sets: Vec<vk::DescriptorSet>, // per-frame-in-flight, set 2

    // Bright input: one per swapchain image
    bright_input_sets: Vec<vk::DescriptorSet>,

    // Blur input: one per bloom mip (8 total)
    blur_input_sets: Vec<vk::DescriptorSet>,

    // Composite input: one per swapchain image
    composite_input_sets: Vec<vk::DescriptorSet>,
}
```

The postprocess UBO set 2 is `MAX_FRAMES_IN_FLIGHT` long, written each frame
(with the current exposure, threshold, weights, etc.). The bright/composite
input sets reference per-swapchain-image views and are written once at
swapchain creation. The blur input sets reference per-mip views and are
written once at bloom pyramid creation.

This is verbose but clear. Pool size calculation:
- UBO: `MAX_FRAMES_IN_FLIGHT = 2` sets, each with 1 UBO binding → 2 UBO
  descriptors.
- Bright input: `num_swapchain_images` sets, each with 1 sampler → N samplers.
- Blur input: 8 sets, each with 1 sampler → 8 samplers.
- Composite input: `num_swapchain_images` sets, each with 9 samplers → 9N
  samplers.

Total: 2 + N + 8 + 9N = 2 + 10N + 8 = 10 + 10N samplers + 2 UBOs. For N=2
swapchain images, that's 30 samplers + 2 UBOs. Trivial.

### 7.1 UBO updates

The postprocess UBO is updated every frame (we want runtime exposure control,
bloom intensity tweakable, etc.). The pattern matches the existing
`global_uniforms`: one `HOST_VISIBLE | HOST_COHERENT` buffer per frame in
flight, persistently mapped, `memcpy` into the mapped pointer before
submitting the frame's command buffer.

## 8. Runtime settings and input

The `PostProcessSettings` struct lives in the renderer and can be tweaked at
runtime via a small debug UI or a key. For a starter framework, key bindings:

- **`[` / `]`**: decrease / increase exposure (in 0.1-stop steps, range −10
  to +10).
- **`;` / `'`** (or `,`/`.` if those are reserved): decrease / increase bloom
  threshold (range 0 to 16).
- **`B`**: toggle bloom on/off.
- **`T`**: cycle tonemapping operator (linear → reinhard → ACES → ...).

These are nice-to-haves. The first iteration doesn't need UI; the values can
be hard-coded constants in `PostProcessSettings::default()`.

## 9. Cleanup ordering

`Renderer::drop` (`renderer.rs:738-795`) must destroy everything in the right
order. New order:

1. `let _ = self.device.device_wait_idle();` (unchanged).
2. **New**: `self.postprocess.destroy(&self.device);` — destroys the new
   postprocess pipelines, layouts, scene color images/views/memory, bloom
   pyramid, samplers, descriptor pool, UBOs.
3. `self.scene.destroy(&self.device);` (unchanged).
4. `self.ibl.destroy(&self.device);` (unchanged).
5. `self.skybox_vertex_buffer.destroy(&self.device);` (unchanged).
6. `self.skybox_index_buffer.destroy(&self.device);` (unchanged).
7. Skybox pipeline/layout destruction (unchanged).
8. Global UBOs (unchanged).
9. **Main descriptor pool destruction** (unchanged) — must come after the
   postprocess descriptor pool, which is destroyed in step 2.
10. Main descriptor set layouts (unchanged).
11. Fences, semaphores (unchanged).
12. Command pool, command buffers (unchanged).
13. PBR pipeline/layout/render pass (unchanged).
14. **New**: composite render pass destruction (added at the end of
    postprocess destroy).
15. Swapchain cleanup (unchanged).

The postprocess destroy in step 2 needs to handle:
- Postprocess descriptor pool (descriptor pools are independent; you can destroy
  them in any order, but Vulkan requires that no descriptor sets allocated from
  the pool be in use. Since `device_wait_idle` is called first, all command
  buffers are idle, so all descriptor sets are not in use, so the order
  between pools doesn't matter).
- Postprocess pipelines + layouts.
- The scene render pass.
- The postprocess samplers.
- The bloom pyramid (images, memories, views).
- The scene color images + memories + views.
- The postprocess UBOs (with `unmap_memory` first).
- The framebuffers (scene color framebuffers and composite framebuffers — the
  latter are the existing ones? No, the existing `framebuffers` in
  `SwapchainData` are now used by the composite pass. They were created with
  the composite render pass. So we need to update `create_swapchain` to use
  the composite render pass instead of the original PBR pass).

So `create_swapchain` is modified:
- Render pass parameter: now the **composite render pass** (not the scene PBR
  pass), because the framebuffers are for the composite step.
- Framebuffer attachments: still `[color_view, depth_view]`, but `render_pass`
  is the composite one (which doesn't use depth, but Vulkan doesn't require a
  framebuffer to use all attachments it declares).

The existing `framebuffers` in `SwapchainData` are 2-attachment (color +
depth). If we use them for the composite pass which has only a color
attachment, is that OK? The Vulkan spec says a framebuffer can have more
attachments than the render pass uses; the unused ones are simply ignored. The
dimensions and the used attachment index must match. So yes, this works.

**But**: the scene pass **does** need depth. The scene pass's framebuffer is
`[scene_color_view, depth_view]`, separate from the composite framebuffer. We
create these in `PostProcessResources::new` (and recreate them in
`recreate_postprocess` when the swapchain is recreated).

### 9.1 Resize handling

When the swapchain is recreated (in `Renderer::recreate_swapchain`), the scene
color images and framebuffers must be recreated too. The bloom pyramid depends
only on extent, so it must be recreated whenever the extent changes (always
together with the swapchain).

I'll add a `recreate_postprocess(&mut self, ctx, width, height)` method called
from `recreate_swapchain` after the new swapchain is created. It destroys the
old scene color images/views/memory/framebuffers and the bloom pyramid, then
creates new ones. The descriptor sets that reference these images
(bright_input_sets, composite_input_sets) must be reallocated and rewritten.

**Descriptor reallocation on resize**: descriptor sets are bound to a specific
pool, and you can't reallocate them; you have to destroy the pool and recreate
it (or just keep the pool but allocate new sets, which is fine as long as the
pool has capacity). Since we pre-sized the pool for
`10 + 10 * MAX_SWAPCHAIN_IMAGES`, and we re-allocate the same number of sets
on resize, this works.

Wait, can descriptor sets be re-allocated from the same pool? Yes, as long as
the pool has space and the previous sets are not in use (they're not, because
we waited for idle). The sets are not "freed" individually; the entire pool is
destroyed at end of life. So we can allocate new sets with the same pool and
the old sets are simply replaced in our `Vec<vk::DescriptorSet>`.

Actually, you can't "free" a single descriptor set; you can only reset the
entire pool with `vkResetDescriptorPool`. To re-allocate, you must either:
- Destroy the pool and create a new one, OR
- Reset the pool (which frees all sets), then re-allocate.

We can `vkResetDescriptorPool` on resize. That's the cleanest pattern.

Or, even simpler, **never re-allocate the descriptor sets** — bind the
postprocess render passes and framebuffers directly to the image views that
are stable. The image views for the scene color change per swapchain
recreation; we'd need to either (a) update the existing sets with new image
infos, or (b) reset and reallocate.

`vkUpdateDescriptorSets` with the new `vk::DescriptorImageInfo` works on
existing descriptor sets; we don't need to reallocate. The image view handles
change, but the descriptor set handle stays the same. So we can keep the same
sets, just rewrite the image info bindings. **This is the right approach.**

So on resize:
1. Destroy old scene color images/views/memory.
2. Destroy old bloom images/views/memory.
3. Destroy old framebuffers (scene and composite).
4. Create new scene color images/views/memory at new extent.
5. Create new bloom pyramid at new extent.
6. Create new framebuffers.
7. Call `update_descriptor_sets` on the existing sets to point at the new
   views.
8. Call `set_object_name` (debug marker) for the new resources.

Step 7 is cheap (just CPU-side updates; no GPU work).

## 10. Shader inventory

New GLSL files (all compiled to `.spv` by `compile.bat`):

| File | Stage | Purpose |
|------|-------|---------|
| `postprocess/fullscreen.vert` | vertex | Generates the fullscreen triangle from `gl_VertexIndex`, outputs `vUV = pos.xy * 0.5 + 0.5`. |
| `postprocess/bright.frag` | fragment | Reads scene color, applies soft threshold, outputs to bloom mip 0. |
| `postprocess/blur.frag` | fragment | 9-tap separable Gaussian, direction uniform. |
| `postprocess/composite.frag` | fragment | Reads scene color (with vUV.y flip), sums 8 bloom mips with weights, applies exposure + tonemapping, writes sRGB output. |

All of these share the same vertex shader. The vertex shader is the simplest
possible:

```glsl
#version 450
layout(location = 0) out vec2 vUV;
void main() {
    vec2 pos = vec2((gl_VertexIndex & 1) * 4.0 - 1.0,
                    (gl_VertexIndex & 2) * 2.0 - 1.0);
    gl_Position = vec4(pos, 0.0, 1.0);
    vUV = pos * 0.5 + 0.5;
}
```

The fullscreen triangle has vertices at NDC `(-1,-1)`, `(3,-1)`, `(-1,3)`. It
covers the entire NDC square and then some; the parts outside `[-1, 1]^2` are
clipped. No vertex buffer is needed (we draw with `vkCmdDraw(3, 1, 0, 0)` and
`vertex_input_state = None`).

### 10.1 Bright pass shader sketch

```glsl
#version 450
layout(set = 0, binding = 0) uniform sampler2D uSceneColor;
layout(set = 2, binding = 0) uniform PostProcessUBO {
    float exposure;          // unused in bright pass
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;   // unused in bright pass
    float bloom_weights[8];  // unused
    uint tonemap_op;         // unused
    uint _pad[3];
} pp;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

void main() {
    // Flip Y because the scene was rendered with a Y-flip viewport.
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
    vec3 color = texture(uSceneColor, uv).rgb;
    float brightness = max(max(color.r, color.g), color.b);
    float threshold = pp.bloom_threshold;
    float knee = pp.bloom_knee * threshold;
    float soft = brightness - threshold + knee;
    soft = clamp(soft, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee + 1e-5);
    float contribution = max(soft, brightness - threshold) / max(brightness, 1e-5);
    outColor = vec4(color * contribution, 1.0);
}
```

### 10.2 Blur shader sketch

```glsl
#version 450
layout(set = 0, binding = 0) uniform sampler2D uInput;
layout(set = 2, binding = 0) uniform PostProcessUBO {
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    float bloom_weights[8];
    uint tonemap_op;
    uint _pad[3];
} pp;
layout(push_constant) uniform BlurPC {
    vec2 uTexelSize;
    int uDirection; // 0 = horizontal, 1 = vertical
} pc;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

const float W0 = 0.227027;
const float W1 = 0.194594;
const float W2 = 0.121622;
const float W3 = 0.054054;
const float W4 = 0.016216;

void main() {
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
    vec2 step = (pc.uDirection == 0)
        ? vec2(pc.uTexelSize.x, 0.0)
        : vec2(0.0, pc.uTexelSize.y);
    vec3 color = texture(uInput, uv).rgb * W0;
    color += texture(uInput, uv + step * 1.0).rgb * W1;
    color += texture(uInput, uv - step * 1.0).rgb * W1;
    color += texture(uInput, uv + step * 2.0).rgb * W2;
    color += texture(uInput, uv - step * 2.0).rgb * W2;
    color += texture(uInput, uv + step * 3.0).rgb * W3;
    color += texture(uInput, uv - step * 3.0).rgb * W3;
    color += texture(uInput, uv + step * 4.0).rgb * W4;
    color += texture(uInput, uv - step * 4.0).rgb * W4;
    outColor = vec4(color, 1.0);
}
```

The 9-tap weights sum to 1.0 (1 + 2 + 2 + 2 + 2 outer taps at weights
W1..W4, plus the center tap at W0). For a higher-quality blur, we can use a
13-tap with weights that better approximate a Gaussian, but 9 is fine for a
starter.

### 10.3 Composite shader sketch

```glsl
#version 450
layout(set = 0, binding = 0) uniform sampler2D uSceneColor;
layout(set = 0, binding = 1) uniform sampler2D uBloom0;
layout(set = 0, binding = 2) uniform sampler2D uBloom1;
layout(set = 0, binding = 3) uniform sampler2D uBloom2;
layout(set = 0, binding = 4) uniform sampler2D uBloom3;
layout(set = 0, binding = 5) uniform sampler2D uBloom4;
layout(set = 0, binding = 6) uniform sampler2D uBloom5;
layout(set = 0, binding = 7) uniform sampler2D uBloom6;
layout(set = 0, binding = 8) uniform sampler2D uBloom7;
layout(set = 2, binding = 0) uniform PostProcessUBO {
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    float bloom_weights[8];
    uint tonemap_op;
    uint _pad[3];
} pp;
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec4 outColor;

vec3 aces(vec3 c) {
    const float a = 2.51, b = 0.03, c2 = 2.43, d = 0.59, e = 0.14;
    return clamp((c * (a * c + b)) / (c * (c2 * c + d) + e), 0.0, 1.0);
}

vec3 reinhard(vec3 c) {
    return c / (c + vec3(1.0));
}

void main() {
    vec2 uv = vec2(vUV.x, 1.0 - vUV.y);
    vec3 scene = texture(uSceneColor, uv).rgb;

    vec3 bloom = vec3(0.0);
    bloom += texture(uBloom0, uv).rgb * pp.bloom_weights[0];
    bloom += texture(uBloom1, uv).rgb * pp.bloom_weights[1];
    bloom += texture(uBloom2, uv).rgb * pp.bloom_weights[2];
    bloom += texture(uBloom3, uv).rgb * pp.bloom_weights[3];
    bloom += texture(uBloom4, uv).rgb * pp.bloom_weights[4];
    bloom += texture(uBloom5, uv).rgb * pp.bloom_weights[5];
    bloom += texture(uBloom6, uv).rgb * pp.bloom_weights[6];
    bloom += texture(uBloom7, uv).rgb * pp.bloom_weights[7];
    bloom *= pp.bloom_intensity;

    vec3 color = scene + bloom;
    color *= pow(2.0, pp.exposure); // exposure in stops

    vec3 mapped;
    if (pp.tonemap_op == 1u) {
        mapped = reinhard(color);
    } else if (pp.tonemap_op == 2u) {
        mapped = aces(color);
    } else {
        mapped = clamp(color, 0.0, 1.0); // linear / none
    }

    outColor = vec4(mapped, 1.0);
}
```

The sRGB swapchain attachment performs the final linear-to-sRGB encoding on
store, so we do not apply manual gamma in the shader. This matches the
existing `pbr.frag:163` comment.

### 10.4 The postprocess UBO layout

The `PostProcessUBO` struct is shared across the three postprocess fragment
shaders, but each shader uses a subset of the fields. GLSL std140 layout puts
arrays at 16-byte alignment, so `float[8]` aligns to 16 and starts at offset
16 (after the first 4 floats = 16 bytes). The struct is 16 + 32 + 16 = 64
bytes.

```rust
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PostProcessUBO {
    pub exposure: f32,           // offset 0
    pub bloom_threshold: f32,    // offset 4
    pub bloom_knee: f32,         // offset 8
    pub bloom_intensity: f32,    // offset 12
    pub bloom_weights: [f32; 8], // offset 16 (vec4-aligned)
    pub tonemap_op: u32,         // offset 48
    pub _pad: [u32; 3],          // offset 52
}
// Total: 64 bytes
```

This struct goes in a new file `src/vulkan/postprocess/ubo.rs`, mirroring the
existing `src/vulkan/pbr_ubo.rs`.

## 11. Per-frame recording (the heart of the change)

`record_command_buffer` in `renderer.rs:797-1028` is extended. The new flow is:

```
1. Begin command buffer.
2. Render pass: scene pass (PBR + skybox), color attachment = scene_color[image_index], depth = depth[image_index].
3. (Implicit barrier: scene_color → SHADER_READ_ONLY_OPTIMAL)
4. Render pass: bloom bright pass, color attachment = bloom.images[0].
5. (Implicit barrier: bloom.images[0] → SHADER_READ_ONLY_OPTIMAL)
6. For each mip i in 0..8:
   a. Render pass: bloom blur horizontal, color attachment = bloom.temp[i].
   b. (Implicit barrier: bloom.temp[i] → SHADER_READ_ONLY_OPTIMAL)
   c. Render pass: bloom blur vertical, color attachment = bloom.images[i].
   d. (Implicit barrier: bloom.images[i] → SHADER_READ_ONLY_OPTIMAL)
7. Render pass: composite, color attachment = swapchain[image_index], depth unused.
8. End command buffer.
```

Each postprocess render pass binds:
- The appropriate pipeline (bright/blur/composite).
- `set 0` = the input image(s).
- `set 2` = the postprocess UBO (or a per-frame constant set; for the blur,
  the push constants carry the texel size and direction).

The composite render pass is the **existing** one (renamed/restructured). The
existing render pass in `pipeline.rs:9-73` is the PBR one. We need a new
render pass for the composite (no depth, sRGB output). The existing
`pipeline.render_pass` in `PipelineData` for the PBR pipeline is now the scene
render pass. The composite render pass is a new `vk::RenderPass` owned by
`PostProcessResources`.

The `SwapchainData.framebuffers` is repurposed to be the **composite
framebuffers** (one per swapchain image), because those are what the composite
pass uses. Their color attachment is the swapchain image view; the depth view
is now unused (since the composite pass has no depth), but it's still in the
framebuffer attachment list (Vulkan doesn't care if a framebuffer has unused
attachments; we just don't reference them in the render pass).

The `scene_color_framebuffers` are new, one per swapchain image, with
attachments `[scene_color_view, depth_view]`.

### 11.1 The actual command buffer recording

Here is a sketch of the new recording. The existing `record_command_buffer` is
heavily refactored; the body that draws PBR meshes is extracted into a helper,
and the postprocess passes are added.

```rust
fn record_command_buffer(
    device: &ash::Device,
    debug_marker: Option<&DebugMarker>,
    command_buffer: vk::CommandBuffer,
    frame: usize,
    image_index: u32,
    postprocess: &PostProcessResources,
    pbr: &PipelineData,
    skybox: &PipelineData,
    global_descriptor_set: vk::DescriptorSet,
    material_descriptor_sets: &[vk::DescriptorSet],
    scene: &Scene,
    skybox_vertex_buffer: vk::Buffer,
    skybox_index_buffer: vk::Buffer,
    skybox_index_count: u32,
    swapchain_framebuffers: &[vk::Framebuffer],
    depth_view: vk::ImageView, // not used by composite, but kept
    extent: vk::Extent2D,
) {
    let begin_info = vk::CommandBufferBeginInfo::default();
    unsafe { device.begin_command_buffer(command_buffer, &begin_info).unwrap(); }

    // Debug marker: "Frame X / Image Y"
    if let Some(dm) = debug_marker { dm.begin_label(...); }

    // ---- Scene pass: PBR + skybox ----
    let scene_fb = postprocess.scene_color_framebuffers[image_index as usize];
    let scene_clear_values = [
        vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 1.0] } },
        vk::ClearValue { depth_stencil: vk::ClearDepthStencilValue { depth: 1.0, stencil: 0 } },
    ];
    let scene_pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(postprocess.scene_render_pass)
        .framebuffer(scene_fb)
        .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent })
        .clear_values(&scene_clear_values);
    let viewport = /* negative height, as in current code */;
    let scissor = /* as in current code */;

    if let Some(dm) = debug_marker { dm.begin_label(command_buffer, "Scene Pass", ...); }
    unsafe { device.cmd_begin_render_pass(command_buffer, &scene_pass_begin, vk::SubpassContents::INLINE); }
    unsafe { device.cmd_set_viewport(command_buffer, 0, std::slice::from_ref(&viewport)); }
    unsafe { device.cmd_set_scissor(command_buffer, 0, std::slice::from_ref(&scissor)); }

    // Draw skybox
    // Draw PBR meshes
    // (existing code, unchanged except using `pbr.render_pass` no longer matters; we hardcode `postprocess.scene_render_pass` in the begin info)

    unsafe { device.cmd_end_render_pass(command_buffer); }
    if let Some(dm) = debug_marker { dm.end_label(command_buffer); }

    // ---- Postprocess passes ----
    postprocess.record_bright_pass(device, command_buffer, debug_marker, image_index, frame, extent);
    postprocess.record_blur_passes(device, command_buffer, debug_marker, extent);
    postprocess.record_composite(device, command_buffer, debug_marker, image_index, frame, swapchain_framebuffers[image_index as usize], extent);

    if let Some(dm) = debug_marker { dm.end_label(command_buffer); }
    unsafe { device.end_command_buffer(command_buffer).unwrap(); }
}
```

Each `record_*` helper writes a few `cmd_begin_render_pass` /
`cmd_bind_pipeline` / `cmd_bind_descriptor_sets` / `cmd_draw` /
`cmd_end_render_pass` calls.

The `set 2` UBO descriptor set is bound once at the top of each postprocess
pass (we can bind it after binding set 0, or together).

## 12. First-frame readiness and the current single-buffer approach

The existing code uses the **same** `vk::Framebuffer` for the main PBR pass
and the present (line 845-852, `record_command_buffer` writes to `framebuffer`
which is the swapchain framebuffer, and the present goes from that same image
to the surface). With the new design, the PBR pass writes to
`scene_color_framebuffers[image_index]` and the composite pass writes to
`swapchain.framebuffers[image_index]`. Both are per-image, both are recreated
on resize. The presentation path is unchanged (we present from the swapchain
image, which is the composite pass's output).

There's no need to introduce additional per-frame synchronization. The
implicit pipeline barrier at the end of each render pass handles the layout
transitions. The `image_available` and `render_finished` semaphores are
unchanged: the composite pass is the last GPU work for the frame, just like
the PBR pass was before. The submit info is unchanged.

## 13. What we explicitly do NOT do (and why)

- **No compute-shader bloom.** A compute-shader bloom with shared-memory
  workgroups is faster on a modern GPU, but it requires:
  - A separate compute pipeline, compute descriptor sets, compute push
    constants.
  - Storage image bindings (`SAMPLED | STORAGE`).
  - More complex synchronization (memory barriers between compute and fragment
    stages).
  For a 800×600 window with 1-2 meshes, the render-pass-based bloom in this
  proposal runs in microseconds. The complexity of compute-shader bloom is not
  justified for a starter framework. We can add a `BloomMode::Compute` variant
  later as an optimization.

- **No downsampling with `vkCmdBlitImage` from the scene color directly to
  bloom mip 1..7.** This is a common optimization (skip the bright pass on the
  higher mips and just downsample). The "Karoly Zsolnai-Fehér" bloom does
  this. It works well, but it requires the bright pass to be applied at each
  mip level, not just mip 0. To do that we need either a 1:1 mapping of
  bright-pass output to each mip (8 bright passes) or a clever
  downsample-with-threshold shader. The simplest correct version is what I
  described: one bright pass to mip 0, then blur each mip from itself. This is
  a 4-tap cost increase over the optimized version but the absolute cost is
  still tiny. We can optimize later.

- **No Karis average / luma-weighted bright pass.** Karis (used in UE4/UE5)
  uses a weighted average of the 4 brightest pixels in a 2×2 block to compute
  a per-pixel threshold, avoiding "firefly" artifacts from single very-bright
  pixels. The simple threshold I described can produce fireflies on small
  bright objects. Karis is the right thing to do eventually, but the simple
  version is fine to start with. We can add `KarisBrightPass` as a variant.

- **No temporal stabilization.** The bloom is a one-frame effect with no
  temporal jitter. For a static camera and static scene, this is fine. For a
  moving camera, the bloom can shimmer slightly at the edges of bright objects
  as the bright-pass threshold crosses the brightness. Mitigation: temporal
  smoothing (blend current frame's bright pass with previous frame's, decay
  factor 0.95). Deferred.

- **No FXAA / SMAA / TAA.** The composite pass can host an antialiasing pass
  after tonemapping. The current project uses no AA (relying on high sample
  density at 800×600). FXAA is the easiest to add — it's a single fragment
  shader, samples the composite output, and runs as an extra postprocess pass.
  SMAA is a multi-pass effect. TAA is the gold standard but requires history
  buffers, motion vectors, and a more complex setup. For this proposal, AA is
  out of scope, but the framework supports adding it as another postprocess
  slot.

- **No color grading LUT.** A 3D LUT (typically 32×32×32) is a common
  postprocess step, applied after tonemapping. It's a single texture sample.
  We can add it as a `set 0, binding 9` in the composite shader.

- **No split-screen / vignette / film grain.** These are easy postprocess
  effects, all single-pass, all fitting into the same framework. Add as needed.

## 14. Performance budget

At 800×600:
- Scene pass: 1 fullscreen quad (skybox, 12 triangles) + N triangles (helmet,
  ~100k triangles). Triangle-bound, GPU does ~100k vertex shader invocations
  and 480k fragment shader invocations. < 0.5 ms.
- Bright pass: 1 fullscreen quad at 800×600. < 0.05 ms.
- 16 blur passes (8 horizontal + 8 vertical) at progressively halved
  resolutions. Total work: `2 * 800 * 600 * (1 + 0.5 + 0.25 + ...) ≈ 1.6M`
  fragment invocations. < 0.1 ms.
- Composite: 1 fullscreen quad. < 0.05 ms.

Total postprocess cost: ~0.2 ms. Negligible. At 1080p, this scales to ~0.8 ms.
Still negligible.

The scene pass's main cost is the PBR fragment shader (5 texture samples + IBL
math), which is what it was before — we're not changing the PBR shader beyond
removing the tonemapping call.

## 15. Step-by-step implementation order

To keep each step testable, I'll order the work so that the project compiles
and runs after every step.

1. **Add the fullscreen vertex shader** (`postprocess/fullscreen.vert`) and
   compile it. No integration yet.
2. **Add the scene color image infrastructure**: the images, views, memory,
   and framebuffers, created in a new `PostProcessResources::new_extent` and
   a matching `destroy` method. No rendering change yet.
3. **Create a new scene render pass** with `R16G16B16A16_SFLOAT` color
   attachment and the existing depth attachment,
   `final_layout = SHADER_READ_ONLY_OPTIMAL`.
4. **Switch the PBR and skybox pipelines to use the new scene render pass**
   by passing it into `create_pbr_pipeline` and `create_skybox_pipeline`.
   Update `record_command_buffer` to render into the scene color framebuffer.
   Update `create_swapchain` to keep the existing `framebuffers` (which are
   now the composite framebuffers). **At this point**, the PBR output goes to
   the scene color, and nothing samples it. The screen goes black (or whatever
   the clear value is).
5. **Add a dummy composite pass** that just blits the scene color to the
   swapchain. This can be a single fullscreen pass that samples the scene
   color and writes it to the swapchain, **without** tonemapping or bloom.
   **At this point**, the PBR output appears on screen again (linear, no
   tonemapping, the helmet might look "too dark" or "blown out" depending on
   exposure). The image may be vertically mirrored — verify and add the
   vUV.y flip.
6. **Verify Y-flip**: confirm that the helmet appears right-side-up. Add the
   `vec2(vUV.x, 1.0 - vUV.y)` UV flip in the composite shader.
7. **Add exposure control** in the composite shader and the postprocess UBO.
   Add a key binding to tweak exposure. Confirm exposure works.
8. **Add tonemapping** in the composite shader. Move the ACES function from
   `pbr.frag` to `composite.frag`. Confirm the scene looks correct (similar
   to before but with the ability to tweak).
9. **Add the bloom pyramid images** (8 mip images + views) and a sampler. No
   rendering yet.
10. **Add the bright pass** pipeline and render pass. The bright pass renders
    to bloom mip 0. No blur yet — confirm the bright pass output is correct
    (only bright pixels are non-zero).
11. **Add the blur pipeline** and ping-pong temp image. Render horizontal +
    vertical blurs for each mip. Confirm the bloom looks soft and wide.
12. **Add the composite bloom sampling** to the composite shader. Confirm the
    bloom appears on the helmet.
13. **Add bloom intensity, threshold, knee controls** to the postprocess UBO
    and key bindings.
14. **Add the recreate-on-resize** logic in `Renderer::recreate_postprocess`.
    Verify resize works.
15. **Verify validation layers** are clean. Run `cargo run` and
    `cargo run --release -- --validation`.
16. **Document the changes** in this file.

## 16. Risk register

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| vUV.y flip needed but missed | medium | image vertically mirrored | Test step 5/6 explicitly; add a debug mode to disable the flip |
| Tonemapping double-applied | low | wrong colors | After moving ACES to composite, do a `grep` to ensure no other shader applies tonemapping |
| Render-pass layout transitions mismatch | medium | validation error or corrupt textures | Each postprocess render pass must declare `initial_layout = SHADER_READ_ONLY_OPTIMAL` for the input attachment |
| Postprocess UBO std140 alignment wrong | medium | wrong values in shader | Use a Rust struct with `#[repr(C)]` matching the GLSL std140 layout; verify with `cargo run -- --validation` |
| Cleanup order: postprocess descriptor pool destroyed before sets are idle | very low | validation error | `device_wait_idle` is called at the top of `Renderer::drop` |
| Resize: descriptor sets reference destroyed image views | high | validation error on next frame | Update the descriptor sets with new image views after resize, before the next frame |
| Bloom too strong/weak | low | visual | Default bloom_intensity = 0.04, threshold = 1.0, knee = 0.5 (mild, tasteful defaults) |
| Performance regression from extra render passes | low | framerate hit | Total cost < 0.5 ms; well within budget at 800×600 |

## 17. Defaults that ship with the framework

```rust
impl Default for PostProcessSettings {
    fn default() -> Self {
        Self {
            exposure: 0.0,        // stops
            bloom_enabled: true,
            bloom_threshold: 1.0, // linear
            bloom_knee: 0.5,      // linear, multiplied by threshold internally
            bloom_intensity: 0.04,
            bloom_weights: [0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05, 0.025], // approximate Gaussian
            tonemap_op: TonemapOp::Aces,
        }
    }
}
```

These are sensible starting values. The user can tweak at runtime. The default
bloom is subtle, the default tonemapper is ACES (same as before), the default
exposure is 0 (no change). The user should not see a dramatic visual change
from the framework's introduction, just a small amount of glow on bright pixels
and the ability to tweak.

## 18. Final file map

New files (≈ 7):

```
shaders/postprocess/
  fullscreen.vert
  fullscreen.vert.spv
  bright.frag
  bright.frag.spv
  blur.frag
  blur.frag.spv
  composite.frag
  composite.frag.spv

src/vulkan/postprocess/
  mod.rs           (~30 lines, re-exports)
  ubo.rs           (~40 lines, PostProcessUBO struct)
  pyramid.rs       (~120 lines, BloomPyramid struct)
  bright.rs        (~80 lines, bright-pass pipeline + render pass)
  blur.rs          (~100 lines, blur pipeline + render pass + push constants)
  composite.rs     (~80 lines, composite pipeline + render pass)
  fullscreen.rs    (~60 lines, fullscreen vertex module + pipeline factory)
  resources.rs     (~250 lines, PostProcessResources struct, the big one)
```

Modified files (4):
- `shaders/pbr.frag`: remove `acesToneMapping` call (and the helper if we want
  to keep it private to pbr.frag, or move it to a shared header). Output
  linear HDR.
- `src/vulkan/renderer.rs`: extend `Renderer` with
  `postprocess: PostProcessResources`, modify `draw_frame` and
  `record_command_buffer` to call postprocess passes, modify
  `recreate_swapchain` to call `recreate_postprocess`.
- `src/vulkan/pipeline.rs`: change `create_render_pass` signature to take a
  `vk::Format` parameter that's the **scene color format**
  (`R16G16B16A16_SFLOAT`). The composite render pass is a new function
  `create_composite_render_pass(device, swapchain_format)`.
- `src/vulkan/swapchain.rs`: `create_swapchain` no longer creates the depth
  image (it does, but the framebuffers it creates use the **composite** render
  pass, not the scene one). The scene color framebuffers are created by
  `PostProcessResources::new_extent`.
- `shaders/compile.bat`: add the postprocess shaders to the compile list.

Estimated total LoC added: ~1000 lines of Rust + ~200 lines of GLSL. Most of
the Rust is in `postprocess/resources.rs` and the bloom pyramid management.

---

This framework satisfies the stated requirements (Bloom + Tonemapping), is
correct with respect to the existing winding contract (Y-flip viewport
preserved everywhere; fullscreen passes use `cull_mode = NONE`; UV flip in the
postprocess samplers to compensate for the Y-flip viewport), respects the
cleanup-order invariant, and provides a clean extension path for future
postprocess effects (FXAA, vignette, color grading, etc.) without further
changes to the core renderer.

---

## Post-Implementation Architecture Refinements

During implementation, several design decisions evolved beyond the original plan.
This section documents the final architecture and the rationale for each change.

### Descriptor Set Numbering (set 0 + set 1, not set 0 + set 2)

The original plan proposed set 0 for input samplers and **set 2** for the
postprocess UBO, to avoid conflicting with the PBR pipeline's set 0 (global)
and set 1 (material). However, postprocess pipelines are completely independent
of the PBR pipeline — they have their own `vk::PipelineLayout` and their own
descriptor set layouts. They don't share any descriptor with the PBR pipeline.

The actual implementation uses **set 0** for input samplers and **set 1** for
the postprocess UBO. Both sets are bound at slot 0 via a single
`cmd_bind_descriptor_sets` call with `first_set = 0` and two descriptor sets in
the array.

| Pipeline | Set 0 | Set 1 |
|---|---|---|
| PBR | Global UBO + IBL + material buffer | Material textures (5) |
| Skybox | Global UBO + IBL (subset) | — |
| Bright pass | Scene color sampler (1) | Postprocess UBO |
| Blur pass | Input sampler (1) | Postprocess UBO + push const |
| Composite | Scene color + 8 bloom mips (9) | Postprocess UBO |

### Bloom Pyramid — Single Image with Mip Levels

The original plan called for 16 separate `vk::Image` objects (8 mips + 8 temps).
The implementation uses **2 images** (one for mips, one for temps), each with
`BLOOM_MIP_COUNT` mip levels. Each level has its own `vk::ImageView` with
`base_mip_level = i`, `level_count = 1`.

Benefits:
- Allocation count reduced from 16 to 2 (less memory fragmentation).
- Layout transition barriers reduced from 16 to 2 (one barrier per image with
  `level_count = BLOOM_MIP_COUNT`).
- Views remain independently indexable (`mip_views[i]`, `temp_views[i]`), so
  the rest of the code (framebuffers, descriptor sets, render passes) is
  unchanged.

```rust
pub struct BloomPyramid {
    pub mip_views: Vec<vk::ImageView>,
    pub temp_views: Vec<vk::ImageView>,
    mip_image: vk::Image,       // single image, BLOOM_MIP_COUNT mip levels
    mip_memory: vk::DeviceMemory,
    temp_image: vk::Image,       // single image, BLOOM_MIP_COUNT mip levels
    temp_memory: vk::DeviceMemory,
    pub sampler: vk::Sampler,
}
```

The `mip_image()` and `temp_image()` accessors are exposed for barrier creation.
Consumer code accesses views via index (`mip_views[i]`, `temp_views[i]`), not
via wrapper structs.

### PostProcessSettings / PostProcessUBO Unification

The original plan defined two separate structs (`PostProcessSettings` for CPU,
`PostProcessUBO` for GPU) with duplicated fields. The implementation makes
`PostProcessUBO` the single canonical representation:

```rust
pub struct PostProcessSettings {
    pub ubo: PostProcessUBO,       // canonical GPU layout
    pub bloom_enabled: bool,       // CPU-side flag, zeroes intensity in UBO
}
```

`update_ubo()` copies `settings.ubo` to mapped GPU memory, setting
`bloom_intensity = 0.0` when `bloom_enabled` is false. Adding a new parameter
now requires changes in only one struct (`PostProcessUBO`), rather than three
places.

### Viewport/Scissor — Must Be Set in Every Render Pass

The fullscreen postprocess pipelines declare `VIEWPORT` and `SCISSOR` as
dynamic state. Every render pass that uses these pipelines **must** call
`cmd_set_viewport` and `cmd_set_scissor`, even if the values match the previous
render pass. Vulkan does not carry dynamic state across render pass boundaries.

The implementation uses a shared helper (`set_viewport_and_bind_pipeline` in
`postprocess/pass_trait.rs`) that sets the Y-flip viewport, scissor, and binds
the pipeline (draw is issued separately by the caller after descriptor-set binding).
This eliminates duplicate boilerplate and makes the viewport requirement
impossible to miss for new passes.

### PostProcessPass Trait (Framework Extensibility)

A `PostProcessPass` trait is defined in `postprocess/pass_trait.rs` to make
adding new effects straightforward. The trait encapsulates:

- `name()` — debug label.
- `render_pass()` — the render pass this pass writes into.
- `pipeline()` / `pipeline_layout()` — the fullscreen pipeline.
- `record()` — standard begin/end render pass with viewport/scissor/pipeline/draw.

To add a new pass (e.g., vignette, FXAA, color grading):

1. Write the fragment shader with `set 0 = input sampler(s)`, `set 1 = UBO`.
2. Create a pass struct holding `pipeline` + `pipeline_layout` (using
   `create_fullscreen_pipeline`).
3. Implement `PostProcessPass` for the struct.
4. Allocate a framebuffer and descriptor set for the new pass.
5. Insert `my_pass.record(device, cmd, fb, extent, sets)` in the postprocess
   chain in `record_command_buffer`.

The `set_viewport_and_bind_pipeline` helper is also available for passes that need
custom begin-info handling (push constants, multiple samples per pass like blur).

### Blur UBO Per-Frame Consistency

The blur passes (horizontal + vertical for each mip level) bind the UBO
descriptor set at the **current frame index** (`postprocess.ubo_sets[frame]`),
not hardcoded to `[0]`. This ensures that animated bloom weights (if added in
the future) take effect in the blur passes, matching the bright and composite
passes.

### Debug Markers

Every postprocess object receives a RenderDoc name via `PostProcessResources::name_debug_objects`:

| Resource | Pattern |
|---|---|
| Render passes | `"Scene HDR Render Pass"`, `"Postprocess Color Render Pass"`, `"Composite sRGB Render Pass"` |
| Scene color | `"Scene Color Image {i}"` / `"View {i}"` / `"Memory {i}"` / `"Framebuffer {i}"` |
| Bloom images | `"Bloom Mip Image"`, `"Bloom Temp Image"`, `"Bloom Mip View {i}"`, `"Bloom Temp View {i}"`, `"Bloom Sampler"` |
| Bloom framebuffers | `"Bright Pass Framebuffer"`, `"Blur Temp Framebuffer {i}"`, `"Blur Mip Framebuffer {i}"` |
| Pipelines | `"Bright Pipeline"`, `"Blur Pipeline"`, `"Composite Pipeline"` (+ layouts) |
| Descriptors | `"Postprocess UBO Desc Layout"`, `"Postprocess Single-Input Desc Layout"`, `"Postprocess Composite-Input Desc Layout"`, `"Postprocess Descriptor Pool"` |
| UBOs | `"Postprocess UBO Buffer Frame {i}"` / `"Memory Frame {i}"` |
| Descriptor sets | `"Postprocess UBO Set Frame {i}"`, `"Bright Input Set Image {i}"`, `"Blur Input Set Level {i} {H/V}"`, `"Composite Input Set Image {i}"` |

In-frame RenderDoc regions:
- Frame → **Scene Pass** (skybox + per-mesh PBR draws) → **Bright Pass** → **Blur Pyramid** (16 per-mip `insert_label` entries) → **Composite Pass**

The blur pyramid has per-mip labels (`"Blur Mip {i} Horizontal"` / `"Blur Mip {i} Vertical"`) using `insert_label`, which lets RenderDoc show individual mip passes without nested regions.
