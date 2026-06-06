# glTF PBR Rendering Plan: DamagedHelmet

## Overview

This plan describes the architecture, implementation steps, and design decisions for loading and rendering a glTF 2.0 model (specifically **DamagedHelmet** from KhronosGroup/glTF-Sample-Models) with correct PBR shading in the existing Vulkan FPS camera demo.

**Goals:**
- Load glTF 2.0 models using the `gltf` crate
- Implement metallic-roughness PBR with image-based lighting (IBL)
- Match the DamagedHelmet screenshot lighting (studio-like environment)
- Refactor renderer into a modular, multi-material, multi-mesh architecture
- Zero Vulkan validation warnings or errors
- Preserve existing FPS camera, coordinate system, and windowing behavior

**Non-goals:**
- Animation / skinning (static mesh only)
- Morph targets
- MSAA (keep single-sample)
- Post-processing beyond tone mapping

---

## 1. Crate Selection

| Crate | Version | Purpose |
|-------|---------|---------|
| `gltf` | `1.4` | Parse `.gltf` / `.glb` files, extract scenes, meshes, materials, textures, accessors |
| `base64` | `0.22` | Decode embedded base64 buffers in `.gltf` (gltf crate may handle this, but keep as fallback) |

The `gltf` crate provides the `gltf::Document` API. We will use `gltf::import` which resolves external `.bin` and image files automatically.

**Cargo.toml additions:**
```toml
gltf = "1.4"
```

---

## 2. Coordinate System Conversion

glTF uses a **right-handed, Y-up** coordinate system. This project uses **left-handed, Y-up** with `+Z` forward.

**Conversion strategy:** When extracting node transforms and vertex positions from glTF, convert from RH to LH by negating the Z component of positions and the Z basis of transforms. Specifically:

- Vertex position: `(x, y, z)` -> `(x, y, -z)`
- Node transform matrix: multiply by `diag(1, 1, -1, 1)` on both sides, or more practically, after computing the glTF `world_matrix`, convert it:
  ```
  let rh_to_lh = Mat4::from_diagonal(Vec4::new(1.0, 1.0, -1.0, 1.0));
  let lh_matrix = rh_to_lh * rh_matrix * rh_to_lh;
  ```
- Normal vectors: since the conversion is a reflection, normals must be transformed by the inverse-transpose of the LH matrix. Tangents (xyzw) have their xyz reflected similarly; the handedness bit `w` must be flipped (`w = -w`) because the reflection changes winding.

**Winding order:** glTF specifies CCW front faces in RH space. The vertex Z-negation is an improper transform (det = −1) that flips winding to CW-from-outside in LH world space. The negative-height viewport applies a second improper transform (y-axis reflection) that flips winding again. Vulkan determines front/back from the signed area in framebuffer coordinates (Vulkan 1.3 §28.4), so the two flips cancel and final framebuffer winding is CCW — matching `FRONT_FACE_COUNTER_CLOCKWISE`. See `docs/winding_orientation.md` for the full derivation.

---

## 3. New Directory Structure

```
src/
  main.rs
  app.rs
  camera.rs
  input.rs
  mesh.rs                 # keep procedural helpers, add PbrVertex
  scene/
    mod.rs                # re-exports
    gltf_loader.rs        # glTF parsing -> SceneGraph + GpuMesh + Material
    scene_graph.rs        # Node hierarchy, world transform computation
    material.rs           # PbrMaterial definition, texture indices
    model.rs              # GpuMesh: vb/ib + index_count + material_index
  vulkan/
    mod.rs
    context.rs
    buffer.rs
    swapchain.rs
    texture.rs            # extend for non-mipmapped, different formats
    descriptors.rs        # new descriptor set layouts for PBR
    pipeline.rs           # PBR pipeline creation
    renderer.rs           # refactored: owns Scene, materials, textures, UBOs
    pbr_ubo.rs            # uniform buffer structs (bytemuck POD)
    environment_map.rs    # cubemap generation / loading for IBL (replaced by ibl.rs + KTX2 loader under `assets/environment_map/ennis/`)
```

---

## 4. Vertex Format

Replace the single `Vertex` with `PbrVertex` for glTF meshes. Keep the old `Vertex` for the procedural cube/floor if they remain (or remove them if the scene is glTF-only).

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PbrVertex {
    pub pos: [f32; 3],      // location 0
    pub normal: [f32; 3],   // location 1
    pub tangent: [f32; 4],  // location 2 (xyz + handedness in w)
    pub uv0: [f32; 2],      // location 3
}
```

**Stride:** 48 bytes.

**Attribute descriptions:**
| Location | Format | Offset |
|----------|--------|--------|
| 0 | `R32G32B32_SFLOAT` | 0 |
| 1 | `R32G32B32_SFLOAT` | 12 |
| 2 | `R32G32B32A32_SFLOAT` | 24 |
| 3 | `R32G32_SFLOAT` | 40 |

**Loading from glTF:**
- Use `gltf::Primitive::reader(...)` to read `Positions`, `Normals`, `Tangents`, `TexCoords(0)`.
- If tangents are missing, compute MikkTSpace tangents offline or approximate via `normal.cross(any_vec).cross(normal)`. For DamagedHelmet, tangents are provided.
- If normals are missing, compute per-vertex averaged face normals.
- Interleave into `PbrVertex` vec. Upload via existing `create_device_local_buffer`.

---

## 5. Material System

### 5.1 PbrMaterial (CPU-side)

```rust
pub struct PbrMaterial {
    pub base_color_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub emissive_factor: [f32; 3],
    pub base_color_texture: Option<usize>,   // index into global texture array
    pub metallic_roughness_texture: Option<usize>,
    pub normal_texture: Option<usize>,
    pub occlusion_texture: Option<usize>,
    pub emissive_texture: Option<usize>,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
}
```

### 5.2 GPU Material Buffer

Instead of per-material descriptor sets (which would require many layouts or dynamic indexing), use a **single SSBO** (Shader Storage Buffer Object) containing an array of `GpuMaterial` structs, indexed by `material_index` passed via push constant or a per-draw UBO field.

**Alternative (chosen approach):** Use a **uniform buffer** for materials if the count is low (< few hundred). For DamagedHelmet there is 1 material, so a UBO is simplest and avoids `VK_KHR_buffer_device_address` requirements.

`GpuMaterial` (bytemuck POD, 64-byte aligned naturally):
```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GpuMaterial {
    pub base_color_factor: [f32; 4],
    pub emissive_factor: [f32; 3],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
    pub pad: [f32; 1], // align to 16 bytes
    pub texture_indices: [i32; 5], // base_color, metallic_roughness, normal, occlusion, emissive; -1 = none
}
```

Upload as one device-local buffer. In the shader, index with `push_constants.material_index`.

### 5.3 Texture Array Management

All textures referenced by the glTF are loaded into a `Vec<Texture>` in the renderer. The shader uses `sampler2D` arrays or separate bindings. Since Vulkan descriptor indexing is complex, use **fixed bindings per texture type**:

| Binding | Type | Count | Stage |
|---------|------|-------|-------|
| 2 | `COMBINED_IMAGE_SAMPLER` | 1 | Fragment | base_color |
| 3 | `COMBINED_IMAGE_SAMPLER` | 1 | Fragment | metallic_roughness |
| 4 | `COMBINED_IMAGE_SAMPLER` | 1 | Fragment | normal |
| 5 | `COMBINED_IMAGE_SAMPLER` | 1 | Fragment | occlusion |
| 6 | `COMBINED_IMAGE_SAMPLER` | 1 | Fragment | emissive |

For materials that lack a texture, bind semantic 1x1 fallback textures: white sRGB for base_color, black sRGB for emissive, white linear for occlusion, linear `[128,128,255,255]` for normal, and linear `[255,255,255,255]` for metallic_roughness so roughness/metallic scalar factors are preserved.

**Simpler approach for single-material model:** Since DamagedHelmet has exactly 1 material, we can bind all 5 textures directly and skip the material buffer / push constant index. But the plan should be general. **Decision:** implement the general multi-material path with push constants and a material buffer, because it is only marginally more complex and avoids a future refactor.

---

## 6. Uniform Buffers & Push Constants

### 6.1 Per-Frame Global UBO

Replace the single `mat4 mvp` UBO with a `GlobalUniforms` struct:

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: [f32; 16],
    pub proj: [f32; 16],
    pub camera_pos: [f32; 3],
    pub _pad: f32,
    pub light_dir: [f32; 3],      // directional light direction (normalized)
    pub light_intensity: f32,
}
```

Size: 160 bytes. Still `HOST_VISIBLE | HOST_COHERENT`, persistently mapped, one per frame.

### 6.2 Per-Draw Push Constants

Use a 16-byte push constant range for per-draw data:

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: [f32; 16],         // 64 bytes
    pub material_index: u32,      // 4 bytes
    pub _pad: [u32; 3],           // 12 bytes
}
```

Total: 80 bytes. Well within the guaranteed 128-byte push constant limit.

Push constants are updated per draw call in `record_command_buffer`:
```rust
device.cmd_push_constants(cmd, pipeline_layout, ShaderStageFlags::VERTEX | ShaderStageFlags::FRAGMENT, 0, bytes);
```

---

## 7. Shader Architecture

### 7.1 Vertex Shader (`pbr.vert`)

```glsl
#version 450

layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view;
    mat4 proj;
    vec3 cameraPos;
    float _pad0;
    vec3 lightDir;
    float lightIntensity;
} globals;

layout(push_constant) uniform PushConstants {
    mat4 model;
    uint materialIndex;
} pc;

layout(location = 0) in vec3 inPos;
layout(location = 1) in vec3 inNormal;
layout(location = 2) in vec4 inTangent;
layout(location = 3) in vec2 inUV;

layout(location = 0) out vec3 vWorldPos;
layout(location = 1) out vec3 vNormal;
layout(location = 2) out vec4 vTangent;
layout(location = 3) out vec2 vUV;

void main() {
    vec4 worldPos = pc.model * vec4(inPos, 1.0);
    vWorldPos = worldPos.xyz;
    gl_Position = globals.proj * globals.view * worldPos;

    mat3 normalMatrix = transpose(inverse(mat3(pc.model)));
    vNormal = normalize(normalMatrix * inNormal);
    vTangent = vec4(normalize(normalMatrix * inTangent.xyz), inTangent.w);
    vUV = inUV;
}
```

### 7.2 Fragment Shader (`pbr.frag`)

Implements the **glTF 2.0 metallic-roughness PBR model** with IBL.

**Inputs:**
- `vWorldPos`, `vNormal`, `vTangent`, `vUV`
- Global UBO: `cameraPos`, `lightDir`, `lightIntensity`
- Material UBO (or push constant indexed into SSBO)
- Textures: base_color, metallic_roughness, normal, occlusion, emissive
- Environment cubemap for diffuse and specular IBL

**Lighting model (simplified but correct):**

1. **Sample material textures:**
   - `base_color = texture(uBaseColor, vUV).rgb * material.baseColorFactor.rgb`
   - `metallic = clamp(texture(uMetallicRoughness, vUV).b * material.metallicFactor, 0.0, 1.0)`
   - `roughness = clamp(texture(uMetallicRoughness, vUV).g * material.roughnessFactor, 0.045, 1.0)`
   - `normal_sample = texture(uNormal, vUV).rgb`
   - `occlusion = mix(1.0, texture(uOcclusion, vUV).r, material.occlusionStrength)`
   - `emissive = texture(uEmissive, vUV).rgb * material.emissiveFactor`

2. **Normal mapping:**
   - Decode: `normal_sample = normal_sample * 2.0 - 1.0`
   - Apply `normalScale` to XY and normalize the tangent-space normal.
   - Re-orthogonalize tangent against normal, then build TBN from `vNormal`, `vTangent.xyz`, and `cross(vNormal, vTangent.xyz) * vTangent.w`.
   - `N = normalize(TBN * normal_sample)`

3. **View vector:** `V = normalize(globals.cameraPos - vWorldPos)`

4. **Directional light contribution (analytic):**
   - Use a single directional light to approximate the studio lighting in the DamagedHelmet screenshot.
   - Light direction: roughly `(-0.5, -1.0, 0.5)` (from upper-left-front), intensity `3.0`.
   - Implement Cook-Torrance BRDF:
     - `D` (GGX/Trowbridge-Reitz)
     - `G` (Smith with Schlick-GGX)
     - `F` (Schlick Fresnel)
   - `F0 = mix(vec3(0.04), base_color, metallic)`
   - `kS = F`, `kD = (1.0 - kS) * (1.0 - metallic)`
   - `Lo = (kD * base_color / PI + specular) * NdotL * globals.lightIntensity * lightColor`

5. **Image-Based Lighting (IBL):**
   - **Diffuse:** Sample a pre-filtered irradiance cubemap with `N`. Multiply by `kD * base_color`.
   - **Specular:** Sample a pre-filtered radiance cubemap with `R = reflect(-V, N)` and LOD based on roughness. Multiply by `kS * envBRDF(NdotV, roughness)` where `envBRDF` is sampled from a 2D LUT (or approximated).
   - **Simplification for plan:** Since generating irradiance / radiance / BRDF LUT cubemaps offline is a large task, we can approximate IBL with a single pre-convolved environment cubemap and a simplified Fresnel term. However, to match the screenshot quality, proper IBL is strongly recommended.
   - **Decision (as implemented):** Load the pre-filtered Ennis KTX2 cubemaps from `assets/environment_map/ennis/` (project-relative — see `correct_pbr_plan.md` and `CODEBUDDY.md` for the path layout). The BRDF LUT is generated procedurally on the GPU at startup. This avoids runtime equirect-to-cube and convolution shaders.

6. **Ambient occlusion:** Apply glTF occlusion strength as `mix(1.0, ao, strength)`. In the current simplified IBL shader, AO is applied to diffuse environment lighting.

7. **Emissive:** Add `emissive`.

8. **Tone mapping and output transform:** Apply ACES filmic tone mapping and output linear color to the sRGB swapchain attachment. Do not manually gamma-correct in `pbr.frag` while rendering to the sRGB attachment.

**Fallback for no IBL:** If IBL cubemaps are not available, use a simple ambient term `vec3(0.03) * base_color * occlusion` plus the directional light. This will not match the screenshot but will still show correct PBR under the analytic light.

---

## 8. IBL Pipeline

To match the DamagedHelmet screenshot, we need an environment map that provides soft, studio-like reflections.

### 8.1 Environment Map Source

Download a small HDR environment map (e.g., from Poly Haven: `studio_small_09_4k.hdr`) or generate a synthetic studio cubemap. Convert to a 6-face cubemap format or use `R32G32B32A32_SFLOAT` 2D equirectangular texture and sample with a conversion function in shader.

**Simpler approach:** Use an equirectangular HDR map. In the fragment shader, convert `direction` to equirectangular UVs:
```glsl
vec2 sampleSphericalMap(vec3 v) {
    vec2 uv = vec2(atan(v.z, v.x), asin(v.y));
    uv *= vec2(0.1591, 0.3183); // 1/(2PI), 1/PI
    uv += 0.5;
    return uv;
}
```

### 8.2 Runtime Cubemap Generation

1. **Load HDR equirectangular map** into a 2D `R32G32B32A32_SFLOAT` texture with mipmaps.
2. **Equirectangular-to-Cubemap:** Render 6 faces of a cubemap by drawing a full-screen triangle/cube with a shader that samples the equirectangular map. Use a one-time command buffer with 6 render passes (or a geometry shader / layered rendering if supported).
3. **Irradiance Convolution:** For each cubemap face, render to a lower-resolution cubemap (e.g., 32x32) sampling the environment map and integrating over the hemisphere (diffuse convolution). Use a compute shader or fragment shader with a large number of samples (e.g., importance sampling).
4. **Prefiltered Radiance:** For each roughness level (e.g., 5 mip levels of a 128x128 cubemap), convolve the environment map with a GGX NDF. Again, use compute or fragment shader.
5. **BRDF LUT:** Generate a 256x256 2D LUT for `F0 * scale + bias` using the split-sum approximation. This can be precomputed offline and embedded as a `.spv` texture or generated at startup in a compute pass.

**Validation considerations:**
- Ensure all image layout transitions use proper pipeline barriers.
- Cubemap images must have `TRANSFER_DST`, `SAMPLED`, and optionally `COLOR_ATTACHMENT` usage.
- When rendering to cubemap faces, use `IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL` and transition to `SHADER_READ_ONLY_OPTIMAL` after.

### 8.3 Simplified IBL (Recommended for First Implementation)

To reduce scope while still looking good:
- Skip runtime convolution. Instead, use the raw HDR equirectangular map for both diffuse and specular, sampling with blurred LODs for diffuse (high mip) and sharp LODs for specular (low mip).
- Skip the BRDF LUT; approximate `envBRDF` with `F0 * (1 - NdotV) + NdotV` or similar Schlick approximation.
- This is not physically correct but produces visually pleasing results with far less code.

**Decision:** The plan describes the full IBL pipeline but marks the simplified path as Phase 1. The full convolution can be Phase 2.

---

## 9. Renderer Refactoring

### 9.1 New Types

```rust
pub struct GpuMesh {
    pub vertex_buffer: GpuBuffer,
    pub index_buffer: GpuBuffer,
    pub index_count: u32,
    pub material_index: usize,
}

pub struct Scene {
    pub meshes: Vec<GpuMesh>,
    pub materials: Vec<PbrMaterial>,
    pub textures: Vec<Texture>,
    pub material_buffer: GpuBuffer, // device-local, one GpuMaterial per material
    pub fallback_textures: FallbackTextures, // 1x1 defaults
}

pub struct FallbackTextures {
    pub white_srgb: Texture,                 // base color fallback
    pub white_linear: Texture,               // occlusion fallback
    pub black_srgb: Texture,                 // emissive fallback
    pub normal_linear: Texture,              // [128,128,255,255]
    pub metallic_roughness_linear: Texture,  // [255,255,255,255]
}
```

### 9.2 Renderer Fields

Replace hard-coded cube/floor fields with `scene: Scene`.

```rust
pub struct Renderer {
    pub device: ash::Device,
    pub swapchain: SwapchainData,
    pub pipeline: PipelineData,
    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub image_available: Vec<vk::Semaphore>,
    pub render_finished: Vec<vk::Semaphore>,
    pub in_flight: Vec<vk::Fence>,
    pub images_in_flight: Vec<Option<vk::Fence>>,
    pub current_frame: usize,
    pub framebuffer_resized: bool,

    // Scene data
    pub scene: Scene,

    // Per-frame global UBOs
    pub global_uniforms: Vec<GpuBuffer>,
    pub global_mapped: Vec<*mut u8>,

    // Descriptor set layout and pool
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    pub descriptor_sets: Vec<vk::DescriptorSet>,

    // Environment map (optional, for IBL)
    pub env_map: Option<EnvironmentMap>,
}
```

### 9.3 Descriptor Set Layout (Updated)

```rust
// set=0, binding=0: GlobalUBO (VERTEX + FRAGMENT)
// set=0, binding=1: Material buffer (SSBO or UBO) (FRAGMENT)
// set=0, binding=2: base_color texture (FRAGMENT)
// set=0, binding=3: metallic_roughness texture (FRAGMENT)
// set=0, binding=4: normal texture (FRAGMENT)
// set=0, binding=5: occlusion texture (FRAGMENT)
// set=0, binding=6: emissive texture (FRAGMENT)
// set=0, binding=7: environment map (FRAGMENT) - optional
```

Since the number of textures per material varies, but we have a global texture array, we use **one descriptor set per frame** that binds the currently-active material's textures. However, Vulkan does not allow rebinding individual descriptors within a set efficiently without `VK_KHR_descriptor_indexing`.

**Better approach:** Bind **all textures at once** using an array of samplers. But `gltf` models may have many textures. For DamagedHelmet there are ~5 textures.

**Chosen approach for correctness and simplicity:**
- Create a descriptor set layout with **one binding per texture type**, each binding a single `COMBINED_IMAGE_SAMPLER`.
- In `record_command_buffer`, for each draw call, bind the specific `Texture` handles for that material. Since all materials in the model share the same layout, we can use `vkCmdBindDescriptorSets` per draw call with per-material descriptor sets.
- Create a `VkDescriptorPool` sized for `MAX_FRAMES_IN_FLIGHT * max_materials` sets, and allocate one set per material per frame (or one set per material, updated each frame if textures don't change).

**Even simpler:** Since textures are static after loading, allocate **one descriptor set per material** (not per frame) that binds its 5 textures. The global UBO is still per-frame. Use two descriptor sets:
- Set 0 (per-frame): global UBO
- Set 1 (per-material): material textures

This requires two descriptor set layouts. The pipeline layout binds both.

```rust
let set_layouts = [global_layout, material_layout];
```

In `record_command_buffer`:
```rust
device.cmd_bind_descriptor_sets(cmd, GRAPHICS, pipeline_layout, 0, &[global_set[frame]], &[]);
for mesh in &scene.meshes {
    device.cmd_bind_descriptor_sets(cmd, GRAPHICS, pipeline_layout, 1, &[material_set[mesh.material_index]], &[]);
    // draw mesh
}
```

This is clean, correct, and avoids descriptor indexing extensions.

### 9.4 Pipeline Layout

```rust
let push_constant_range = vk::PushConstantRange::default()
    .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
    .offset(0)
    .size(std::mem::size_of::<PushConstants>() as u32);

let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
    .set_layouts(&set_layouts)
    .push_constant_ranges(std::slice::from_ref(&push_constant_range));
```

### 9.5 Draw Loop

```rust
fn record_command_buffer(...) {
    // ... begin render pass, set viewport/scissor, bind pipeline ...

    // Bind per-frame global descriptor set (set = 0)
    device.cmd_bind_descriptor_sets(cmd, GRAPHICS, pipeline_layout, 0, &[global_set[frame]], &[]);

    for mesh in &scene.meshes {
        let pc = PushConstants {
            model: mesh.world_matrix.to_cols_array(),
            material_index: mesh.material_index as u32,
            ..Default::default()
        };
        let pc_bytes = bytemuck::bytes_of(&pc);
        device.cmd_push_constants(cmd, pipeline_layout, VERTEX | FRAGMENT, 0, pc_bytes);

        // Bind per-material descriptor set (set = 1)
        device.cmd_bind_descriptor_sets(cmd, GRAPHICS, pipeline_layout, 1, &[material_sets[mesh.material_index]], &[]);

        device.cmd_bind_vertex_buffers(cmd, 0, &[mesh.vertex_buffer.buffer], &[0]);
        device.cmd_bind_index_buffer(cmd, mesh.index_buffer.buffer, 0, UINT32);
        device.cmd_draw_indexed(cmd, mesh.index_count, 1, 0, 0, 0);
    }
}
```

---

## 10. glTF Loading Pipeline

### 10.1 Loading Steps

1. **Call `gltf::import("assets/models/DamagedHelmet/DamagedHelmet.gltf")`** -> returns `(Document, Vec<BufferData>, Vec<ImageData>)`.
2. **Build `SceneGraph`:** Select `document.default_scene()` or scene 0 fallback, compute local transforms (matrix / TRS), and build the hierarchy only for reachable active-scene nodes.
3. **Compute world transforms:** DFS from root nodes, multiplying parent transforms. Convert RH to LH during this step.
4. **Extract meshes:** For each node with a mesh, iterate primitives.
   - Read positions, normals, tangents, texcoords(0), indices.
   - Build `PbrVertex` vec, upload to GPU.
   - Store `GpuMesh` with world transform and material index.
5. **Extract materials:** For each `gltf::Material`, read pbr_metallic_roughness values and texture indices.
   - Map glTF texture indices to our `Vec<Texture>` indices.
   - Load images from `ImageData` (already decoded by `gltf::import`).
   - Handle `sampler` wrap modes and min/mag filters. glTF samplers define `wrapS`, `wrapT`, `magFilter`, `minFilter`. Create one `vk::Sampler` per unique glTF sampler, or create samplers per texture with the correct parameters.
   - **Important:** glTF images may be in various formats (PNG, JPEG). The `gltf` crate with the `import` feature decodes them to raw pixels. Convert supported formats to RGBA8, then use `Texture::from_rgba8_with_format` with the texture semantic: base-color/emissive use `R8G8B8A8_SRGB`; normal/metallic-roughness/occlusion use `R8G8B8A8_UNORM`.
6. **Create fallback textures:** semantic 1x1 textures for missing maps.
7. **Upload material buffer:** Create one `GpuBuffer` with all `GpuMaterial` structs.

### 10.2 Texture Loading Details

`gltf::import` gives us `Vec<image::DynamicImage>` (or raw bytes depending on feature flags). With `gltf` crate features:
```toml
gltf = { version = "1.4", features = ["import", "utils"] }
```

The `ImageData` is `Vec<u8>` of the raw file bytes. We must decode PNG/JPEG ourselves using the `image` crate (already a dependency). However, `gltf::import` with the `import` feature actually decodes images automatically if the `image` crate is available.

Actually, `gltf` 1.4's `import` feature returns `gltf::image::Data` which contains `pixels: Vec<u8>, width: u32, height: u32, format: Format`. The format is `R8G8B8A8` or `R8G8B8`. If `R8G8B8`, pad to RGBA8 before upload.

**Sampler mapping:**
```rust
fn create_sampler_from_gltf(device: &ash::Device, sampler: &gltf::texture::Sampler) -> vk::Sampler {
    let address_mode_u = match sampler.wrap_s() {
        MirroredRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        Repeat => vk::SamplerAddressMode::REPEAT,
        ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
    };
    // similar for wrap_t
    // mag_filter: Linear -> LINEAR, Nearest -> NEAREST
    // min_filter: similar, with mipmap mode
}
```

---

## 11. Environment Map Implementation

### 11.1 HDR Equirectangular to Cubemap

**Vertex shader (`equirect_to_cube.vert`):** Passes local cube vertex positions as `outPos`.

**Fragment shader (`equirect_to_cube.frag`):**
```glsl
#version 450
layout(location = 0) in vec3 localPos;
layout(location = 0) out vec4 outColor;
layout(set = 0, binding = 0) uniform sampler2D equirectangularMap;

const vec2 invAtan = vec2(0.1591, 0.3183);
vec2 sampleSphericalMap(vec3 v) {
    vec2 uv = vec2(atan(v.z, v.x), asin(v.y));
    uv *= invAtan;
    uv += 0.5;
    return uv;
}

void main() {
    vec2 uv = sampleSphericalMap(normalize(localPos));
    outColor = texture(equirectangularMap, uv);
}
```

**Rendering:**
- Create a cubemap image: `TYPE_2D` with `array_layers=6`, `flags=CUBE_COMPATIBLE`.
- Create 6 image views (one per face) or one cubemap view.
- Create a framebuffer per face with a 512x512 render pass.
- Render 6 times, each time setting the viewport and binding the appropriate framebuffer. Use a push constant or uniform to pass the view-projection matrix for each face.
- View matrices look along +X, -X, +Y, -Y, +Z, -Z. Projection is 90-degree FOV.

### 11.2 Irradiance Convolution

Same setup, but fragment shader integrates over the hemisphere:
```glsl
vec3 irradiance = vec3(0.0);
vec3 up = vec3(0.0, 1.0, 0.0);
vec3 right = cross(N, up);
up = cross(right, N);
float sampleDelta = 0.025;
int nrSamples = 0;
for(float phi = 0.0; phi < 2.0 * PI; phi += sampleDelta) {
    for(float theta = 0.0; theta < 0.5 * PI; theta += sampleDelta) {
        vec3 tangentSample = vec3(sin(theta) * cos(phi), sin(theta) * sin(phi), cos(theta));
        vec3 sampleVec = tangentSample.x * right + tangentSample.y * up + tangentSample.z * N;
        irradiance += texture(environmentMap, sampleVec).rgb * cos(theta) * sin(theta);
        nrSamples++;
    }
}
irradiance = PI * irradiance / float(nrSamples);
```

Render to 32x32 cubemap.

### 11.3 Prefiltered Environment

Use importance sampling with GGX distribution. Render to 128x128 with 5 mip levels. Each mip level uses a different roughness (`roughness = mip_level / max_mip_level`).

### 11.4 BRDF LUT

Generate offline or at startup using a compute shader or fullscreen quad. The shader computes the split-sum integral:
```glsl
vec2 integrateBRDF(float NdotV, float roughness) { ... }
```
Output to a 256x256 `R16G16_SFLOAT` or `R32G32_SFLOAT` texture.

**Validation note:** When rendering to cubemap faces, ensure the render pass uses `LOAD_OP_CLEAR` and `STORE_OP_STORE`. After all faces are rendered, transition the cubemap image from `COLOR_ATTACHMENT_OPTIMAL` to `SHADER_READ_ONLY_OPTIMAL` with a pipeline barrier covering all 6 layers and all mip levels.

---

## 12. Scene Graph

For DamagedHelmet, the scene graph is flat (one node with one mesh), but we implement a general hierarchy.

```rust
pub struct SceneNode {
    pub local_transform: Mat4,
    pub children: Vec<usize>, // indices into SceneGraph.nodes
    pub mesh: Option<usize>,  // index into Scene.meshes
}

pub struct SceneGraph {
    pub nodes: Vec<SceneNode>,
    pub roots: Vec<usize>,
}

impl SceneGraph {
    pub fn compute_world_transforms(&self) -> Vec<Mat4> {
        // DFS from roots
    }
}
```

At load time, flatten the scene: for each node with a mesh, bake the world transform into the `GpuMesh`. This avoids per-frame hierarchy traversal.

---

## 13. Light Setup for DamagedHelmet

The DamagedHelmet screenshot shows a studio lighting setup with soft reflections and a bright highlight.

**Analytic light:**
- Direction: `normalize(vec3(-0.5, -1.0, 0.5))` (from upper-left, slightly behind)
- Intensity: `4.0`
- Color: `(1.0, 0.98, 0.95)` (warm white)

**Environment map:**
- Use a studio HDRi (e.g., `studio_small_09` from Poly Haven) or a generated soft gradient cubemap.
- The environment provides the majority of the illumination; the analytic light adds the specular highlight.

**Camera position:**
- Start at `(0.0, 0.0, -3.0)` looking at origin. The helmet is centered at origin with radius ~1.0.

---

## 14. Cleanup Order (Critical)

The existing `Renderer::drop` has a strict order. The refactored version must maintain this:

1. `device_wait_idle`
2. Destroy environment map textures / samplers / views
3. Destroy scene textures (iterate `scene.textures`, call `destroy`)
4. Destroy fallback textures
5. Destroy scene mesh buffers (iterate `scene.meshes`, destroy vb/ib)
6. Destroy material buffer
7. Destroy global uniform buffers
8. Destroy descriptor pool, then descriptor set layouts (both global and material)
9. Destroy fences, semaphores
10. Free command buffers, destroy command pool
11. Destroy pipeline, layout, render pass
12. `cleanup_swapchain`

**Note:** `scene` is owned by `Renderer`, so it drops naturally after the explicit destroys. Ensure that `Scene` does NOT implement `Drop` that destroys Vulkan objects, because `Renderer::drop` handles it. Or, implement `Scene::destroy(&mut self, device)` and call it from `Renderer::drop` before the pipeline/swapchain cleanup.

---

## 15. Validation Safety Checklist

- [ ] All `vk::ImageMemoryBarrier` use correct `srcAccessMask`/`dstAccessMask` and `srcStageMask`/`dstStageMask`.
- [ ] Image layout transitions from `UNDEFINED` use `TOP_OF_PIPE` / `empty()` access.
- [ ] Cubemap image has `CUBE_COMPATIBLE` flag.
- [ ] Cubemap view uses `view_type = CUBE`, `layer_count = 6`.
- [ ] Descriptor pool sizes account for all allocated sets.
- [ ] Descriptor set writes match the layout bindings exactly.
- [ ] Push constant ranges in pipeline layout match `cmd_push_constants` calls.
- [ ] `ManuallyDrop` ordering in `App` is preserved.
- [ ] No descriptor set is updated while it may be in use by the GPU. Since we update UBOs after fence wait, this is safe.
- [ ] Texture samplers use `max_anisotropy` only if the physical device supports it (check `samplerAnisotropy` feature). If not, set `anisotropy_enable = false`.
- [ ] When creating the logical device, enable `samplerAnisotropy` if desired.
- [ ] Check that `R32G32B32A32_SFLOAT` is supported for the HDR environment map (it is required by Vulkan 1.0).
- [ ] For the BRDF LUT and irradiance maps, ensure format supports `COLOR_ATTACHMENT` if rendering to them.

---

## 16. Build & Asset Instructions

### 16.1 Shader Compilation

Add to `shaders/compile.bat`:
```bat
glslc pbr.vert -o pbr.vert.spv
glslc pbr.frag -o pbr.frag.spv
glslc equirect_to_cube.vert -o equirect_to_cube.vert.spv
glslc equirect_to_cube.frag -o equirect_to_cube.frag.spv
glslc irradiance.frag -o irradiance.frag.spv
glslc prefilter.frag -o prefilter.frag.spv
glslc brdf_lut.frag -o brdf_lut.frag.spv
```

### 16.2 Asset Setup

1. Clone or download `DamagedHelmet` from https://github.com/KhronosGroup/glTF-Sample-Models/tree/main/2.0/DamagedHelmet
2. Place `DamagedHelmet.gltf` and `DamagedHelmet.bin` in `assets/models/DamagedHelmet/`.
3. (Optional) Download a studio HDRi to `assets/environment/studio.hdr`.

### 16.3 Cargo.toml

```toml
[dependencies]
ash = "0.38"
ash-window = "0.13"
winit = "0.30"
raw-window-handle = "0.6"
glam = "0.32"
bytemuck = { version = "1", features = ["derive"] }
image = { version = "0.25", default-features = false, features = ["png", "jpeg"] }
gltf = { version = "1.4", features = ["import", "utils"] }
```

Add `jpeg` feature to `image` for glTF JPEG textures.

---

## 17. Implementation Phases

### Phase 1: Foundation
1. Add `gltf` dependency.
2. Create `src/scene/` module with `PbrVertex`, `PbrMaterial`, `GpuMesh`, `SceneGraph`.
3. Implement glTF loader: parse DamagedHelmet, extract vertices/indices/materials, upload to GPU.
4. Create fallback textures.
5. Remove cube/floor from renderer; replace with scene draw loop.

### Phase 2: PBR Shaders (No IBL)
1. Write `pbr.vert` and `pbr.frag` with analytic directional light only.
2. Update descriptor layout: global UBO + per-material textures.
3. Implement push constants for model matrix.
4. Test: helmet should appear with correct base color, metallic, roughness, normal mapping.

### Phase 3: IBL
1. Current implementation: synthetic LDR 2D environment map with simplified diffuse/specular sampling and roughness-aware ambient Fresnel.
2. Future full IBL: load HDR environment map.
3. Future full IBL: implement equirectangular-to-cubemap render pass.
4. Future full IBL: implement irradiance convolution.
5. Future full IBL: implement prefiltered radiance.
6. Future full IBL: add BRDF LUT.
7. Future full IBL: replace simplified `pbr.frag` IBL block with split-sum IBL.

### Phase 4: Polish
1. Tune light direction/intensity to match screenshot.
2. Add tone mapping.
3. Fix any validation warnings.
4. Performance check: ensure 60 FPS.

---

## 18. Risk Mitigation

| Risk | Mitigation |
|------|------------|
| glTF tangents missing | Compute fallback tangents; DamagedHelmet has them. |
| Validation errors from IBL generation | Use `with_one_time_command` for each face, insert barriers between render passes. |
| HDR image loading failure | Use `image` crate with `hdr` feature; fallback to synthetic gradient cubemap. |
| Too many textures for descriptor pool | Size pool generously; DamagedHelmet has < 10. |
| Coordinate system confusion | Write unit test for RH->LH matrix conversion; verify normal direction visually. |
| Push constant size exceeded | `PushConstants` is 80 bytes < 128 bytes guaranteed. |

---

## 19. Summary of Key Design Decisions

1. **glTF crate:** `gltf` with `import` feature for automatic buffer/image resolution.
2. **Coordinate conversion:** Negate Z in positions and transforms; flip tangent.w; use `COUNTER_CLOCKWISE` front face. The vertex Z-negate and the negative-height viewport are both improper (orientation-reversing) transforms — they cancel, producing CCW winding in framebuffer space, where Vulkan evaluates the front-face test (Vulkan 1.3 §28.4). Full derivation: `docs/winding_orientation.md`.
3. **Descriptor strategy:** Two sets — set 0 per-frame global UBO, set 1 per-material textures. No descriptor indexing required.
4. **Per-draw data:** Push constants for model matrix + material index.
5. **Material data:** Device-local uniform buffer with `GpuMaterial` array, indexed by push constant.
6. **IBL:** Load pre-filtered Ennis KTX2 cubemaps from `assets/environment_map/ennis/` (see `correct_pbr_plan.md` for the path layout). BRDF LUT is generated procedurally on the GPU at startup. This replaced the synthetic LDR 2D placeholder and avoided the runtime equirect-to-cube / convolution pipeline.
7. **Cleanup:** Explicit destroy order in `Renderer::drop`, `Scene::destroy` called first.
8. **No animation:** Static mesh only; scene graph flattened at load time.
