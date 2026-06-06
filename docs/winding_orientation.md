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

`src/vulkan/renderer.rs:854-860`:

```rust
let viewport = vk::Viewport::default()
    .x(0.0)
    .y(extent.height as f32)         // start at bottom
    .width(extent.width as f32)
    .height(-(extent.height as f32)) // grow upward
    .min_depth(0.0)
    .max_depth(1.0);
```

Plugging `viewport.y = H`, `viewport.height = −H` into the spec formula:

```
y_f = H + (−H) · (y_d + 1) / 2 = H/2 · (1 − y_d)
```

Evaluated at the NDC endpoints:

- `y_d = −1` (Vulkan NDC "top", smallest y) → `y_f = H` (framebuffer **bottom**)
- `y_d = +1` (Vulkan NDC "bottom", largest y) → `y_f = 0` (framebuffer **top**)

The y-axis is reflected: `y_f` is a decreasing function of `y_d` (dy_f/dy_d = −H/2 < 0).
The determinant of the 2D (x, y) affine map is `(W/2) · (−H/2) = −W·H/4 < 0`.
This is improper (det = −1) and **flips winding a second time**.

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
| Negative-height viewport | `src/vulkan/renderer.rs:854-860` |
| Pipeline `front_face = CCW`, `cull_mode = BACK` | `src/vulkan/pipeline.rs:136-137`, `271-272` |

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

---

# Skybox Winding — Why `cull_mode = BACK`

> **Bold note:** The skybox pipeline uses `cull_mode = BACK` (see
> `src/vulkan/pipeline.rs:271`), the **same** cull mode as the PBR pipeline. The
> reason is not a coincidence — both pipelines share the negative-height
> viewport (the one improper transform in the chain), and both keep the
> triangles whose framebuffer-space winding is CCW (which the rasterizer treats
> as front-facing with `frontFace = COUNTER_CLOCKWISE`). The math below shows
> exactly why.

## S1. Skybox geometry

`src/vulkan/renderer.rs:280-298` defines a unit cube centered at the origin:

| Vertex | Position (world) |
|---|---|
| 0 | (-1, -1, -1) |
| 1 | (+1, -1, -1) |
| 2 | (+1, +1, -1) |
| 3 | (-1, +1, -1) |
| 4 | (-1, -1, +1) |
| 5 | (+1, -1, +1) |
| 6 | (+1, +1, +1) |
| 7 | (-1, +1, +1) |

Index buffer (36 indices = 12 triangles, 6 faces × 2 triangles each):

```
Front (+Z) : (4,5,6) (4,6,7)
Back  (-Z) : (1,0,3) (1,3,2)
Top   (+Y) : (3,7,6) (3,6,2)
Bot   (-Y) : (0,1,5) (0,5,4)
Right (+X) : (1,2,6) (1,6,5)
Left  (-X) : (0,4,7) (0,7,3)
```

**Convention check — front face `(4, 5, 6)`:**

```
v4 = (-1, -1, +1)
v5 = (+1, -1, +1)
v6 = (+1, +1, +1)
```

In the world XY-plane (LH Y-up, +X right, +Y up), the order
lower-left → lower-right → upper-right is CCW. The cross product
`(v5 − v4) × (v6 − v4) = (2, 0, 0) × (0, 2, 0) = (0, 0, +4)` points in **+Z**,
which is the **outward** normal of the +Z face. ✓

**The cube index buffer is CCW-from-outside in world space (LH Y-up).**

## S2. Transform chain (world → framebuffer)

`shaders/skybox.vert` does:

```glsl
mat4 rotView = mat4(mat3(globals.view));  // strip view translation
vec4 clipPos = globals.proj * rotView * vec4(inPos, 1.0);
gl_Position = clipPos.xyww;               // force NDC z to 1.0
```

| Step | Transform | Determinant | Effect on 2D winding |
|---|---|---|---|
| 1. World → view | `rotView` = upper 3×3 of `view` | +1 (rotation) | none |
| 2. View → clip | `globals.proj` = `Mat4::perspective_lh` | +1 (proper) | none |
| 3. `.xyww` swizzle | algebraic rewrite of `gl_Position` | +1 | none (z is reassigned, x/y/w are preserved) |
| 4. Perspective divide | `(x, y, z, w) / w` | +1 (w > 0 for skybox) | none |
| 5. Viewport | `y = extent.height`, `height = -extent.height` | **−1** | **flip** |

Total improper transforms: **1** (the Y-flip viewport, per
`src/vulkan/renderer.rs:854-860`).

> **Note on the `.xyww` swizzle:** the swizzle is a re-broadcast of the
> clip-space w into the z component. It is **not** a transform — it is a
> GLSL expression that yields `vec4(clipPos.x, clipPos.y, clipPos.w, clipPos.w)`.
> Its effect is purely on the NDC z value (forcing it to 1.0 after the
> perspective divide), not on x or y. Winding is determined by x and y.

> **Note on `rotView`:** the construction `mat4(mat3(view))` is the upper
> 3×3 (rotation + scale) of the view matrix, embedded in a 4×4. The camera
> follows the cube so the skybox appears infinitely far away; this is
> **not** a coordinate transformation with a non-unit determinant. The
> determinant is +1 (rotation).

> **Note on `perspective_lh`:** the LH perspective matrix has the standard
> form (no Y-flip baked in — the project explicitly chooses to flip Y via
> the viewport instead, per `docs/winding_orientation.md` §4). Its
> determinant is +1 on the subspace that matters for winding.

## S3. Consequence for the cube

**One improper transform in the chain.** Therefore:

For any world-space triangle with a well-defined 2D projection onto the
screen, the 2D signed area in framebuffer is **flipped in sign** relative
to the world-space interpretation. Specifically:

- If the triangle's index order, when projected to the world XY-plane,
  is **CCW** (cross product in +Z, the world-up direction), then after
  the Y-flip viewport it is **CW in framebuffer**.
- This is a statement about the **sign of the framebuffer signed area**,
  which is a single number for any 2D triangle.

The world-space "CCW-from-outside" convention (used by the cube index
buffer) means: looking at the triangle from its **outward** side, the
index order traces CCW. The outward side of a triangle is the side from
which the cross product `(v1 − v0) × (v2 − v0)` points toward the viewer.

**Key insight:** the framebuffer signed area of a 2D triangle is
**opposite in sign to "what you would see looking at the triangle from
its outward side"**. The outward side is the side that, in world space,
is "facing the camera" for an outside-looking camera. From the inside
of a CCW-from-outside cube, the camera looks at the **inside** of the
walls, which is the **opposite** side from the outward direction.
Therefore the visible 2D winding in framebuffer is the **opposite** of
the framebuffer winding that the index order would produce for the
outward side.

Concretely:
- World-side: CCW-from-outside index order.
- After one Y-flip in the chain, the outward-side projection in
  framebuffer is CW.
- The camera is on the inside (opposite side from outward), so the
  visible 2D winding in framebuffer is **CCW**.

**For a cube with CCW-from-outside indexing, viewed from inside through
one Y-flip in the transform chain: the visible interior surfaces produce
CCW-in-framebuffer triangles.**

## S4. Rasterizer decision

`src/vulkan/pipeline.rs:271-272`:

```rust
.cull_mode(vk::CullModeFlags::BACK)
.front_face(vk::FrontFace::COUNTER_CLOCKWISE)
```

`cull_mode = BACK` + `front_face = CCW` means: cull triangles whose
framebuffer-space signed area is **negative** (i.e., CW in framebuffer);
keep triangles whose framebuffer-space signed area is **positive** (CCW in
framebuffer).

Per §S3, the visible interior of the cube produces **CCW-in-framebuffer**
triangles. These are kept. ✓

If `cull_mode` were `FRONT` instead, the GPU would cull the visible interior
surfaces and keep only the back-faces (the outside of the cube, which the
camera does not see). The result would be an empty framebuffer. This is
exactly the bug that was originally in `pipeline.rs:271` — it was set to
`FRONT`, culling the skybox. The fix is `BACK`.

## S5. Why the PBR pipeline also uses `cull_mode = BACK`

`src/vulkan/pipeline.rs:136` — the PBR pipeline uses the same cull mode.
This is **not** a coincidence; both pipelines share the same Y-flip
viewport and the same rasterizer rule.

- **PBR (helmet):** the glTF loader negates Z on every vertex
  (`src/scene/gltf_loader.rs:186-203`). This is an improper transform
  applied **at load time**, with det = −1. The helmet's index buffer is
  CCW-from-outside in glTF's RH world; after Z-negation, it is
  **CW-from-outside in the project's LH world** (`docs/winding_orientation.md`
  §3c). The Y-flip viewport is the second improper transform. Two
  improper transforms cancel: the helmet's outside-facing triangles are
  **CCW in framebuffer**, and the camera (on the outside of the helmet)
  sees them. `cull_mode = BACK` keeps them.

- **Skybox (cube):** there is **no** load-time Z-negation. The cube index
  buffer is CCW-from-outside in world (LH Y-up). The Y-flip viewport is
  the only improper transform. The camera is on the **inside** of the
  cube, which corresponds to the **opposite** framebuffer winding from
  "looking from outside" — i.e., **CCW in framebuffer**. `cull_mode = BACK`
  keeps them.

| Pipeline | World-space winding (outside) | Improper transforms in chain | Framebuffer-space winding (visible side) | `cull_mode` |
|---|---|---|---|---|
| PBR (helmet) | CW (after glTF Z-negate) | 2 (Z-negate + viewport) | CCW (camera on outside) | `BACK` (keep CCW) |
| Skybox (cube) | CCW (no load transform) | 1 (viewport only) | CCW (camera on inside) | `BACK` (keep CCW) |

**The shared invariant: the rasterizer keeps the side of the surface the
camera is on, under `cull_mode = BACK` and `front_face = CCW`.**

## S6. The "FRONT cull for skybox" advice — why it doesn't apply here

Some Vulkan tutorials and the project's own earlier notes
(`docs/correct_pbr_plan.md:566`, `docs/ibl_implementation_plan.md:342`)
advise `cull_mode = FRONT` for skyboxes. That advice comes from a
config where the **index buffer is CW-from-outside in world space**
(e.g., the cube is constructed with CW indices, as is common in
OpenGL-era code), so that the outside is back-facing under the standard
NDC convention and the camera-on-inside sees the front. With that index
convention, the visible interior surfaces are front-facing in framebuffer
and the **outside** is back-facing. `cull_mode = FRONT` would cull the
back-faces — but the camera is **inside** the cube, not outside, so
"cull the outside" doesn't make sense in that interpretation.

The advice is internally confused. The correct invariant — which holds
**regardless of which config from `§8` is used, as long as the cube
index buffer is CCW-from-outside in world space** — is: the visible
interior of the cube (the side the camera is on) corresponds to
**CCW-in-framebuffer triangles**, and `cull_mode = BACK` keeps them.

Under the "Vulkan classic" config B (no Z-negate, projection Y-flip,
positive viewport), the transform chain is:

| Step | Det | Framebuffer winding |
|---|---|---|
| World → view | +1 | CCW (CCW-from-outside index buffer) |
| View → clip (with Y-flip in projection) | **−1** | **CW** |
| Clip → NDC | +1 | CW |
| Viewport (positive height) | +1 | CW |

The visible side of the cube (camera inside) is the **opposite** 2D
winding from "looking from outside" — i.e., **CCW** in framebuffer.
`cull_mode = BACK` keeps CCW, so the skybox is visible. Same answer as
this project.

**The shared invariant across all three configs in `§8`:** with a
CCW-from-outside cube index buffer, the skybox uses `cull_mode = BACK`
under the rule "the side the camera is on has CCW-in-framebuffer
winding."

## S7. Anti-symmetry with the PBR pipeline — why this is not "double-cull"

A common confusion: "the helmet is outside-facing with one winding, the
skybox is inside-facing with the same winding, so shouldn't the cull
mode be opposite?" The answer is **no**, because the cull mode and the
winding are not two independent parameters — the rasterizer's cull test
measures 2D winding in framebuffer, and the 2D winding of the visible
side is CCW in both pipelines.

| Pipeline | Index order (world) | Camera position | 2D winding in framebuffer (visible side) | Rasterizer calls it | Cull |
|---|---|---|---|---|---|
| PBR | CW-from-outside (CCW-from-outside glTF → Z-negate → CW) | outside | CCW | front | keep (BACK) |
| Skybox | CCW-from-outside | inside | CCW | front | keep (BACK) |

Both pipelines end up with the **same** rasterizer call ("front") for
the visible side, so both use `cull_mode = BACK`.

## S8. Summary

- **The skybox pipeline uses `cull_mode = BACK` because the cube index
  buffer is CCW-from-outside in world space (LH Y-up), and the only
  improper transform in the world→framebuffer chain is the Y-flip
  viewport.**
- **The camera, positioned on the inside of the cube, sees the interior
  surfaces, which correspond to CCW-in-framebuffer triangles. The
  rasterizer keeps them under `cull_mode = BACK` and `front_face = CCW`.**
- **The PBR pipeline uses the same cull mode, for an analogous but
  separately-derived reason: the helmet's outside-facing triangles are
  CCW-in-framebuffer after two cancelling improper transforms
  (Z-negate at load, Y-flip viewport at draw), and the camera on the
  outside of the helmet sees them.**

## S9. Authoritative references

- **Vulkan 1.3 Specification**, Khronos Group — §28.4 "Basic Polygon
  Rasterization" (front-face determination from framebuffer-space
  signed area).
- **Vulkan 1.3 Specification** — §27.4 (NDC) and §27.5 (viewport
  transform with negative height).
- **`glam` crate** v0.32 — `Mat4::look_to_lh`, `Mat4::perspective_lh`
  (both proper, no Y-flip baked in).
- **`ash` crate** — `vk::CullModeFlags::BACK`, `vk::FrontFace::COUNTER_CLOCKWISE`.
