# Implementation Plan: HDR Environment Map, Split-Sum IBL, and Skybox

## Overview

This plan adds a complete IBL pipeline to the Vulkan PBR renderer:
1. Load the Ennis HDR equirectangular environment map
2. Convert it to a cubemap at runtime via GPU shaders
3. Generate diffuse irradiance and GGX prefiltered specular cubemaps
4. Generate a BRDF integration LUT
5. Render the environment as a skybox background
6. Update the PBR shader to use split-sum IBL

The code review issues from code_review_5_26.md are already fixed. This plan focuses entirely on the new IBL features.

---

## Implementation Steps (Ordered)

### Step 1: Add hdr feature to image crate in Cargo.toml

**File**: `D:\GitProjects\LearnVulkan\Cargo.toml`

Add `"hdr"` to the image crate features:
```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "hdr"] }
```

---

### Step 2: Create a Cubemap Rust struct

**New file**: `D:\GitProjects\LearnVulkan\src\vulkan\cubemap.rs`

Struct holding a Vulkan cubemap image (6 face array), memory, CUBE image view, and sampler.

Key Vulkan details:
- `ImageCreateInfo.flags` must include `vk::ImageCreateFlags::CUBE_COMPATIBLE`
- `image_type`: `TYPE_2D`, `array_layers`: 6
- View type: `TYPE_CUBE`, `layer_count`: 6
- Sampler: `CLAMP_TO_EDGE` for all axes, `LINEAR` filtering, `mipmap_mode: LINEAR`

Methods:
- `create_empty(ctx, size, mip_levels, format, usage) -> Self`
- `destroy(&self, device)`

**Register**: Add `pub mod cubemap;` to `src/vulkan/mod.rs`

---

### Step 3: Create a Texture2D struct for float textures

**New file**: `D:\GitProjects\LearnVulkan\src\vulkan\texture_2d.rs`

A 2D texture struct for float-format textures (HDR equirectangular, BRDF LUT).

Methods:
- `from_f32_data(ctx, command_pool, data, width, height, num_components, format) -> Self`
- `create_empty(ctx, width, height, format, usage) -> Self`
- `destroy(&self, device)`

**Register**: Add `pub mod texture_2d;` to `src/vulkan/mod.rs`

---

### Step 4: Add HDR equirectangular loading

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\environment_map.rs`

Replace `create_synthetic_environment_map` with `load_hdr_equirectangular`:
- Use `image::open()` then `to_rgba32f()` to get f32 pixel data
- Upload as `R32G32B32A32_SFLOAT` via `Texture2D::from_f32_data()`

---

### Step 5: Create IBL shaders

**5a. cubemap_convert.vert** - Shared vertex shader for cubemap face rendering
- push_constant: `{ mat4 mvp; float roughness; }`
- Outputs `vDirection = inPos`

**5b. equirect_to_cube.frag** - Equirectangular to cubemap
- `set=0 binding=0`: `sampler2D uEquirectMap`
- `directionToEquirectUV`: `phi = atan(dir.z, dir.x)`, `theta = asin(dir.y)`

**5c. irradiance_convolution.frag** - Diffuse irradiance
- `set=0 binding=0`: `samplerCube uEnvironmentMap`
- Cosine-weighted hemisphere sampling with `sampleDelta=0.025`

**5d. prefilter_env.frag** - GGX prefiltered specular
- `set=0 binding=0`: `samplerCube uEnvironmentMap`
- push_constant roughness
- Hammersley + GGX importance sampling, 1024 samples

**5e. brdf_lut.vert** - Fullscreen triangle (no vertex buffer)
- Uses `gl_VertexIndex` for 3 vertices

**5f. brdf_lut.frag** - BRDF integration
- Output: `vec2(scale, bias)` at each `(NdotV, roughness)` texel
- 1024 Hammersley samples with GGX importance sampling

**5g. skybox.vert** - Skybox vertex shader
- Uses GlobalUBO view/proj, strips translation from view matrix
- `gl_Position = clipPos.xyww` (forces depth to far plane)

**5h. skybox.frag** - Skybox fragment shader
- `set=0 binding=5`: `samplerCube uEnvironmentCubemap`
- ACES tone mapping for visual consistency

---

### Step 6: Create IBL generation module

**New file**: `D:\GitProjects\LearnVulkan\src\vulkan\ibl.rs`

`IblResources` struct:
- `env_cubemap`: Cubemap (512x512, 1 mip, R32G32B32A32_SFLOAT)
- `irradiance_map`: Cubemap (32x32, 1 mip, R32G32B32A32_SFLOAT)
- `prefilter_map`: Cubemap (128x128, 5 mips, R32G32B32A32_SFLOAT)
- `brdf_lut`: Texture2D (512x512, R32G32_SFLOAT)

`generate()` flow:
1. Load HDR equirectangular
2. Convert to cubemap: 6 face loop, render unit cube sampling equirect
3. Generate irradiance: 6 face loop, render unit cube sampling env cubemap
4. Generate prefilter: 5 mip levels x 6 faces, render with roughness = mip/(max_mips-1)
5. Generate BRDF LUT: single fullscreen triangle draw
6. Clean up temporary equirectangular texture

**Cubemap face rendering pattern** (for each face):
1. Create temporary 2D image view of cubemap layer (and mip level)
2. Create framebuffer with that view
3. Begin color-only render pass (no depth)
4. Bind pipeline, descriptor set, push constants
5. Draw unit cube (8 verts, 36 indices, positions only)
6. End render pass, destroy temp framebuffer and view

**LH face view matrices** (`look_to_lh` with 90-degree perspective):

| Face | Direction | Up |
|------|-----------|-----|
| +X | (1,0,0) | (0,1,0) |
| -X | (-1,0,0) | (0,1,0) |
| +Y | (0,1,0) | (0,0,-1) |
| -Y | (0,-1,0) | (0,0,1) |
| +Z | (0,0,1) | (0,1,0) |
| -Z | (0,0,-1) | (0,1,0) |

**Offscreen render pass**: color-only, CLEAR/STORE, UNDEFINED -> SHADER_READ_ONLY_OPTIMAL

**Pipeline layout for IBL pipelines**:
- One descriptor set layout: binding 0 = COMBINED_IMAGE_SAMPLER
- Push constants: mat4 mvp (64B) + float roughness (4B) = 68B

**Register**: Add `pub mod ibl;` to `src/vulkan/mod.rs`

---

### Step 7: Update descriptor layout

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\descriptors.rs`

Global descriptor set layout changes from 3 to 6 bindings:

| Binding | Type | Name | Stages |
|---------|------|------|--------|
| 0 | UNIFORM_BUFFER | GlobalUBO | vert+frag |
| 1 | UNIFORM_BUFFER | MaterialBuffer | frag |
| 2 | COMBINED_IMAGE_SAMPLER | uIrradianceMap (samplerCube) | frag |
| 3 | COMBINED_IMAGE_SAMPLER | uPrefilterMap (samplerCube) | frag |
| 4 | COMBINED_IMAGE_SAMPLER | uBRDFLUT (sampler2D) | frag |
| 5 | COMBINED_IMAGE_SAMPLER | uEnvironmentCubemap (samplerCube) | frag |

Pool sizes: COMBINED_IMAGE_SAMPLER count = `num_materials * 5 + frames * 4`

---

### Step 8: Update pipeline creation

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\pipeline.rs`

Add `create_skybox_pipeline(device, render_pass, global_layout)`:
- Vertex input: vec3 position only (stride 12B, R32G32B32_SFLOAT at location 0)
- Shaders: skybox.vert.spv + skybox.frag.spv
- Depth: test=true, write=false, compare=LESS_OR_EQUAL
- Rasterizer: cull_mode=FRONT, front_face=COUNTER_CLOCKWISE
- Descriptor layouts: [global_layout] only
- No push constants

---

### Step 9: Update the Renderer

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\renderer.rs`

**Struct changes**:
- Remove `env_map: Texture`
- Add `ibl: IblResources`
- Add `skybox_pipeline: PipelineData`
- Add `skybox_vertex_buffer: GpuBuffer`
- Add `skybox_index_buffer: GpuBuffer`
- Add `skybox_index_count: u32`

**Initialization**:
1. Generate IBL: `IblResources::generate(ctx, command_pool, "assets/environment_map/ennis/ennis.hdr")` (project-relative — the KTX2 loader path that ultimately won out is `assets/environment_map/ennis/`; this HDR-equirect approach was not adopted, see `correct_pbr_plan.md`)
2. Create skybox pipeline
3. Create skybox cube vertex/index buffers (positions only)
4. Update descriptor writes for 6 bindings

**record_command_buffer changes**:
- Draw skybox FIRST (before PBR geometry)
- Then draw PBR scene (existing code unchanged)

**Drop changes**: Destroy ibl, skybox_pipeline, skybox vertex/index buffers

**Debug naming**: Add names for IBL resources and skybox objects

---

### Step 10: Update PBR fragment shader

**File**: `D:\GitProjects\LearnVulkan\shaders\pbr.frag`

Remove: `sampler2D uEnvironmentMap`, `sampleSphericalMap()`, `MAX_REFLECTION_LOD`, old IBL code

Add:
```glsl
layout(set = 0, binding = 2) uniform samplerCube uIrradianceMap;
layout(set = 0, binding = 3) uniform samplerCube uPrefilterMap;
layout(set = 0, binding = 4) uniform sampler2D uBRDFLUT;
```

New IBL section:
```glsl
vec3 F_ambient = fresnelSchlickRoughness(NdotV, F0, roughness);
vec3 kD_ambient = (1.0 - F_ambient) * (1.0 - metallic);

vec3 irradiance = texture(uIrradianceMap, N).rgb;
vec3 diffuse_ibl = irradiance * kD_ambient * baseColor;

vec3 R = reflect(-V, N);
const float MAX_PREFILTER_LOD = 4.0;
vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * MAX_PREFILTER_LOD).rgb;
vec2 brdf = texture(uBRDFLUT, vec2(NdotV, roughness)).rg;
vec3 specular_ibl = prefilteredColor * (F_ambient * brdf.x + brdf.y);

vec3 ambient = (diffuse_ibl + specular_ibl) * occlusion;
```

---

### Step 11: Update compile.bat

**File**: `D:\GitProjects\LearnVulkan\shaders\compile.bat`

Add compile commands for all 8 new shaders.

---

### Step 12: Update module registrations

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\mod.rs`

Add: `pub mod cubemap;`, `pub mod texture_2d;`, `pub mod ibl;`

---

### Step 13: Update debug naming

**File**: `D:\GitProjects\LearnVulkan\src\vulkan\renderer.rs`

Add names for IBL resources and skybox. Remove old "Synthetic Environment Map" naming.

---

## File Change Summary

### New Files (11)
| File | Purpose |
|------|---------|
| src/vulkan/cubemap.rs | Cubemap struct |
| src/vulkan/texture_2d.rs | Texture2D struct for float textures |
| src/vulkan/ibl.rs | IblResources and IBL generation logic |
| shaders/cubemap_convert.vert | Shared cubemap face vertex shader |
| shaders/equirect_to_cube.frag | Equirect-to-cubemap conversion |
| shaders/irradiance_convolution.frag | Diffuse irradiance convolution |
| shaders/prefilter_env.frag | GGX prefiltered specular |
| shaders/brdf_lut.vert | BRDF LUT fullscreen triangle vertex |
| shaders/brdf_lut.frag | BRDF integration LUT fragment |
| shaders/skybox.vert | Skybox vertex shader |
| shaders/skybox.frag | Skybox fragment shader |

### Modified Files (8)
| File | Changes |
|------|---------|
| Cargo.toml | Add hdr feature |
| src/vulkan/mod.rs | Add 3 module declarations |
| src/vulkan/environment_map.rs | Replace synthetic with HDR loader |
| src/vulkan/descriptors.rs | 6 bindings, updated pool |
| src/vulkan/pipeline.rs | Add skybox pipeline |
| src/vulkan/renderer.rs | IBL, skybox, descriptor writes, drop order |
| shaders/pbr.frag | Split-sum IBL, new bindings |
| shaders/compile.bat | 8 new shader compiles |

---

## Recommended Implementation Sequence

1. Cargo.toml (hdr feature)
2. cubemap.rs + texture_2d.rs (data structures)
3. All new shaders (write and compile)
4. ibl.rs (IBL generation - core feature)
5. descriptors.rs (layout and pool)
6. pipeline.rs (skybox pipeline)
7. renderer.rs (wire everything together)
8. pbr.frag (update IBL code)
9. environment_map.rs (replace synthetic map)
10. compile.bat + mod.rs (register)
11. Debug naming
12. Test and debug

---

## Key Design Decisions

1. **Runtime cubemap generation** (not KTX2) - educational, no KTX2 parser needed
2. **Fragment shader approach** (not compute) - simpler for learning project
3. **R32G32B32A32_SFLOAT for cubemaps** - preserves HDR data
4. **Separate pipelines per IBL step** - clear and modular
5. **Environment cubemap in global descriptor set binding 5** - no separate layout for skybox
6. **CLAMP_TO_EDGE for cubemap samplers** - correct wrapping mode
7. **5 mip levels for prefilter at 128x128** - standard practice
8. **Skybox drawn first with depth write disabled** - far plane trick (gl_Position.z = w)

---

## Risks and Mitigations

1. **Cubemap face orientation in LH** - test with known environment, adjust face directions if mirrored
2. **HDR tone mapping range** - ACES handles this; add exposure multiplier later if needed
3. **Startup time (~53MB HDR)** - acceptable for learning project
4. **R32G32B32A32_SFLOAT format support** - add format checks with clear errors
5. **Skybox winding with negative viewport** - use `cull_mode = BACK` (the same as the PBR pipeline). The cube index buffer is CCW-from-outside in LH Y-up, and the Y-flip viewport is the only improper transform in the world→framebuffer chain, so the visible interior surfaces are CCW-in-framebuffer. See `docs/winding_orientation.md` §"Skybox Winding — Why `cull_mode = BACK`" for the full derivation.
