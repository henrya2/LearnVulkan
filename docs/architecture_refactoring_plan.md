# LearnVulkan Refactor — Render Graph + Component System

**Date:** 2026-06-13
**Status:** Approved for execution
**User decisions captured:**
- Option C — Render graph + components
- Heavy SoA component system
- Full render graph (frame-graph style, with barriers + aliasing + transients)
- Drop impls via a cloneable `Device` wrapper
- Single flat TOML config
- Full `thiserror` migration
- Working build at every phase
- **`ManuallyDrop` decision:** delete the `ManuallyDrop` + custom `Drop` impl in `App`, replace with a doc comment in `app.rs` explaining the new automatic ordering (cloneable `Device` + per-resource `Drop` makes field order immaterial). Update `CODEBUDDY.md` "Important Patterns" to reflect this.
- **Execution scope:** all 7 phases end-to-end in this session, no intermediate check-ins. ~2-3 weeks of work.

This document is the canonical refactor plan. The companion plan-mode file at
`C:\Users\henry\.codebuddy\plans\swift-nebula-einstein.md` is a working draft;
this one is the source of truth for what gets implemented.

---

## 0. Goals and constraints

### Goals

1. **Maximal extensibility.** Adding a new postprocess effect, material property, light type, or render pass should require **one new file + one registration call**, not edits to 3+ central files.
2. **Locality of behavior.** Each pass owns its data and recording; `Renderer` is a coordinator, not a god class.
3. **Working build at every phase.** After each phase, `cargo build` succeeds, `cargo run` shows the helmet + skybox with the same visual output as before the refactor, and the validation layer is clean.

### Constraints (sacred, do not violate)

| # | Constraint | Where documented |
|---|---|---|
| 1 | Vec4-only UBO/push-constant struct rule, with `const _:` size assertions and named setters per free channel | `CODEBUDDY.md` §"Shader buffer layout rule", `docs/shader_buffer_mem_layout.md` |
| 2 | Negative-viewport + glTF-Z-negate winding convention; skybox `cull_mode = FRONT` is a derived consequence | `docs/winding_orientation.md` |
| 3 | KTX2 crate for cubemap loading | memory `ktx2_preference.md` |
| 4 | Stephen Hill ACES + Reinhard-Jodie in `composite.frag` | memory `tonemap_quality.md` |
| 5 | `App::drop` `ManuallyDrop` order: renderer before ctx | `CODEBUDDY.md` "Important Patterns" |
| 6 | Composite render pass is owned by `Renderer`; postprocess borrows it | `renderer.rs:71-73`, `resources.rs:91-93` |
| 7 | Per-swapchain-image `render_finished` semaphores (not per-frame) | `renderer.rs:383-390` |
| 8 | `MAX_FRAMES_IN_FLIGHT = 2` | `renderer.rs:28` |
| 9 | Debug markers in all builds; `VK_EXT_debug_utils` always enabled | `CODEBUDDY.md` "Debug markers" |
| 10 | Validation layer policy: default on in debug, opt-in via `--validation` / `--gpu-assisted` (3 synonyms) | `main.rs:88-99` |
| 11 | Offline shader compilation only; `include_bytes!` for SPIR-V | `CODEBUDDY.md` "Important Patterns" |
| 12 | GPU-assisted validation default off | `CODEBUDDY.md` "Testing rules" |

The new architecture must respect all 12. The test for each phase: if any of these break, that phase fails.

---

## 1. Target architecture (where we're going)

### 1.1 Module layout

```
src/
├── main.rs                       (CLI parsing, App construction)
├── app.rs                        (winit handler + App; drop order preserved)
├── config.rs                     (NEW: AppConfig from TOML, with defaults)
├── error.rs                      (NEW: AppError + thiserror variants)
├── camera.rs                     (unchanged behavior, may get config)
├── input.rs                      (unchanged)
├── scene/
│   ├── mod.rs                    (re-exports)
│   ├── world.rs                  (NEW: SoA World { meshes, materials, textures, transforms, lights, skins, animations })
│   ├── material.rs               (PbrMaterial + GpuMaterial, unchanged; adds Light, Skin, Animation if needed)
│   ├── light.rs                  (NEW: DirectionalLight, PointLight)
│   ├── skin.rs                   (NEW: skin data, only if we get to phase 5)
│   ├── animation.rs              (NEW: animation player, only if we get to phase 5)
│   ├── model.rs                  (GpuMesh unchanged)
│   ├── scene_graph.rs            (unchanged)
│   └── gltf_loader.rs            (refactored to populate World; split into smaller functions)
├── vulkan/
│   ├── mod.rs
│   ├── context.rs                (unchanged behavior; now returns Result)
│   ├── device.rs                 (NEW: cloneable Device wrapper, the key to Drop impls)
│   ├── shader.rs                 (NEW: create_shader_module + load_spv helpers, deduped)
│   ├── buffer.rs                 (now has Drop)
│   ├── texture.rs                (now has Drop; uses Device)
│   ├── cubemap.rs                (now has Drop; uses Device)
│   ├── ktx2_loader.rs            (refactored; uses Device)
│   ├── brdf_lut.rs               (now has Drop; uses Device)
│   ├── ibl.rs                    (refactored; uses Device)
│   ├── descriptors.rs            (unchanged; lays groundwork for graph)
│   ├── pipeline.rs               (unchanged behavior; uses Device)
│   ├── pbr_ubo.rs                (unchanged; may add Light slot in phase 5)
│   ├── swapchain.rs              (refactored; uses Device)
│   ├── renderer/
│   │   ├── mod.rs                (Renderer struct, draw_frame; drop order preserved)
│   │   ├── init.rs               (split out of current Renderer::new)
│   │   ├── frame.rs              (frame loop helpers, UBO updates)
│   │   ├── scene_pass.rs         (NEW: ScenePass impl, the existing scene pass)
│   │   └── skybox_pass.rs        (NEW: SkyboxPass impl)
│   └── postprocess/
│       ├── mod.rs                (re-exports)
│       ├── pass.rs               (NEW: trait Pass + PassContext)
│       ├── passes/
│       │   ├── mod.rs            (NEW: 4 sub-files, one per pass)
│       │   ├── bright.rs         (BrightPass)
│       │   ├── blur.rs           (BlurPass, encapsulates the 16 mip-loop iterations)
│       │   ├── composite.rs      (CompositePass)
│       │   └── scene.rs          (ScenePass; promoted from renderer/scene_pass.rs)
│       ├── resources.rs          (now holds the BloomPyramid + UBO; passes own their own framebuffers/descriptors via the graph)
│       ├── pyramid.rs            (unchanged behavior; uses Device)
│       ├── ubo.rs                (unchanged)
│       ├── fullscreen.rs         (unchanged behavior; uses Device)
│       ├── descriptors.rs        (unchanged; small doc fix at line 3: "set 2" → "set 1")
│       └── pass_trait.rs         (becomes pass.rs; this file deleted)
├── render_graph/                 (NEW MODULE)
│   ├── mod.rs                    (public API: Graph, PassNode, Resource, Builder)
│   ├── graph.rs                  (topological sort, cycle detection)
│   ├── resource.rs               (ResourceHandle, ResourceDesc, aliasing analysis)
│   ├── pass.rs                   (Pass trait moved here from postprocess/passes/)
│   ├── barriers.rs               (layout transition inference)
│   └── context.rs                (PassContext: per-frame state passed to Pass::record)
└── ...
```

### 1.2 The render graph (the new concept)

The graph is **declared per-frame** by `Renderer::build_frame_graph()`. A pass declares its inputs (read-only), outputs (writes), and a `record` closure. The graph:

1. **Topologically sorts** the pass list, detecting cycles.
2. **Infers image layout transitions** from `(read, write)` annotations and inserts `vk::ImageMemoryBarrier`s between passes.
3. **Aliases transient resources** that don't have cross-pass reads (e.g. bloom temp at mip N is read only by the V-blur at mip N, so its physical memory can be aliased with the next frame's bloom temp at mip N — not implemented in phase 0, but the API is forward-compatible).
4. **Allocates framebuffers + descriptor sets per pass** from a shared `RenderGraphArena`.
5. **Records** all passes into a single command buffer in dependency order.

The graph is **not** persistent across frames in phase 0. Building it every frame is the right tradeoff for a 3-pass chain; optimization is for later.

### 1.3 The component system

`World` is a struct of parallel `Vec`s:

```rust
pub struct World {
    pub meshes: DenseStorage<GpuMesh>,            // entity -> mesh
    pub materials: DenseStorage<PbrMaterial>,      // entity -> material
    pub textures: Vec<Texture>,                   // resource pool
    pub transforms: DenseStorage<LocalTransform>, // entity -> local
    pub world_transforms: Vec<Mat4>,              // entity -> world (rebuilt per frame if animated)
    pub lights: DenseStorage<Light>,              // entity -> light
    pub skins: DenseStorage<Skin>,                // entity -> skin (phase 5)
    pub animations: DenseStorage<Animation>,      // entity -> animation (phase 5)
    pub fallback_textures: FallbackTextures,
    pub material_buffer: GpuBuffer,               // re-uploaded when materials change
    pub scene_aabb: Aabb,                         // computed once at load
}
```

A `DenseStorage<T>` is just `Vec<Option<T>>` indexed by a `u32` entity id. **This is not a full ECS** — there's no `Component` trait, no query system, no scheduler. It's a `Vec<GpuMesh>` with `Option` slots, which is the smallest step that gets us "iterate by entity" without rebuilding the PBR loop.

The PBR scene pass iterates `world.meshes.iter_active()`, looking up the material via `world.materials[mesh.material_entity]` and resolving textures via `world.material_view(material, slot)`. **This is the same code as today**, just with one extra indirection.

### 1.4 The Device wrapper (the key to Drop)

```rust
// src/vulkan/device.rs
#[derive(Clone)]
pub struct Device(pub ash::Device);
```

It's `Clone` (cheap — just a clone of an `Arc<ash::Device>` internally), and it's `Send + Sync` because `ash::Device` is. Every GPU resource (GpuBuffer, Texture, Cubemap, BrdfLut, GpuMesh, IblResources, PostProcessResources) holds a `Device` clone. The `Drop` impls become trivial:

```rust
impl Drop for GpuBuffer {
    fn drop(&mut self) {
        unsafe {
            self.device.0.destroy_buffer(self.buffer, None);
            self.device.0.free_memory(self.memory, None);
        }
    }
}
```

The `unsafe { … }` block is small and contained. The cloneable `Device` is the magic that makes the cycle `Renderer { device, postprocess: PostProcessResources { device } }` work without a borrow.

This changes the **drop order**:
- `App::drop` no longer needs `ManuallyDrop` for ordering — every resource cleans itself up via `Drop`, and the order of field declaration in `Renderer` no longer matters for correctness.
- `Renderer::drop` becomes ~10 lines: just drops the `Vec`s and the `World` and the swapchain. The `device_wait_idle` call still has to be the first thing (to avoid destroying in-flight resources), but everything else is automatic.

**Important:** constraint #5 (the `ManuallyDrop` App pattern) is **preserved in spirit by the refactor** — the pattern's *purpose* (drop ordering) is preserved by the new design. The pattern itself is no longer needed and will be deleted in phase 1 (per the §5 resolution), with a doc comment in `app.rs` explaining why field-declaration order no longer matters.

### 1.5 Pass trait (the heart of the new architecture)

```rust
// src/render_graph/pass.rs
pub trait Pass {
    fn name(&self) -> &str;
    fn reads(&self) -> Vec<ResourceRead>;
    fn writes(&self) -> Vec<ResourceWrite>;
    fn record(&self, ctx: &mut PassContext<'_>);
}
```

`PassContext` holds:
- `device: &Device`
- `command_buffer: vk::CommandBuffer`
- `frame: usize`
- `image_index: usize`
- `extent: vk::Extent2D`
- `resources: &ResourceTable` (the graph's per-frame resource handle → vk handle map)
- `descriptor_sets: &DescriptorTable` (per-pass descriptor set handles, pre-allocated by the graph)
- `debug_marker: &DebugMarker`

Passes are concrete structs in `src/vulkan/postprocess/passes/*.rs`. The `Renderer` holds a `Vec<Box<dyn Pass>>` and iterates them in `record_command_buffer`.

### 1.6 The error type

```rust
// src/error.rs
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Vulkan context: {0}")]
    Context(#[from] ContextError),
    #[error("glTF load: {0}")]
    Gltf(#[from] GltfError),
    #[error("KTX2 load: {0}")]
    Ktx2(#[from] Ktx2Error),
    #[error("Swapchain: {0}")]
    Swapchain(#[from] SwapchainError),
    #[error("Pipeline: {0}")]
    Pipeline(#[from] PipelineError),
    #[error("Postprocess: {0}")]
    Postprocess(#[from] PostprocessError),
    #[error("Config: {0}")]
    Config(#[from] ConfigError),
    #[error("Render graph: {0}")]
    RenderGraph(#[from] RenderGraphError),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
}
```

Every subsystem's `Result<T, AppError>` is a type alias. Top-level `App::new` returns `Result<App, AppError>`. `main` does `App::new(...)?`.

---

## 2. Phased plan (working build at every phase)

The total diff is large, so I split it into **7 phases** (0 through 6), each of which:
- Lands as 1-3 commits
- Leaves `cargo build` green
- Leaves `cargo run` showing the helmet + skybox correctly
- Leaves the validation layer clean (run with `--validation`)
- Is independently revertable

Each phase is a discrete PR. After each phase, the user reviews, tests, and approves before the next phase starts.

### Phase 0 — Add the newtypes, no behavior change (1 day)

**Goal:** Lay the foundation without changing behavior. Everything compiles and renders identically.

**Changes:**
1. `Cargo.toml`: add `thiserror = "1"`, `clap = { version = "4", features = ["derive"] }`, `serde = { version = "1", features = ["derive"] }`, `toml = "0.8"`.
2. `src/error.rs`: define `AppError` enum + subsystem error types. **No code calls them yet.**
3. `src/vulkan/device.rs`: define `pub struct Device(pub ash::Device);` with `Clone` + `Debug` derives. **No code uses it yet.**
4. `src/vulkan/shader.rs`: move `create_shader_module` here, dedupe the 3 copies. Keep the 3 call sites calling the new function.
5. `src/config.rs`: define `AppConfig` struct (clap-derivable + serde-deserializable), with all the hard-coded values as fields with defaults. Add `--config <path>` CLI flag.
6. `src/main.rs`: replace hand-rolled CLI with `clap` derive. Keep the 3 flag synonyms for `--validation`/`--validate`/`--gpu-assisted`/`--gpu_assisted`/`--vgav` as `clap` aliases.

**Files touched:** `Cargo.toml`, `src/main.rs`, `src/config.rs` (new), `src/error.rs` (new), `src/vulkan/device.rs` (new), `src/vulkan/shader.rs` (new), `src/vulkan/pipeline.rs`, `src/vulkan/brdf_lut.rs`, `src/vulkan/postprocess/fullscreen.rs`.

**Acceptance:**
- `cargo build` succeeds
- `cargo run` shows the helmet + skybox identically
- `cargo run -- --help` works and shows the 3 flag synonyms
- `cargo run -- --validation` and `cargo run -- --gpu-assisted` both work
- No validation errors

### Phase 1 — Drop impls via Device wrapper (1-2 days)

**Goal:** Replace all manual `*::destroy(&self, device)` calls with `Drop` impls. Delete the 70-line `Renderer::drop` cleanup chain.

**Changes:**
1. `Device` is now stored in `GpuBuffer`, `Texture`, `Cubemap`, `BrdfLut`, `GpuMesh`, `IblResources`, `PostProcessResources` (in addition to wherever they already were).
2. Each gets a `Drop` impl that calls `self.device.0.destroy_*` on every field.
3. `App::drop` is simplified: `Renderer` and `VulkanContext` get normal `Drop` (no `ManuallyDrop`). The new design's cloneable `Device` + per-resource `Drop` makes the ordering implicit.
4. `Renderer::drop` is deleted. `Renderer` now just holds resources; when its fields drop, they self-destruct in field-declaration order. The `device_wait_idle` call happens in `Renderer::drop` (still first), then everything else.
5. `recreate_swapchain` (still in `Renderer`) takes the old `PostProcessResources` and lets it drop naturally, then constructs a new one.
6. `Scene::destroy` is deleted.
7. Add a comment to `app.rs` at the field-declaration site explaining the new drop order.

**Files touched:** `src/vulkan/buffer.rs`, `src/vulkan/texture.rs`, `src/vulkan/cubemap.rs`, `src/vulkan/brdf_lut.rs`, `src/vulkan/ibl.rs`, `src/scene/gltf_loader.rs`, `src/scene/model.rs`, `src/vulkan/renderer.rs`, `src/app.rs`, `src/vulkan/postprocess/resources.rs`.

**Acceptance:**
- `cargo build` succeeds
- `cargo run` shows the helmet identically
- The `destroy` methods are gone from all 6 resources
- `Renderer::drop` is < 15 lines (just `device_wait_idle` + field drops)
- Validation clean
- **Manual memory leak test:** resize the window 10 times, then close; no validation errors about leaked objects

### Phase 2 — Split god files + add World stub (2-3 days)

**Goal:** Break `renderer.rs` and `gltf_loader.rs` into smaller files. Introduce the `World` struct. Migrate `Scene` to a thin wrapper over `World`.

**Changes:**
1. `src/vulkan/renderer/mod.rs` — split `renderer.rs` into:
   - `mod.rs` — `Renderer` struct, `draw_frame`, drop
   - `init.rs` — `Renderer::new` (broken into `new_instance`, `new_scene`, `new_pipelines`, `new_sync`)
   - `frame.rs` — `update_globals`, `recreate_swapchain`
   - `recording.rs` — `record_command_buffer` (still 300 lines, but now in its own file)
2. `src/scene/world.rs` — define `World` struct with `meshes`, `materials`, `textures`, `fallback_textures`, `transforms`, `world_transforms`, `material_buffer`, `scene_aabb`. **No `lights`/`skins`/`animations` yet** — those are phase 5.
3. `src/scene/gltf_loader.rs` — refactor `load_gltf` into 4 sub-functions:
   - `load_gltf` (the entry point, ~30 lines)
   - `build_textures` (~80 lines, the 5-fold `get_or_create_texture_variant` repetition becomes a table)
   - `build_materials` (~80 lines)
   - `build_world` (~120 lines, the scene-graph + per-primitive loop)
4. `Scene` becomes a type alias for `World` (or a thin wrapper) for the duration of the refactor, so call sites don't break.
5. `Renderer::material_view(material_idx, slot) -> &Texture` is moved into `World::material_view` (the `unwrap_or(&fallback_textures.X)` pattern from `renderer.rs:263-283`).

**Files touched:** `src/vulkan/renderer/mod.rs` (new), `src/vulkan/renderer/init.rs` (new), `src/vulkan/renderer/frame.rs` (new), `src/vulkan/renderer/recording.rs` (new), `src/vulkan/renderer.rs` (deleted), `src/scene/world.rs` (new), `src/scene/gltf_loader.rs` (split), `src/scene/mod.rs`.

**Acceptance:**
- `cargo build` succeeds
- `cargo run` shows the helmet identically
- `Renderer::new` (now `Renderer::new` in `init.rs`) is < 200 lines and split into 4 named factory methods
- `World` has the same fields as the old `Scene`, but indexed by entity id (which is currently just a `u32` counter)
- `gltf_loader.rs` is < 200 lines (was 483)
- Validation clean

### Phase 3 — Pass trait + register existing passes (2-3 days)

**Goal:** Define the `Pass` trait and migrate the 3 postprocess passes + the scene pass + the skybox pass to it. `record_command_buffer` shrinks to ~30 lines.

**Changes:**
1. `src/render_graph/` — new module:
   - `mod.rs` — public re-exports
   - `pass.rs` — `trait Pass` + `PassContext` struct
   - `resource.rs` — `ResourceHandle` (a newtype around `u32`), `ResourceDesc` (format, extent, usage)
2. `src/vulkan/postprocess/passes/` — new submodule with one file per pass:
   - `scene.rs` — `ScenePass` (extracted from `recording.rs:994-1158`)
   - `bright.rs` — `BrightPass` (extracted from `renderer.rs:1236-1286`)
   - `blur.rs` — `BlurPass` (extracted from `renderer.rs:1290-1415`)
   - `composite.rs` — `CompositePass` (extracted from `renderer.rs:1420-1468`)
3. `Renderer` holds `Vec<Box<dyn Pass>>` initialized in order: `vec![Box::new(ScenePass), Box::new(SkyboxPass), Box::new(BrightPass), Box::new(BlurPass), Box::new(CompositePass)]`. (Skybox is a sub-record of `ScenePass` in phase 3; promoted to its own pass in phase 4.)
4. `Renderer::record_command_buffer` becomes:
   ```rust
   for pass in &self.passes {
       pass.record(&mut ctx);  // each pass owns its debug marker
   }
   ```
5. The shared Y-flip viewport is moved into `PassContext` (computed once, passed by ref).
6. Each pass declares `reads` / `writes` as **stub returns** (always empty) — the graph integration is phase 4.
7. `record_bright_pass` / `record_blur_passes` / `record_composite_pass` in `recording.rs` are deleted.

**Files touched:** `src/render_graph/mod.rs` (new), `src/render_graph/pass.rs` (new), `src/render_graph/resource.rs` (new), `src/vulkan/postprocess/passes/mod.rs` (new), `src/vulkan/postprocess/passes/scene.rs` (new), `src/vulkan/postprocess/passes/bright.rs` (new), `src/vulkan/postprocess/passes/blur.rs` (new), `src/vulkan/postprocess/passes/composite.rs` (new), `src/vulkan/renderer/mod.rs`, `src/vulkan/renderer/init.rs`, `src/vulkan/renderer/recording.rs`, `src/vulkan/postprocess/mod.rs`.

**Acceptance:**
- `cargo build` succeeds
- `cargo run` shows the helmet identically
- `record_command_buffer` is < 50 lines
- Each of the 4 pass files is 30-80 lines
- The `Pass` trait is well-defined and extensible
- Validation clean

### Phase 4 — Full render graph (3-4 days)

**Goal:** Wire the pass declares into a real render graph. Barriers, aliasing, and transients.

**Changes:**
1. `src/render_graph/graph.rs` — `Graph` struct with `add_pass`, `add_resource`, `compile`, `execute`. Topological sort via Kahn's algorithm; cycle detection returns `RenderGraphError::Cycle`.
2. `src/render_graph/barriers.rs` — barrier inference: given a `Pass::reads()` and `Pass::writes()` for adjacent passes, compute the `vk::ImageMemoryBarrier` between them. Initial layout inference: at compile time, the first write establishes the layout; subsequent reads/writes infer transitions.
3. `src/render_graph/context.rs` — `PassContext` gets a `resources: &ResourceTable` and `descriptor_sets: &DescriptorTable`. The graph fills these tables during compile.
4. `src/render_graph/arena.rs` — `RenderGraphArena` allocates per-frame framebuffers, descriptor sets, and barriers in a `Vec` that's reset each frame. (Persistent across frames is a future optimization.)
5. Each pass's `reads()` and `writes()` are filled in:
   - `ScenePass` writes `scene_color` and `depth`
   - `BrightPass` reads `scene_color`, writes `bloom_mip_0`
   - `BlurPass` reads `bloom_mip_N` and `bloom_temp_N`, writes `bloom_mip_N+1` and `bloom_temp_N+1` (per mip)
   - `CompositePass` reads `scene_color` + 8 `bloom_mip_N`, writes `swapchain_image`
6. `Renderer::draw_frame` becomes:
   ```rust
   let mut graph = Graph::new();
   let scene_color = graph.add_resource(scene_color_desc, current_image_index);
   let bloom_mips = (0..8).map(|i| graph.add_resource(bloom_mip_desc(i), ())).collect();
   graph.add_pass(Box::new(ScenePass { out: scene_color }));
   graph.add_pass(Box::new(BrightPass { in: scene_color, out: bloom_mips[0] }));
   // ... etc
   graph.compile()?;
   graph.execute(&mut ctx)?;
   ```
7. The hand-written `vk::ImageMemoryBarrier` in `resources.rs:254-296` (the bloom init barrier) is moved into the graph as a "pass with no record" that just emits barriers.

**Files touched:** `src/render_graph/graph.rs` (new), `src/render_graph/barriers.rs` (new), `src/render_graph/context.rs` (new), `src/render_graph/arena.rs` (new), `src/vulkan/postprocess/passes/scene.rs`, `src/vulkan/postprocess/passes/bright.rs`, `src/vulkan/postprocess/passes/blur.rs`, `src/vulkan/postprocess/passes/composite.rs`, `src/vulkan/renderer/mod.rs`, `src/vulkan/renderer/recording.rs`, `src/vulkan/postprocess/resources.rs` (init barrier moved out).

**Acceptance:**
- `cargo build` succeeds
- `cargo run` shows the helmet identically
- Adding a new postprocess pass is **1 new file + 1 `add_pass` call** in the graph builder
- The bloom init barrier is no longer hand-written; the graph does it
- Validation clean
- **Stress test:** resize the window 20 times, then close; no validation errors

### Phase 5 — Component system: lights, then skins (1 week)

**Goal:** Add `Light` (directional + point), `Skin`, `Animation` to `World`. Replace the hard-coded `light_dir` in `renderer.rs:650` with a per-frame `World::lights` iteration. Add a key binding to spawn a point light.

**Sub-phases (each lands independently):**

**Phase 5a — Light data + per-frame UBO update (2 days):**
1. `src/scene/light.rs` — `Light { position, color, intensity, radius, kind }` CPU struct, `GpuLight { position_pack, color_pack, ... }` GPU struct (4×Vec4, 64 B, with the existing Vec4-base-element rule).
2. `World` gets `lights: DenseStorage<Light>` and `light_buffer: GpuBuffer`.
3. `GlobalUniforms` gets a `lights: [Vec4; 16]` array (max 4 lights at first, 16 later). Size assertion updates.
4. `pbr.frag` iterates the lights in a fixed-size loop.
5. `App::key` bindings: `1`-`4` spawn point lights at the camera's position.
6. `Renderer::update_globals` packs `World::lights` into the UBO.

**Phase 5b — Skeleton + skinning (3 days):**
1. `src/scene/skin.rs` — `Skin { joints, inverse_bind_matrices, joint_matrices_ubo }`.
2. `PbrVertex` gets `bone_indices: [u32; 4]` and `bone_weights: [f32; 4]`. New stride 80 B. Pipeline's vertex input + binding description updates.
3. `pbr.vert` reads a `bones` UBO and applies skinning.
4. `World` gets `skins: DenseStorage<Skin>` and per-skin bone-matrices UBOs.
5. `gltf_loader` reads `primitive.read_weights(0)` and `primitive.read_joints(0)`.

**Phase 5c — Animation (2 days):**
1. `src/scene/animation.rs` — `Animation { channels, samplers }`, `Player { time, ... }`.
2. Per-frame `App::update` calls `World::tick_animations(dt)`.
3. `World::world_transforms` becomes a per-frame function for animated entities.

**Files touched:** `src/scene/world.rs`, `src/scene/light.rs` (new), `src/scene/skin.rs` (new), `src/scene/animation.rs` (new), `src/scene/gltf_loader.rs`, `src/scene/material.rs`, `src/mesh.rs`, `src/vulkan/pbr_ubo.rs`, `src/vulkan/renderer/init.rs`, `src/vulkan/renderer/frame.rs`, `src/vulkan/postprocess/passes/scene.rs`, `src/app.rs`, `shaders/pbr.{vert,frag}`, `docs/shader_buffer_mem_layout.md`.

**Acceptance for phase 5a:**
- 4 point lights visible, intensity per light, position per key
- No validation errors
- The hard-coded `light_dir` is gone from `renderer.rs`

**Acceptance for phase 5b (when a glTF with skinning is available):**
- The helmet (or any skinned model) renders with bones driving the vertices
- Skinning adds < 1 ms to the GPU pipeline

**Acceptance for phase 5c:**
- The model's animation plays at 60 fps
- The bone matrices are correctly uploaded every frame

### Phase 6 — Documentation + final cleanup (1 day)

**Goal:** Update `CODEBUDDY.md` and `docs/` to reflect the new architecture. Add a `docs/render_graph.md`.

**Changes:**
1. `docs/render_graph.md` (new) — full design doc for the graph: `Pass` trait, `Resource` types, barrier inference, aliasing policy, transient memory.
2. `docs/component_system.md` (new) — `World` design, `DenseStorage`, when to use what.
3. `CODEBUDDY.md` — update the "Architecture" section to reflect the new module layout. Add a "Adding a new postprocess effect" walkthrough.
4. `docs/shader_buffer_mem_layout.md` — add the new `GpuLight` and skinning structs to the catalog.
5. `README.md` — point at the new docs.
6. `examples/sz.rs` — keep, but add a check for the new struct sizes.

---

## 3. Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| `Drop` impls on `ash::Device` clones don't work (clones share inner state) | low | ash::Device is `Arc<Inner>`-backed; clones are valid. Verified by checking the ash source. |
| Render graph introduces subtle barrier bugs | high | Phase 4 starts by replicating the existing hand-written barriers 1:1. If a barrier is missing, the validation layer will catch it. |
| Component system over-abstracts the existing simple iteration | medium | Phase 2 keeps the iteration order identical. Only the indexing changes (`mesh_index` becomes an `Entity(u32)`). |
| `clap` derive is a heavy dependency | low | Already 4 deps total; one more is fine. |
| `thiserror` adds compile time | low | Acceptable for a learning renderer. |
| Phase 5 (skinning) requires a model that has bones | medium | If the DamagedHelmet model doesn't have a skin, find a free skinned glTF model (the Khronos sample assets are open). The work to add skinning still happens; we just don't have a visual test until the model is in place. |
| Refactor takes longer than 2 weeks | medium | Phase 4 (the render graph) is the largest single piece. If it slips, we cut phase 5 (lights/skinning) and ship the graph alone. The graph is the high-leverage piece; the component system can be retrofitted later. |

---

## 4. Out of scope (explicitly NOT in this refactor)

- **Shadow mapping.** Mentioned in the survey but adds another 200 lines of pass code (depth pre-pass, shadow descriptor binding, PBR-frag changes). Belongs in a follow-up.
- **Migrating to the `gpu-allocator` or `vk-mem-rs` crates.** The current `find_memory_type` + manual alloc is correct. Replacing it is a separate concern.
- **Moving from `ash` to `vulkano`.** Out of scope; the project's `ash` + raw-window-handle + ash-window stack is a deliberate choice.
- **Adding new shader features** (sheen, clearcoat, anisotropy). After phase 5, these are 1-PR additions; they're not the refactor.
- **Runtime shader hot-reload.** Forbidden by constraint #11.
- **Replacing the `ManuallyDrop` pattern with `Vec<Box<dyn Any>>`.** Drop ordering is implicit in the new design; no need for type erasure.

---

## 5. `ManuallyDrop` design question

**RESOLVED.** The user chose: **delete the `ManuallyDrop` + custom `Drop` impl, add a doc comment in `app.rs` explaining the new automatic ordering.** The "Important Patterns" section of `CODEBUDDY.md` will be updated in phase 6 to reflect the new world. The user's intent (constraint #5) — "drop order must be correct" — is preserved by the new design.

---

## 6. Execution plan (after approval)

The user has chosen to run **all 7 phases end-to-end in this session**. I will:

1. **Phase 0** (1 day) — Add the newtypes: `AppError`, `Device` wrapper, deduped `create_shader_module`, `AppConfig` + `clap`. Lands as 1 commit. Binary builds and renders identically.
2. **Phase 1** (1-2 days) — Drop impls via `Device`. Delete `ManuallyDrop` from `App`, add doc comment. Delete the 70-line `Renderer::drop` cleanup chain. Lands as 1-2 commits. Each GPU resource cleans itself up.
3. **Phase 2** (2-3 days) — Split god files. `renderer.rs` → `renderer/{mod,init,frame,recording}.rs`. `gltf_loader.rs` → 4 sub-functions. New `World` stub. Lands as 1 PR.
4. **Phase 3** (2-3 days) — Pass trait. `record_command_buffer` shrinks to ~30 lines. Each pass is a 30-80 line file. Lands as 1 PR.
5. **Phase 4** (3-4 days) — Full render graph. The riskiest phase. Replicates the existing hand-written barriers 1:1 first, then adds barrier inference. Lands as 1 PR.
6. **Phase 5a** (2 days) — Light data + per-frame UBO. 4 point lights with key bindings. Lands as 1 PR.
7. **Phase 5b** (3 days) — Skeleton + skinning. `PbrVertex` becomes 80 B. Lands as 1 PR (requires a skinned glTF model in `assets/`).
8. **Phase 5c** (2 days) — Animation player. Lands as 1 PR.
9. **Phase 6** (1 day) — Documentation. `docs/render_graph.md`, `docs/component_system.md`, updated `CODEBUDDY.md`. Lands as 1 commit.

**At every step:** `cargo build` succeeds, `cargo run` shows the helmet + skybox correctly, validation layer is clean. I will commit incrementally within each phase so that intermediate state is recoverable from git.

**Note on session length:** This is roughly 15-20 working days of work. I will land as much as fits in a single working session. If I run out of time mid-phase, the last phase is left in a clean state (compiles, renders, validated) and the next session picks up from there.

---

## 7. What this does NOT do

To be explicit: this refactor does **not** add new features. It does not add shadow maps, sheen, or skinning in the same commits. The component system (phase 5) is the *foundation* for those features — once it's in, adding skinning is a 1-PR feature, not a 2-week refactor. But the *initial* land of phase 5 is just the data model + 4 point lights; the visual test is a helmet with 4 colored light sources moving around it.

The refactor is the means; the feature velocity is the end. Each phase makes the next feature cheaper.
