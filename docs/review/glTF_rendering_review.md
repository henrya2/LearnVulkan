# glTF Rendering and PBR Review

## Status after implementation

The high-impact correctness issues from this review have been addressed in code:

- `Texture::from_rgba8_with_format` now supports explicit Vulkan formats.
- glTF base-color and emissive textures are uploaded as `R8G8B8A8_SRGB`.
- glTF normal, metallic-roughness, and occlusion textures are uploaded as `R8G8B8A8_UNORM`.
- Fallback textures are semantic-specific: white sRGB, white linear, black sRGB, linear flat normal, and linear metallic-roughness white.
- Primitives with no material now resolve to an explicit glTF default material.
- The loader now traverses only the default scene, or scene 0 if no default is declared.
- Missing normals on non-indexed primitives now use synthesized indices for normal generation.
- `pbr.frag` now outputs linear tone-mapped color to the sRGB swapchain attachment, with no manual gamma correction.
- Metallic and roughness are clamped.
- glTF occlusion strength is applied with `mix(1.0, ao, strength)`.
- TBN construction re-orthogonalizes tangent against normal.
- The simplified environment term now uses roughness-aware Fresnel.

The remaining major visual limitation is still IBL quality: the renderer uses one synthetic 2D LDR environment texture as a placeholder. Full HDR split-sum IBL remains future work.

## What changed and why

### Color management

The previous shader manually gamma-corrected after ACES tone mapping while rendering to an sRGB swapchain attachment. That was a double-encoding path. The shader now tone maps and writes linear color; the sRGB attachment performs final encoding on store.

### glTF texture color spaces

glTF distinguishes color textures from data textures:

- base color: sRGB
- emissive: sRGB
- normal: linear UNORM
- metallic-roughness: linear UNORM
- occlusion: linear UNORM

The loader now creates Vulkan texture variants by semantic/format, so data textures are no longer sampled through sRGB decoding.

### Fallback textures

Missing texture slots now bind fallbacks that preserve glTF material semantics:

- base color: white sRGB
- emissive: black sRGB
- occlusion: white linear
- normal: `[128, 128, 255, 255]` linear
- metallic-roughness: `[255, 255, 255, 255]` linear, so roughness and metallic scalar factors remain effective

### glTF material and scene semantics

Primitives without materials no longer use material index 0 by accident. The loader appends a glTF default material and routes omitted material bindings to it.

The loader now uses `document.default_scene()` with scene 0 fallback. This prevents inactive scenes and unreachable nodes from being rendered accidentally.

### Shader math

The shader now:

- clamps metallic and roughness;
- applies occlusion strength with glTF semantics;
- re-orthogonalizes TBN;
- applies ACES tone mapping without manual gamma correction;
- uses roughness-aware Fresnel for the current simplified environment term.

## Remaining IBL work

The current environment map is still a low-resolution synthetic 2D LDR gradient. It is useful as a placeholder ambient/reflection source, but it is not physically based IBL.

A full IBL implementation should add:

1. HDR equirectangular environment loading.
2. Equirectangular-to-cubemap conversion.
3. Diffuse irradiance cubemap convolution.
4. GGX prefiltered specular cubemap generation.
5. BRDF integration LUT generation.
6. Dedicated IBL descriptors, likely a separate descriptor set containing:
   - `samplerCube uIrradianceMap`
   - `samplerCube uPrefilterMap`
   - `sampler2D uBrdfLut`
7. Shader replacement of the current simplified IBL block with split-sum IBL.

## Architecture recommendations still worth doing

These were not implemented in the correctness pass and remain recommended:

1. Split `Renderer` into focused owners: frame resources, swapchain state, pipeline state, descriptor state, scene GPU resources, upload context, and environment/IBL resources.
2. Move material data from the fixed 64-element UBO array to a storage buffer for scalable glTF scenes.
3. Implement glTF sampler-state mapping instead of always using repeat/linear samplers.
4. Support texture coordinate set selection (`TEXCOORD_1`) for slots such as occlusion.
5. Support glTF alpha modes and double-sided materials with separate pipeline/render-pass handling.
6. Fix swapchain/render-pass cleanup ordering and create render passes from the actual selected swapchain format.
7. Batch uploads instead of calling `queue_wait_idle` for every one-time upload.

## Validation performed

- `cargo fmt`
- `cargo check`
- `shaders/compile.bat`
- `cargo build`

All completed successfully. The build still reports existing unused-code warnings for legacy helpers and unused paths.
