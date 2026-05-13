# Plan: Textured Cube + Uniform Buffer Refactor

## 1. Goals

1. Replace the colored cube with a textured cube (single 2D texture sampled in the fragment shader). The floor keeps sampling the same texture (tiled), so one pipeline serves both.
2. Move the MVP matrix from a push constant to a uniform buffer (one UBO per in-flight frame, bound via descriptor set).

Non-goals: anisotropy, per-object model matrices, multiple materials.

## 2. Design

### 2.1 Vertex layout (cube and floor share it)

Replace `color: [f32;3]` with `uv: [f32;2]`. Stride 24 -> 20 B.

```rust
pub struct Vertex { pub pos: [f32;3], pub uv: [f32;2] }
```

Attribute 1: `R32G32_SFLOAT`, offset 12.

Cube UVs: each face is a standard 0..1 quad, matching the p0..p3 order in `src/mesh.rs:34-60`. Map p0,p1,p2,p3 -> (0,1),(0,0),(1,0),(1,1) so the image appears upright from outside each face.

Floor UVs: tile. With `half=20.0`, use UVs in 0..10 so the texture repeats 10x. Sampler uses `REPEAT`.

### 2.2 Texture

- Format: `R8G8B8A8_SRGB` (matches the sRGB swapchain color).
- Source: 256x256 procedural checkerboard generated in code by default; `Texture::from_png` also provided (adds `image` crate). Keeps the repo runnable without an external asset.
- Upload: mirror the staging pattern in `src/vulkan/buffer.rs:113-152`. Staging buffer -> `vk::Image` via `cmd_copy_buffer_to_image`, with two `cmd_pipeline_barrier` calls: `UNDEFINED -> TRANSFER_DST_OPTIMAL` then `TRANSFER_DST_OPTIMAL -> SHADER_READ_ONLY_OPTIMAL`.
- Full mip chain generated at runtime via GPU blit (see §2.2.1). Linear min/mag/mipmap. `REPEAT` addressing. No anisotropy.

#### 2.2.1 Mipmap Generation

Mipmaps are generated at texture upload time inside the same one-time command buffer that copies the base-level pixels from the staging buffer. No CPU-side downsampling or offline mip baking is required.

**Compute `mip_levels`:** `floor(log2(max(width, height))) + 1`. For a 256×256 texture this yields 9 levels (256→128→64→32→16→8→4→2→1).

**Image usage flags:** `TRANSFER_DST | TRANSFER_SRC | SAMPLED`. `TRANSFER_SRC` is required because each mip level (except the last) is read as a blit source after being written.

**Blit format support check:** Before creating the image, the code asserts that the chosen image format supports both `BLIT_SRC` and `BLIT_DST` in optimal tiling (via `get_physical_device_format_properties`). The textured-cube path uses `R8G8B8A8_SRGB`; later glTF data textures use linear formats such as `R8G8B8A8_UNORM`. If the device does not support blitting for the chosen format, the application panics with a clear message.

**Command buffer sequence:**

1. `cmd_pipeline_barrier`: all mip levels `UNDEFINED → TRANSFER_DST_OPTIMAL`.
2. `cmd_copy_buffer_to_image`: staging buffer → mip level 0.
3. For each level `i` in `1..mip_levels`:
   - `cmd_pipeline_barrier`: level `i-1` `TRANSFER_DST_OPTIMAL → TRANSFER_SRC_OPTIMAL` (`src_access=TRANSFER_WRITE`, `dst_access=TRANSFER_READ`).
   - `cmd_blit_image`: level `i-1` (source, `TRANSFER_SRC_OPTIMAL`) → level `i` (dest, `TRANSFER_DST_OPTIMAL`), `vk::Filter::LINEAR`, source/dest regions cover the full extent of each level (each dimension halved, minimum 1).
4. Final `cmd_pipeline_barrier`: two sub-ranges transitioned to `SHADER_READ_ONLY_OPTIMAL`:
   - Levels `0..mip_levels-1` from `TRANSFER_SRC_OPTIMAL` (`src_access=TRANSFER_READ`, `dst_access=SHADER_READ`).
   - Level `mip_levels-1` from `TRANSFER_DST_OPTIMAL` (`src_access=TRANSFER_WRITE`, `dst_access=SHADER_READ`).

If `mip_levels == 1` (e.g., a 1×1 texture), the blit loop is skipped and only the final barrier transitions level 0 from `TRANSFER_DST_OPTIMAL` to `SHADER_READ_ONLY_OPTIMAL`.

**Image view:** `subresource_range.level_count` is set to the computed `mip_levels` (not `1`).

**Sampler:** `max_lod` is set to `(mip_levels - 1) as f32` (not `0.0`), so the GPU samples lower-resolution levels for distant fragments. `mipmap_mode` remains `LINEAR`, `min_lod` remains `0.0`, `mip_lod_bias` remains `0.0`.

### 2.3 Uniform buffer (per-frame)

- One `HOST_VISIBLE | HOST_COHERENT` buffer per frame (`MAX_FRAMES_IN_FLIGHT = 2`), 64 B each (one mat4).
- Persistently mapped; `memcpy` the MVP before submit. Safe because the frame's `in_flight` fence is already waited on before recording.
- Cube and floor draw with the same UBO (same MVP), so one descriptor set per frame, bound once after `cmd_bind_pipeline`.

### 2.4 Descriptors

One descriptor set layout, two bindings:

```
set 0, binding 0 : UNIFORM_BUFFER         , stage = VERTEX
set 0, binding 1 : COMBINED_IMAGE_SAMPLER , stage = FRAGMENT
```

Descriptor pool sized for `MAX_FRAMES_IN_FLIGHT` of each type. Two sets, one per frame. UBO binding differs per frame; the image binding is the same texture each frame.

### 2.5 Shaders (`shaders/scene.vert`, `shaders/scene.frag`)

```glsl
// scene.vert
#version 450
layout(set=0, binding=0) uniform UBO { mat4 mvp; } ubo;
layout(location=0) in vec3 inPos;
layout(location=1) in vec2 inUV;
layout(location=0) out vec2 vUV;
void main() { gl_Position = ubo.mvp * vec4(inPos, 1.0); vUV = inUV; }
```

```glsl
// scene.frag
#version 450
layout(set=0, binding=1) uniform sampler2D uTex;
layout(location=0) in vec2 vUV;
layout(location=0) out vec4 outColor;
void main() { outColor = texture(uTex, vUV); }
```

Rerun `shaders/compile.bat` after editing.

## 3. Files

| File | Change |
|---|---|
| `Cargo.toml` | Add `image = { version = "0.25", default-features = false, features = ["png"] }`. |
| `src/mesh.rs` | Replace `color` with `uv`; fix `attribute_descriptions` (R32G32_SFLOAT, offset 12); update `face()` to take UVs; drop `color` param on `floor()` and emit UVs 0..tile. |
| `src/vulkan/texture.rs` (new) | `struct Texture { image, memory, view, sampler }`. `Texture::checkerboard(ctx, command_pool)`, `Texture::from_png(ctx, command_pool, path)`, `destroy(&self, device)`. Runtime mipmap generation via GPU blit chain. |
| `src/vulkan/descriptors.rs` (new) | `create_descriptor_set_layout`, `create_descriptor_pool`, `create_descriptor_sets`. |
| `src/vulkan/pipeline.rs` | `create_pipeline` takes `descriptor_set_layout`; replace `push_constant_ranges` with `set_layouts`. Drop the 64-B push-constant range (lines 168-174). |
| `src/vulkan/renderer.rs` | Remove both `cmd_push_constants` calls (lines 409-415 cube, 426-432 floor). Add fields: `descriptor_set_layout`, `descriptor_pool`, `descriptor_sets`, `uniform_buffers: Vec<GpuBuffer>`, `uniform_mapped: Vec<*mut u8>`, `texture: Texture`. Before submit, memcpy `view_proj.to_cols_array()` into `uniform_mapped[frame]`. In `record_command_buffer`, after `cmd_bind_pipeline`, call `cmd_bind_descriptor_sets` once with `descriptor_sets[frame]`, then draw cube and floor. Extend `Drop` to destroy texture, UBOs, pool, layout before pipeline. |
| `src/vulkan/mod.rs` | `pub mod texture; pub mod descriptors;` |
| `shaders/scene.vert`, `shaders/scene.frag` | Rewrite per 2.5. |

## 4. Implementation Order (each step compiles & runs)

1. **Vertex UV refactor** — keep push constants for MVP temporarily. Frag shader outputs `vec4(vUV, 0.0, 1.0)` to verify UVs. Confirms mesh + vertex input plumbing.
2. **UBO + descriptor sets** — add `descriptors.rs`, UBO fields, remove push constants, update pipeline layout, bind descriptor set. Frag shader unchanged (still UV gradient). Isolates the UBO path.
3. **Texture** — add `texture.rs` and binding 1. Swap frag to `texture(uTex, vUV)`. Default to procedural checkerboard.
4. **Validation pass** — debug build must have zero validation errors on startup, resize, shutdown (invariant from `CODEBUDDY.md`).

## 5. Cleanup Order (`Renderer::Drop`)

Insert new destroys before the existing pipeline/layout/render_pass block (`src/vulkan/renderer.rs:307-335`):

```
device_wait_idle
  -> texture.destroy(device)             // sampler, view, image, memory
  -> uniform_buffers[i].destroy()        // memory free implicitly unmaps
  -> destroy_descriptor_pool             // frees sets implicitly
  -> destroy_descriptor_set_layout
  -> [existing] VBs, IBs, sync, cmd pool, pipeline, layout, render_pass, swapchain
```

`ManuallyDrop` ordering in `src/app.rs` stays: renderer before ctx.

## 6. Reference Points

- Push-constant sites to remove: `src/vulkan/pipeline.rs:168-174`, `src/vulkan/renderer.rs:409-415` (cube), `src/vulkan/renderer.rs:426-432` (floor).
- One-time-submit pattern to mirror for image upload: `src/vulkan/buffer.rs:113-152`.
- Image create/alloc/bind/view pattern to mirror (depth image): `src/vulkan/swapchain.rs` depth helpers.
- `MAX_FRAMES_IN_FLIGHT = 2` (`src/vulkan/renderer.rs:12`) drives UBO / descriptor-set counts.

## 7. Decisions

1. **Texture asset**: `assets/texture.png` is loaded via `Texture::from_png` at startup (adds the `image` crate). A placeholder PNG is generated by `assets/gen_texture.py`, runnable with `uv run assets/gen_texture.py`, which writes a 256x256 checkerboard with a UV-origin marker. The user can overwrite `assets/texture.png` with any PNG.
2. **Floor appearance**: floor samples the same texture as the cube, tiled 10x. Single pipeline, single descriptor layout.
