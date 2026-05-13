# Triangle Mesh Winding Orientation: glTF → Vulkan End-to-End

This document traces the winding (front/back-face) orientation of DamagedHelmet's
triangles through every stage of the pipeline, with citations to authoritative
sources. The take-away: the project applies **two improper (orientation-reversing)
transforms** — vertex Z-negation at load time and a negative-height viewport at
draw time — and they cancel, so the original glTF CCW-from-outside winding ends
up as CCW in framebuffer space. The pipeline's `frontFace =
VK_FRONT_FACE_COUNTER_CLOCKWISE` then declares those triangles front-facing.

## 1. glTF 2.0 Spec — Source of Truth

Khronos glTF 2.0, §3.7.2 "Meshes":

> The front face of a triangle is defined by the **counterclockwise** order of
> its vertices when projected to the viewing plane, with the X axis pointing
> right and the Y axis pointing up. Implementations **MUST** use the right-hand
> rule to determine the front face.

> The coordinate system is **right-handed**; +Y is up, +Z is toward the viewer,
> +X is right.

For DamagedHelmet (a Khronos sample asset), every primitive's index buffer
encodes triangles whose vertex order is **CCW from outside in RH Y-up**. The
geometric normal `n = (p1 − p0) × (p2 − p0)` evaluated under the right-hand rule
points outward.

## 2. The `gltf` Crate Loads Bytes Verbatim

`gltf::import(path)` (crate `gltf` 1.4, called from `src/scene/gltf_loader.rs:35`)
returns `(document, buffers, images)` where:

- `document` is the parsed JSON tree.
- `buffers: Vec<gltf::buffer::Data>` is the raw `.bin` payload(s).
- `images` are decoded image pixels.

`primitive.reader(...).read_indices().into_u32()` (line 162) widens
`u8`/`u16`/`u32` indices to `u32` **without re-ordering**. `read_positions()`,
`read_normals()`, `read_tangents()`, `read_tex_coords()` similarly reinterpret
typed accessor bytes without coordinate transformation.

After this stage, the in-memory data is still in glTF's RH Y-up frame, with
CCW-from-outside front faces.

## 3. RH → LH Conversion (Improper Transform #1)

The project uses LH Y-up with +Z forward (see `src/camera.rs`:
`Mat4::look_to_lh`, `Mat4::perspective_lh`). The loader converts at two levels.

### 3a. Per-vertex Z-negation (`gltf_loader.rs:186-203`)

```rust
pos[2]     = -pos[2];
normal[2]  = -normal[2];
tangent[2] = -tangent[2];
tangent[3] = -tangent[3];   // flip handedness w
```

This is a reflection through the XY plane: multiplication by
`M = diag(1, 1, −1, 1)`, with `det(M) = −1`. **An improper transform inverts
orientation**: a triangle that was CCW from outside in RH becomes CW from
outside in LH.

Why the cross product still "works": `glam::Vec3::cross` implements the
fixed algebraic formula `[ay·bz − az·by, az·bx − ax·bz, ax·by − ay·bx]`. This
formula corresponds to the right-hand rule **by definition of the basis**.
After Z-negation, the same algebraic cross product, interpreted in an LH basis,
yields a vector pointing into the surface (i.e., the surface that was
CCW-from-outside in RH is now CW-from-outside in LH).

The `tangent.w` flip preserves bitangent handedness: shaders compute
`B = cross(N, T) * tangent.w`. The cross-product algebraic formula is invariant,
but the basis flipped, so `tangent.w` must flip to keep B on the same side of
the surface (glTF 2.0 §3.7.2.1 "Tangent space").

### 3b. Per-node transform conjugation (`gltf_loader.rs:320-335`)

```rust
let rh_to_lh = Mat4::from_diagonal(Vec4::new(1.0, 1.0, -1.0, 1.0));
let lh_matrix = rh_to_lh * rh_matrix * rh_to_lh;   // M · T · M⁻¹, with M = M⁻¹
```

This is the standard similarity transform. `glam::Mat4::from_scale_rotation_translation`
builds `T = translate · rotate · scale`; `glam::Quat::from_array` reads
`[x, y, z, w]`. Conjugation by `M` re-expresses the transform in the LH basis.
A similarity by an improper matrix is itself proper (`det(M·T·M) = det(T)`), so
this step **does not flip winding** — it just keeps the geometry coherent under
the basis change applied to vertices in 3a.

### 3c. State after step 3

DamagedHelmet vertices in LH world space, with **CW-from-outside** winding.

## 4. View and Projection — Both Proper

`src/camera.rs`:
- `Mat4::look_to_lh(pos, fwd, up)` — LH view matrix; +Z is the look direction.
- `Mat4::perspective_lh(fovy, aspect, 0.1, 100.0)` — LH perspective matrix
  mapping `z ∈ [near, far]` to `z_ndc ∈ [0, 1]` (Vulkan-friendly).

**No projection Y-flip is encoded.** Conventional Vulkan code multiplies the
projection's row 1 by −1 to compensate for Vulkan's Y-down NDC; this project
chooses to use a negative-height viewport instead (step 5).

Both matrices are proper (det > 0 in the relevant subspace), so winding is
preserved: triangles remain CW from outside through clip space and NDC.

## 5. Vulkan NDC and the Negative-Height Viewport (Improper Transform #2)

Vulkan 1.3 spec §27.4 "Coordinate Systems":

> NDC: x ∈ [−1, 1] right-positive, y ∈ [−1, 1] **down-positive**, z ∈ [0, 1].

§27.5 "Controlling the Viewport" — framebuffer-coordinate transform:

```
x_f = (px / 2) · x_d + ox
y_f = (py / 2) · y_d + oy
z_f = pz · z_d + oz
```
with `(ox, oy) = (viewport.x + viewport.width/2, viewport.y + viewport.height/2)`
and `(px, py) = (viewport.width, viewport.height)`. Core since Vulkan 1.1
(formerly `VK_KHR_maintenance1`), `viewport.height` is allowed to be negative.

`src/vulkan/renderer.rs:695-701`:

```rust
let viewport = vk::Viewport::default()
    .x(0.0)
    .y(extent.height as f32)         // start at bottom
    .width(extent.width as f32)
    .height(-(extent.height as f32)) // grow upward
    .min_depth(0.0)
    .max_depth(1.0);
```

With `py < 0`, the y-mapping is a reflection: NDC `y = −1` (Vulkan-up) maps to
framebuffer `y = 0` (top of screen); NDC `y = +1` maps to framebuffer `y = height`
(bottom of screen). This is improper (det = −1) and **flips winding a second
time**.

## 6. Net Winding Through the Pipeline

| Stage | Transform | Det | Winding (from outside) |
|---|---|---|---|
| glTF on disk (RH) | — | — | CCW |
| `gltf` crate read | identity | +1 | CCW |
| Vertex Z-negate | `diag(1,1,−1,1)` | **−1** | **CW** |
| Node transform conjugation | `M · T · M` | +1 | CW |
| `look_to_lh` × `perspective_lh` | proper | +1 | CW |
| Negative-height viewport | y-reflection | **−1** | **CCW** |

**Two improper transforms cancel.** Final framebuffer-space winding: CCW from
outside.

## 7. Vulkan Rasterizer Decision

Vulkan 1.3 §28.4 "Basic Polygon Rasterization":

> Front- and back-facing triangles are determined from the sign of the area
> computed in **framebuffer coordinates**:
> `a = (1/2) Σᵢ (xᵢ · y_{i+1} − x_{i+1} · yᵢ)`.
> If `frontFace == VK_FRONT_FACE_COUNTER_CLOCKWISE`, the triangle is
> front-facing iff `a > 0`.

The signed area is computed **after** the viewport transform, not in NDC. This
is exactly why step 5's flip — not steps 3a's flip alone — determines the final
cull decision.

`src/vulkan/pipeline.rs:269-276` (and the legacy pipeline at lines 136-143):

```rust
.cull_mode(vk::CullModeFlags::BACK)
.front_face(vk::FrontFace::COUNTER_CLOCKWISE)
```

CCW framebuffer triangles ⇒ front ⇒ kept. CW framebuffer triangles ⇒ back ⇒
culled. The helmet renders correctly.

## 8. Why This Configuration

Three configurations would render correctly:

| Config | Vertex Z-flip | Projection Y-flip | Viewport height | `front_face` |
|---|---|---|---|---|
| **A (this project)** | yes | no | negative | CCW |
| B (Vulkan classic) | no, keep RH | yes | positive | CW |
| C (mixed) | yes | yes | positive | CW |

The project picks **A** so the entire world-space math is uniformly LH
(`look_to_lh`, `perspective_lh`, camera `Quat::from_euler(YXZ, …)`,
forward/right/up = `quat * Z/X/Y`), no shader-level sign flips, and the
ubiquitous `front_face = CCW` rule from most Vulkan tutorials still holds.

## 9. Worked Example

A glTF triangle in RH:
`p0 = (0,0,1)`, `p1 = (1,0,1)`, `p2 = (0,1,1)`, indices `(0,1,2)`. Viewed from +Z,
the order p0→p1→p2 traces CCW; right-hand-rule normal is `(0,0,1)` (toward
viewer). ✓

After Z-negate (LH world):
`p0' = (0,0,−1)`, `p1' = (1,0,−1)`, `p2' = (0,1,−1)`. Viewed from −Z (the
"outside" of the original face), p0'→p1'→p2' traces CW. The cross product
`(p1'−p0') × (p2'−p0') = (1,0,0) × (0,1,0) = (0,0,1)` — pointing into the
surface in LH. CW-from-outside in LH, as expected.

Through view × proj into NDC, winding is preserved (CW). The negative-height
viewport reflects y, returning the triangle to CCW in framebuffer space. The
rasterizer measures positive signed area, declares front-facing, and the
fragment shader runs.

## 10. File Map

| Step | File:lines |
|---|---|
| Read indices/positions/normals/tangents | `src/scene/gltf_loader.rs:154-183` |
| Vertex Z-negate + tangent.w flip | `src/scene/gltf_loader.rs:186-203` |
| Node transform conjugation | `src/scene/gltf_loader.rs:320-335` |
| LH camera math | `src/camera.rs` |
| Negative-height viewport | `src/vulkan/renderer.rs:695-701` |
| Pipeline `front_face = CCW`, `cull_mode = BACK` | `src/vulkan/pipeline.rs:136-143`, `269-276` |

## 11. Authoritative References

- **glTF 2.0 Specification**, Khronos Group — §3.7.2 (winding, RH coord system),
  §3.7.2.1 (tangent space and `tangent.w`).
- **Vulkan 1.3 Specification**, Khronos Group — §27.4 (NDC), §27.5 (viewport
  transform with negative height), §28.4 (front-face determination from
  framebuffer-space signed area).
- **`VK_KHR_maintenance1`** — extension that introduced negative viewport
  height; promoted to core in Vulkan 1.1.
- **`gltf` crate** v1.4 — `Reader::read_*` accessor methods reinterpret typed
  accessor bytes without coordinate transformation.
- **`glam` crate** v0.32 — `Mat4::look_to_lh`, `Mat4::perspective_lh`,
  `Vec3::cross` (fixed algebraic formula, basis-agnostic).

# Additional notes
**Note on handedness and winding intuition:**
A fixed index order `(i0, i1, i2)` viewed from the same physical side of the
surface appears CCW in one handedness and CW in the opposite handedness —
handedness flips the *label*, not the geometry. glTF authors content as
"CCW = front in RH" (§1). After this project's Z-negate, that same index order
is "CW from outside in LH world" (§3c).

The pipeline's `frontFace` is a *separate* choice that determines which
framebuffer-space winding is treated as front (§7). **LH coordinates do not
require CW front faces.** Many LH engines (Unreal, this project) use CCW front;
DirectX defaults to CW front via `FrontCounterClockwise = FALSE` but exposes the
flag precisely because CCW is equally valid. The decisive winding for culling
is the one measured in framebuffer coordinates, after every transform including
the viewport (Vulkan 1.3 §28.4) — not the winding in world or NDC space.

## My own notes
The original model should be CCW winding orientation for front face if assuming to be right handed coordinate. If the model is in left handed coordinate, it should be CW winding orientation for front face (not implemented for other type of model than glTF yet.). 

Since glTF 2.0 uses right-handed coordinates by default, the original model should be CCW winding orientation for front face.
