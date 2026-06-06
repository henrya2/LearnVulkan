# PBR Correction and IBL Implementation Plan

## PBR Theory: Thorough Explanation

### What is Physically Based Rendering?

PBR is a rendering approach that models light-material interaction based on physical laws rather than ad-hoc hacks. The key principles:

1. **Energy Conservation**: A surface cannot reflect more light than it receives. The sum of diffuse + specular reflected energy never exceeds the incoming energy.
2. **Microfacet Theory**: Surfaces are modeled as composed of tiny perfectly reflective facets. Roughness controls how aligned these facets are — smooth surfaces have aligned facets (clear reflections), rough surfaces have random facets (blurry reflections).
3. **Fresnel Effect**: All materials become more reflective at grazing angles. The Schlick approximation `F0 + (1-F0)(1-cosTheta)^5` models this, where `F0` is the reflectance at normal incidence.
4. **Metallic vs Dielectric**: Metals have colored reflections (F0 = baseColor) and no diffuse reflection. Non-metals have achromatic reflections (F0 ≈ 0.04) and colored diffuse. The `metallic` factor interpolates between these.

### The Cook-Torrance Specular BRDF

```
specular = D(h) * G(l,v) * F(v,h) / (4 * (n·l) * (n·v))
```

- **D (NDF — Normal Distribution Function)**: GGX/Trowbridge-Reitz. Determines how many microfacets are aligned with the half-vector H. `α² / (π * ((n·h)² * (α²-1) + 1)²)` where `α = roughness²`.
- **G (Geometry Function)**: Smith-GGX. Models microfacet self-shadowing and masking. Uses the re-mapped roughness `k = (roughness+1)²/8` for the Schlick-GGX sub-function.
- **F (Fresnel)**: Schlick approximation. Determines the ratio of specular vs diffuse reflection.

### The Full PBR Equation

```
Lo = ∫_Ω (kD * baseColor/π + kS * D*G*F / (4*n·l*n·v)) * Li * n·l dω
```

Where:
- `kD = (1-F) * (1-metallic)` — diffuse contribution (metals have no diffuse)
- `kS = F` — specular contribution
- `Li` — incoming radiance from direction ω
- The integral is over the hemisphere oriented around the surface normal

### Split-Sum Approximation for IBL

The integral is split into two parts:

```
Lo ≈ (∫_Ω Li * n·l dω_diffuse) * kD * baseColor/π  +  (∫_Ω Li dω_specular) * ∫_Ω F * G * (v·h) / (n·v * n·h) dω_brdf
```

1. **Diffuse irradiance**: `∫_Ω Li(n·l) dω` — a cubemap convolution with cosine weighting. Precomputed into a low-res irradiance cubemap.
2. **Prefiltered specular**: `∫_Ω Li dω` for specular lobes — a cubemap convolution with GGX importance sampling at various roughness levels. Stored as a mip-mapped cubemap.
3. **BRDF LUT**: `∫_Ω F * G * (v·h) / (n·v * n·h) dω` — a 2D lookup texture indexed by `(NdotV, roughness)`, outputting `(scale, bias)`.

The final IBL ambient term:
```
ambient = irradiance * kD * baseColor + prefilteredColor * (F_ambient * brdf.x + brdf.y)
```

### Current Code Issues with PBR

1. **Synthetic LDR environment map** — not physically based, produces flat/unrealistic lighting
2. **Equirectangular sampling only** — no proper cubemap, cannot use hardware cube sampling
3. **Hardcoded MAX_REFLECTION_LOD = 8.0** — not tied to actual environment mip count
4. **Ad-hoc intensity multiplier (1.5)** — compensates for missing proper irradiance convolution
5. **No BRDF LUT** — uses raw Fresnel as specular IBL, missing the geometry/visibility term
6. **No skybox rendering** — the environment is only visible on reflective surfaces, not as background

---

## Code Review Fixes

All 4 issues from `code_review_5_26.md` are **already fixed** in the current code:

| # | Issue | Status | Evidence |
|---|-------|--------|----------|
| 1 | Render pass format must match swapchain format | Fixed | `renderer.rs:55` uses `surface_format.format` for render pass |
| 2 | Swapchain cleanup ordering | Fixed | `swapchain.rs:248-262` destroys framebuffers → depth → views → swapchain |
| 3 | Legacy pipeline/shader removal | Fixed | `compile.bat` only has `pbr.vert`/`pbr.frag` |
| 4 | Persistently mapped UBO cleanup | Fixed | `renderer.rs:620` calls `unmap_memory` before destroy |

No additional code review fixes are needed.

---

## New Feature: KTX2 IBL + Skybox

### Approach: Load Pre-filtered KTX2 Files

Instead of runtime IBL generation from HDR equirectangular, we load the pre-filtered KTX2 cubemap files from the Ennis environment. This is simpler, faster at startup, and uses higher-quality pre-computed data.

### Environment Asset: Ennis

Pre-filtered KTX2 files in `assets/environment_map/ennis/` (project-relative, mirrored from the upstream `glTF-Sample-Environments/ennis/` source):

| File | KTX2 Header | Purpose |
|------|-------------|---------|
| `lambertian/outputCubeMap.ktx2` | R16G16B16A16_SFLOAT, 1024×1024, 6 faces, 11 mips, uncompressed | Environment cubemap (skybox) |
| `lambertian/diffuse.ktx2` | R16G16B16A16_SFLOAT, 1024×1024, 6 faces, 1 mip, uncompressed | Diffuse irradiance cubemap |
| `ggx/specular.ktx2` | R16G16B16A16_SFLOAT, 1024×1024, 6 faces, 11 mips, uncompressed | GGX prefiltered specular cubemap |

All files use R16G16B16A16_SFLOAT (half-float RGBA), no supercompression — the lightweight `ktx2` crate (pure Rust) is sufficient for parsing. No Basis transcoding needed.

---

### Step 1: Add dependencies to Cargo.toml

**File**: `Cargo.toml`

```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
ktx2 = "0.3"
```

No need for the `hdr` feature on `image` — KTX2 files contain the pre-filtered data directly.

---

### Step 2: Create Cubemap struct

**New file**: `src/vulkan/cubemap.rs`

```rust
pub struct Cubemap {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
    pub size: u32,
    pub mip_levels: u32,
    pub format: vk::Format,
}
```

Methods:
- `create_empty(device, instance, physical_device, size, mip_levels, format, usage) -> Self` — allocate a cube-compatible image with memory, view, and sampler
- `destroy(&self, device)` — cleanup

Key Vulkan creation details:
- `ImageCreateInfo.flags`: `CUBE_COMPATIBLE`
- `image_type`: `TYPE_2D`, `array_layers`: 6
- `extent`: `{ size, size, 1 }`
- View: `TYPE_CUBE`, `layer_count`: 6, `level_count`: mip_levels
- Sampler: `CLAMP_TO_EDGE` all axes, `LINEAR` filter, `LINEAR` mipmap, `max_lod = mip_levels - 1`

---

### Step 3: Create KTX2 cubemap loader

**New file**: `src/vulkan/ktx2_loader.rs`

Uses the `ktx2` crate to parse KTX2 files and upload cubemap data to Vulkan.

```rust
pub fn load_ktx2_cubemap(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
    path: &str,
) -> Cubemap
```

Implementation flow:
1. Read file bytes, create `ktx2::Reader::new(bytes)`
2. Extract header: `vkFormat`, `pixelWidth`, `pixelHeight`, `faceCount`, `levelCount`
3. Map `ktx2::Format` → `ash::vk::Format` using `vk::Format::from_raw(header.vk_format() as i32)`
4. Create empty `Cubemap` with the correct dimensions, mip levels, and format
5. For each mip level:
   - Get level data via `reader.levels()`
   - For a cubemap, each level contains data for all 6 faces sequentially
   - Calculate face size: `face_width * face_height * bytes_per_pixel`
   - Create staging buffer with the entire level data
   - For each face (0..6):
     - `vkCmdCopyBufferToImage` with `baseArrayLayer = face`, `mipLevel = level`, correct buffer offset
6. Add image barriers: `TRANSFER_DST` → `SHADER_READ_ONLY_OPTIMAL` for all mip levels
7. Destroy staging buffer, return `Cubemap`

KTX2 level data layout for cubemaps (uncompressed, layerCount=0):
```
Level N: [Face 0 pixels][Face 1 pixels][Face 2 pixels][Face 3 pixels][Face 4 pixels][Face 5 pixels]
```

**Register**: Add `pub mod ktx2_loader;` to `src/vulkan/mod.rs`

---

### Step 4: Create BRDF LUT generation (still needs runtime generation)

The BRDF LUT is environment-independent and must be generated at runtime.

**New file**: `src/vulkan/brdf_lut.rs`

```rust
pub struct BrdfLut {
    pub image: vk::Image,
    pub memory: vk::DeviceMemory,
    pub view: vk::ImageView,
    pub sampler: vk::Sampler,
}

pub fn generate_brdf_lut(
    ctx: &VulkanContext,
    command_pool: vk::CommandPool,
) -> BrdfLut
```

Implementation:
1. Create 512×512 `R16G16_SFLOAT` image with `COLOR_ATTACHMENT | SAMPLED` usage
2. Create offscreen render pass (color-only, CLEAR/STORE, UNDEFINED→SHADER_READ_ONLY)
3. Create BRDF LUT pipeline (brdf_lut.vert + brdf_lut.frag)
4. Create framebuffer, render fullscreen triangle
5. Transition to SHADER_READ_ONLY_OPTIMAL
6. Destroy temp objects (pipeline, framebuffer, render pass, pipeline layout, descriptor set/layout/pool)
7. Return `BrdfLut`

**Register**: Add `pub mod brdf_lut;` to `src/vulkan/mod.rs`

---

### Step 5: Write new shaders

Only 4 new shaders needed (down from 8 in the runtime-generation approach):

**5a. `shaders/brdf_lut.vert`** — Fullscreen triangle (no VBO needed)
```glsl
#version 450
layout(location = 0) out vec2 vUV;
void main() {
    // Fullscreen triangle: 3 vertices cover the entire clip space
    vec2 pos = vec2(gl_VertexIndex & 2, (gl_VertexIndex << 1) & 2);
    vUV = pos * 0.5;
    gl_Position = vec4(pos - 1.0, 0.0, 1.0);
}
```

**5b. `shaders/brdf_lut.frag`** — BRDF integration LUT
```glsl
#version 450
layout(location = 0) in vec2 vUV;
layout(location = 0) out vec2 outColor;

const float PI = 3.14159265359;

// Hammersley, GGX importance sampling, geometrySchlickGGX, geometrySmith
// (standard split-sum BRDF integration)

void main() {
    float NdotV = vUV.x;
    float roughness = vUV.y;
    // ... 1024 samples, output (scale, bias)
    outColor = vec2(scale, bias);
}
```

**5c. `shaders/skybox.vert`** — Skybox vertex shader
```glsl
#version 450
layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    // ... (same as pbr.vert)
} globals;

layout(location = 0) in vec3 inPos;
layout(location = 0) out vec3 vDirection;

void main() {
    // Strip translation from view matrix so skybox follows camera
    mat4 rotView = mat4(mat3(globals.view));
    vec4 clipPos = globals.proj * rotView * vec4(inPos, 1.0);
    gl_Position = clipPos.xyww; // force depth = far plane
    vDirection = inPos;
}
```

**5d. `shaders/skybox.frag`** — Skybox fragment shader
```glsl
#version 450
layout(set = 0, binding = 5) uniform samplerCube uEnvironmentCubemap;
layout(location = 0) in vec3 vDirection;
layout(location = 0) out vec4 outColor;

vec3 acesToneMapping(vec3 color) { /* same as pbr.frag */ }

void main() {
    vec3 color = textureLod(uEnvironmentCubemap, vDirection, 0.0).rgb;
    color = acesToneMapping(color);
    outColor = vec4(color, 1.0);
}
```

---

### Step 6: Create IBL resources struct

**New file**: `src/vulkan/ibl.rs`

```rust
pub struct IblResources {
    pub env_cubemap: Cubemap,    // 1024×1024, 11 mips, R16G16B16A16_SFLOAT (from KTX2)
    pub irradiance_map: Cubemap,  // 1024×1024, 1 mip, R16G16B16A16_SFLOAT (from KTX2)
    pub prefilter_map: Cubemap,   // 1024×1024, 11 mips, R16G16B16A16_SFLOAT (from KTX2)
    pub brdf_lut: BrdfLut,        // 512×512, R16G16_SFLOAT (generated at runtime)
}

impl IblResources {
    pub fn load(
        ctx: &VulkanContext,
        command_pool: vk::CommandPool,
        env_base_path: &str, // e.g. "assets/environment_map/ennis"
    ) -> Self {
        let env_cubemap = load_ktx2_cubemap(ctx, command_pool, &format!("{}/lambertian/outputCubeMap.ktx2", env_base_path));
        let irradiance_map = load_ktx2_cubemap(ctx, command_pool, &format!("{}/lambertian/diffuse.ktx2", env_base_path));
        let prefilter_map = load_ktx2_cubemap(ctx, command_pool, &format!("{}/ggx/specular.ktx2", env_base_path));
        let brdf_lut = generate_brdf_lut(ctx, command_pool);
        Self { env_cubemap, irradiance_map, prefilter_map, brdf_lut }
    }

    pub fn destroy(&self, device: &ash::Device) {
        self.env_cubemap.destroy(device);
        self.irradiance_map.destroy(device);
        self.prefilter_map.destroy(device);
        self.brdf_lut.destroy(device);
    }
}
```

**Register**: Add `pub mod ibl;` to `src/vulkan/mod.rs`

---

### Step 7: Update descriptor layout

**File**: `src/vulkan/descriptors.rs`

Global descriptor set layout (set 0) — 6 bindings:

| Binding | Type | Name | Stage |
|---------|------|------|-------|
| 0 | UNIFORM_BUFFER | GlobalUBO | VERTEX+FRAGMENT |
| 1 | UNIFORM_BUFFER | MaterialBuffer | FRAGMENT |
| 2 | COMBINED_IMAGE_SAMPLER | uIrradianceMap (samplerCube) | FRAGMENT |
| 3 | COMBINED_IMAGE_SAMPLER | uPrefilterMap (samplerCube) | FRAGMENT |
| 4 | COMBINED_IMAGE_SAMPLER | uBRDFLUT (sampler2D) | FRAGMENT |
| 5 | COMBINED_IMAGE_SAMPLER | uEnvironmentCubemap (samplerCube) | FRAGMENT |

Material descriptor set (set 1) — unchanged (5 texture bindings).

Pool sizes:
- UNIFORM_BUFFER: `frames * 2`
- COMBINED_IMAGE_SAMPLER: `num_materials * 5 + frames * 4`

---

### Step 8: Add skybox pipeline

**File**: `src/vulkan/pipeline.rs`

Add `create_skybox_pipeline(device, render_pass, extent, global_layout)`:
- Vertex input: `vec3 position` (stride 12B, `R32G32B32_SFLOAT` at location 0)
- Shaders: `skybox.vert.spv` + `skybox.frag.spv`
- Depth: test=true, **write=false**, compare=**LESS_OR_EQUAL**
- Rasterizer: cull_mode=**FRONT**, front_face=COUNTER_CLOCKWISE (viewing from inside cube)
- Color blend: no blending, RGBA write mask
- Descriptor layouts: [global_layout]
- Push constants: none (skybox uses GlobalUBO directly)

**Note**: The skybox pipeline shares the main render pass (color + depth attachments). Skybox is drawn first, before PBR geometry.

---

### Step 9: Update Renderer

**File**: `src/vulkan/renderer.rs`

**Struct changes**:
- Remove `env_map: Texture`
- Add `ibl: IblResources`
- Add `skybox_pipeline: PipelineData`
- Add `skybox_vertex_buffer: GpuBuffer`
- Add `skybox_index_buffer: GpuBuffer`
- Add `skybox_index_count: u32`

**Initialization flow**:
1. After command pool creation, load IBL: `IblResources::load(ctx, command_pool, env_path)`
2. Create skybox pipeline after PBR pipeline
3. Create skybox vertex/index buffers (unit cube, positions only)
4. Update descriptor writes: 6 bindings per global set
5. Remove `create_synthetic_environment_map` call

**`record_command_buffer` changes**:
1. After beginning render pass + setting viewport, draw skybox:
   - Bind skybox pipeline
   - Bind global descriptor set (set 0)
   - Bind skybox vertex/index buffers
   - `cmd_draw_indexed(skybox_index_count, 1, 0, 0, 0)`
2. Then bind PBR pipeline and draw scene (existing code)

**Drop order**: ibl → skybox_pipeline → skybox buffers → (rest unchanged)

**Debug naming**: Name IBL cubemaps, BRDF LUT, skybox pipeline objects

---

### Step 10: Update PBR fragment shader

**File**: `shaders/pbr.frag`

Remove:
- `layout(set = 0, binding = 2) uniform sampler2D uEnvironmentMap;`
- `sampleSphericalMap()` function
- `MAX_REFLECTION_LOD` constant
- Old simplified IBL code block

Add:
```glsl
layout(set = 0, binding = 2) uniform samplerCube uIrradianceMap;
layout(set = 0, binding = 3) uniform samplerCube uPrefilterMap;
layout(set = 0, binding = 4) uniform sampler2D uBRDFLUT;
```

New IBL section (replacing the old simplified IBL block):
```glsl
// Split-sum IBL
vec3 F_ambient = fresnelSchlickRoughness(NdotV, F0, roughness);
vec3 kD_ambient = (vec3(1.0) - F_ambient) * (1.0 - metallic);

// Diffuse IBL
vec3 irradiance = texture(uIrradianceMap, N).rgb;
vec3 diffuse_ibl = irradiance * kD_ambient * baseColor;

// Specular IBL
vec3 R = reflect(-V, N);
const float MAX_PREFILTER_LOD = 10.0; // prefilter_map has 11 mip levels (0..10)
vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * MAX_PREFILTER_LOD).rgb;
vec2 brdf = texture(uBRDFLUT, vec2(NdotV, roughness)).rg;
vec3 specular_ibl = prefilteredColor * (F_ambient * brdf.x + brdf.y);

vec3 ambient = (diffuse_ibl + specular_ibl) * occlusion;
```

---

### Step 11: Remove environment_map.rs

**File**: `src/vulkan/environment_map.rs`

Delete the file entirely (or replace with a re-export from ibl.rs). Remove `pub mod environment_map;` from `mod.rs`.

The synthetic environment map function is no longer needed — replaced by KTX2 loading.

---

### Step 12: Update compile.bat

**File**: `shaders/compile.bat`

Add 4 new compile commands:
```bat
glslc brdf_lut.vert -o brdf_lut.vert.spv
glslc brdf_lut.frag -o brdf_lut.frag.spv
glslc skybox.vert -o skybox.vert.spv
glslc skybox.frag -o skybox.frag.spv
```

---

### Step 13: Update module registrations

**File**: `src/vulkan/mod.rs`

- Remove: `pub mod environment_map;`
- Add: `pub mod cubemap;`, `pub mod ktx2_loader;`, `pub mod brdf_lut;`, `pub mod ibl;`

---

### Step 14: Update debug naming

**File**: `src/vulkan/renderer.rs`

- Remove `name_texture(dm, &self.env_map, "Synthetic Environment Map")`
- Add naming for IBL resources (env cubemap, irradiance, prefilter, BRDF LUT)
- Add naming for skybox pipeline, vertex/index buffers

---

## File Change Summary

### New Files (7)

| File | Purpose |
|------|---------|
| `src/vulkan/cubemap.rs` | Cubemap struct for 6-face cube images |
| `src/vulkan/ktx2_loader.rs` | KTX2 file parser + Vulkan cubemap upload |
| `src/vulkan/brdf_lut.rs` | BRDF LUT generation at runtime |
| `src/vulkan/ibl.rs` | IblResources: loads KTX2 cubemaps + generates BRDF LUT |
| `shaders/brdf_lut.vert` | BRDF LUT fullscreen vertex shader |
| `shaders/brdf_lut.frag` | BRDF integration LUT fragment shader |
| `shaders/skybox.vert` | Skybox vertex shader |
| `shaders/skybox.frag` | Skybox fragment shader |

### Modified Files (7)

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `ktx2 = "0.3"` dependency |
| `src/vulkan/mod.rs` | Remove environment_map, add cubemap/ktx2_loader/brdf_lut/ibl |
| `src/vulkan/descriptors.rs` | 6 global bindings, updated pool sizes |
| `src/vulkan/pipeline.rs` | Add `create_skybox_pipeline()` |
| `src/vulkan/renderer.rs` | IBL resources, skybox, descriptor writes, drop order |
| `shaders/pbr.frag` | Split-sum IBL with cubemaps + BRDF LUT |
| `shaders/compile.bat` | 4 new shader compile commands |

### Deleted Files (1)

| File | Reason |
|------|--------|
| `src/vulkan/environment_map.rs` | Replaced by KTX2 loader in ibl.rs |

---

## Recommended Implementation Order

1. **Cargo.toml** — add `ktx2` dependency
2. **cubemap.rs** — Cubemap struct
3. **ktx2_loader.rs** — KTX2 parsing + Vulkan upload
4. **Shaders** — brdf_lut.vert, brdf_lut.frag, skybox.vert, skybox.frag
5. **brdf_lut.rs** — BRDF LUT generation
6. **ibl.rs** — IblResources orchestration
7. **descriptors.rs** — layout and pool updates
8. **pipeline.rs** — skybox pipeline
9. **renderer.rs** — wire everything together
10. **pbr.frag** — update IBL code
11. **compile.bat + mod.rs** — register and compile
12. **Delete environment_map.rs** — remove old synthetic map
13. **Debug naming** — RenderDoc labels

---

## Key Design Decisions

1. **KTX2 pre-filtered files** (not runtime generation) — simpler code, faster startup, higher quality (1024×1024 vs 512×512/128×128)
2. **`ktx2` crate** (pure Rust parser, not `ktx2-rw`) — no C++ build needed, files are uncompressed R16G16B16A16_SFLOAT
3. **BRDF LUT still generated at runtime** — it's environment-independent, tiny (512×512), and fast to compute
4. **R16G16B16A16_SFLOAT for cubemaps** — matches the KTX2 file format, good HDR precision with half the memory of R32G32B32A32_SFLOAT
5. **Environment cubemap in binding 5** — skybox shares the global descriptor set
6. **CLAMP_TO_EDGE for cubemap samplers** — correct wrapping for cubemaps
7. **Skybox drawn first with depth write disabled** — uses `gl_Position.z = w` far-plane trick
8. **FRONT face culling for skybox** — viewing the cube from inside
9. **MAX_PREFILTER_LOD = 10.0** — matches the 11 mip levels (0..10) in the specular KTX2

---

## Comparison: KTX2 Loading vs Runtime Generation

| Aspect | KTX2 Loading (chosen) | Runtime Generation |
|--------|----------------------|-------------------|
| Startup time | Fast (~170MB file reads + GPU uploads) | Slow (multiple GPU render passes) |
| Code complexity | Low (file parse + upload) | High (8 shaders, offscreen rendering) |
| New shaders | 4 (brdf_lut + skybox) | 8 (cubemap convert + irradiance + prefilter + brdf_lut + skybox) |
| New Rust files | 4 (cubemap, ktx2_loader, brdf_lut, ibl) | 4 (cubemap, texture_2d, ibl, environment_map) |
| Cubemap quality | 1024×1024, 11 mips (pre-filtered) | 512×512 env, 32×32 irradiance, 128×128 prefilter |
| Dependencies | `ktx2` crate (pure Rust) | `image` with `hdr` feature |
| Flexibility | Limited to pre-filtered environments | Any HDR equirectangular |

---

## Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| KTX2 face orientation in LH | Test with known environment; may need to flip Z on sample direction in shaders |
| R16G16B16A16_SFLOAT format support | Add format checks with `get_physical_device_format_properties` |
| Skybox winding with negative viewport | `cull_mode = BACK` for skybox; the cube is CCW-from-outside in LH Y-up, and the only improper transform in the world→framebuffer chain is the Y-flip viewport, so the visible interior surfaces are CCW-in-framebuffer. See `docs/winding_orientation.md` §"Skybox Winding — Why `cull_mode = BACK`" for the full derivation. |
| KTX2 level data layout | Verify by reading header; face data is sequential within each level |
| BRDF LUT pipeline temp resources | Clean up pipeline, framebuffer, render pass after generation |
| Large KTX2 files (~170MB total) | Acceptable for a learning project; files are memory-mapped by OS |
