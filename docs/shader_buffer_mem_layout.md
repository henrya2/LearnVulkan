# Shader Buffer Memory Layout

A canonical reference for how GLSL shader-buffer blocks (UBOs, SSBOs,
push constants) interop with Rust `#[repr(C)]` and `bytemuck::Pod`
structs in this project, and the project-wide rule that every
shader-buffer struct must follow.

> **Authoritative sources.** This document distills four primary
> references and a handful of secondary ones. When in doubt, follow
> the links in §11.
>
> - Vulkan 1.3 Specification, *Standard Buffer Layout* (defines std140
>   and std430) and *Push Constant Interface* (defines the
>   `push_constant` packing).
> - Vulkan Guide, *Shader Memory Layout / Standard Buffer Layout* —
>   the vendor-friendly walkthrough.
> - GLSL 4.5 Specification, *Storage Qualifiers*, *Standard Uniform
>   Block Layout*, and *Conversion and Scalar Built-Ins* (§8.3 covers
>   `floatBitsToUint` and friends).
> - Rust Reference, *Type Layout — `repr(C)` structs*.
>
> Citations in the text are to the Vulkan 1.3 spec unless otherwise
> noted. The GLSL sections are cited by their §-number in the GLSL
> 4.5 spec.

---

## 1. Purpose and scope

This document is the canonical reference for any UBO, SSBO, or
push-constant block in the project. It does **three** things:

1. **States the rule.** Every shader-buffer struct in this project
   uses `glam::Vec4` as the base element. No `f32` / `u32` / `i32` /
   `[f32; N]` fields appear in the struct. All scalar values are
   bit-packed into `Vec4` channels and accessed via `f32::from_bits`
   / `f32::to_bits` on the CPU and the `floatBitsToUint` /
   `uintBitsToFloat` / `floatBitsToInt` / `intBitsToFloat` family
   on the GPU.

2. **Explains the rule.** Walks through the GLSL std140 and std430
   rules, the Rust `#[repr(C)]` rules, and the `bytemuck::Pod`
   semantics, showing that the rule is a deliberate consequence of
   those three layers and that it makes the CPU and GPU byte
   layouts **trivially equivalent**.

3. **Catalogs the existing structs.** Shows the before/after for
   every shader-buffer struct in the project — `GlobalUniforms`,
   `GpuMaterial`, `PushConstants`, `PostProcessUBO`, and
   `BlurPushConstants` — with byte-layout tables and the
   corresponding GLSL.

This is a **normative spec**, not a tutorial. Future contributors
who add a new shader buffer must follow §10's checklist.

---

## 2. The rule (TL;DR)

> **Every shader-buffer struct in this project is a `#[repr(C)]`,
> `bytemuck::Pod`, `bytemuck::Zeroable` struct whose fields are
> exclusively `glam::Mat4`, `glam::Vec4`, or arrays of `glam::Vec4`.**
> No `f32`, `u32`, `i32`, `[f32; N]`, `[u32; N]`, or any other
> scalar or array field appears in the struct.

Scalar values are bit-packed into `Vec4` channels:

- **CPU write**: `f32::from_bits(my_u32)` into the chosen channel.
- **CPU read**: `self.tail.x.to_bits()`.
- **GPU write**: `uintBitsToFloat(my_uint)` into the chosen channel.
- **GPU read**: `floatBitsToUint(pp.tonemap_pack.x)`.

**Channel-reuse policy.** The previous version of this rule said
"`.x` is canonical"; that has been replaced with a channel-agnostic
policy. Any free channel (x, y, z, or w) of any group-named `Vec4`
in a GLSL block is fair game for a bit-packed scalar, **provided**:

- The GLSL block declares the slot (a `vec4 foo;` declaration
  implicitly reserves `.w`; the comment on the GLSL field must say
  so even if the shader never reads the channel).
- The Rust struct mirrors the channel 1:1 and provides a named
  setter/getter for it.
- The pack is **opportunistic** — do not introduce reserved fields
  speculatively; pack only when a real scalar needs a slot.

The only structural exception is the **std140 alignment pad**:
when the GLSL block declares a `vec3` followed by another field,
std140 rounds the `vec3` to 16 B and the trailing 4 B is an
alignment pad, not a free slot. The Rust `Vec4` mirroring it
keeps `.w` at 0 forever — **never** bit-pack here. The
`GpuMaterial::emissive_factor.w` is the canonical example.

The mapping is documented and enforced by **named setter / getter
methods** on the struct (e.g.
`PostProcessUBO::set_tonemap_op(u32)` /
`PostProcessUBO::tonemap_op() -> u32`). Field names like
`exposure_pack`, `lighting_pack`, or `tail` reflect a *group* of
bit-packed scalars, not a single one.

The same rule applies to push-constant blocks (Vulkan §15.8.1):
the struct is a sequence of `Mat4` and `Vec4` fields, with the last
`Vec4` often carrying the bit-packed tail of data and reserved
channels on the rest.

---

## 3. Why this rule is robust

The rule is not stylistic — it is a **mechanical consequence** of
three constraints:

### 3.1 No implicit padding, ever

`glam::Vec4` is 16-byte aligned and 16 bytes wide (`#[repr(C)]` with
`align(16)`). A `#[repr(C)]` struct whose fields are all `Vec4`,
`[Vec4; N]`, or `Mat4` (which is itself 4 × `Vec4`) has:

```
size_of = 16 * (sum of field counts)
align   = 16
```

There is **zero implicit padding** anywhere in the struct. Each
field's offset is a multiple of 16 (its align), so the struct's
alignment (16) is satisfied at every boundary, and the final size
is already a multiple of 16. The struct's `size_of` equals the sum
of its field sizes.

`bytemuck::Pod`'s "no padding" check therefore passes
unconditionally. The `#[derive(Pod)]` macro is satisfied without
explicit `_pad: [u32; 3]` fields or any other workaround.

### 3.2 std140 layout is trivial

A std140 block whose only members are `vec4`, `vec4[]`, and
`mat4` has a trivial layout:

- Each `vec4` member is at the next 16-byte-aligned offset.
- A `vec4[]` array's stride is 16, no extra round-up needed (the
  element's align is already 16, which is the std140 base align).
- A `mat4` member is 4 columns, each 16 B, stride 16, no extra
  round-up.
- The block size is the sum of member sizes, rounded up to 16
  (the std140 base align).

Since the CPU struct is also a flat sequence of `Vec4` / `Mat4` /
`[Vec4; N]` fields, the **CPU struct and the GPU block are
byte-for-byte identical with zero layout work**. There is no field
to translate, no pad member to invent, no offset table to maintain.
The std140 size *is* `size_of::<RustStruct>()` and the
`bytemuck::bytes_of` output is the GPU wire format.

### 3.3 Push constants are also clean

Push constants in Vulkan (§15.8.1) are **tightly packed** (no
std140), with only scalar alignment rules. A `vec4` is still 16 B
aligned, but adjacent scalars *can* share a 4-byte slot. For a
`mat4` + `uint` push constant, the GPU sees only 68 B of data, but
the CPU struct's 16-byte-aligned `Vec4` is 16 B; the trailing
`uint` lives in `.x` of the trailing `Vec4`, and `.y`/`.z`/`.w`
are dead on both sides.

The push-constant **range** (set on the pipeline layout) must
cover the struct's `size_of`, which is 80 B. The GPU only reads
the first 68 B; the extra 12 B are zero and harmless. This is the
**only** place where the CPU and the GPU have a "mismatch" in
what they read, and it is a deliberate consequence of the
alignment rule: by making the trailing field a `Vec4` we satisfy
`bytemuck::Pod` (no implicit padding) and keep the struct
discoverable (one `Vec4` per logical group), at the cost of 12 B
of dead space in the push-constant range.

The net cost is negligible: the push-constant range is the
smallest of all shader-buffer objects in this project, and modern
Vulkan implementations handle 80 B as easily as 68 B.

---

## 4. GLSL std140 and std430 reference

This section is a complete, authoritative description of the
standard uniform block layout rules as the GLSL and Vulkan specs
define them. Skip to §5 if you already know the rules and want
the Rust side.

### 4.1 Scalar and vector base alignments

A GLSL type has a **base alignment** that determines where it must
start within a block:

| GLSL type             | Base alignment | Size (bytes) |
|-----------------------|----------------|--------------|
| `float` / `uint` / `int` (scalar)   | N (4)  | N  |
| `vec2` / `ivec2` / `uvec2`           | 2N (8) | 2N |
| `vec3` / `ivec3` / `uvec3`           | **4N (16)** | 3N (12) |
| `vec4` / `ivec4` / `uvec4`           | 4N (16) | 4N (16) |
| `mat4` (column-major)                | 4N (16) | 4N × 4N = 64 |
| `mat3` (column-major)                | 4N (16) | 3 × 4N-stride × 3N (48 in std140, 36 in std430) |

Note the `vec3` rule: although `vec3` is only 12 bytes of useful
data, it occupies 16 bytes in a std140/std430 block, with the
trailing 4 bytes acting as the next member's alignment pad. This
is why the GLSL `vec3` and the GLSL `vec4` have **identical**
byte footprints in std140 (and a `Vec3` in Rust with `align(16)`
is a drop-in for either).

### 4.2 Block offset and size rules

For a block (or a struct) laid out by these rules:

1. **Member offset.** Each member starts at the next offset ≥
   the member's base alignment.
2. **Struct alignment.** A struct's alignment is the max of its
   members' base alignments. (E.g. `{ float, vec4, float }` has
   align 16 because of the `vec4`.)
3. **Struct size.** A struct's size is the offset of the last
   member plus that member's size, rounded up to the struct's
   alignment. (E.g. `{ vec4, float, float, float, float }` is
   16+16 = 32 B; the four trailing scalars occupy 16 B, and 32
   is already a multiple of 16, so no round-up pad is needed.
   But `{ vec4, float }` is 16+4 = 20 B, rounded up to 32 B,
   with 12 B of pad at the end.)
4. **Block size.** A block's size is the struct's size, rounded
   up to the **block's base alignment** (16 for std140 and
   std430).

### 4.3 Array stride

The stride of an array of T is the base alignment of T, **rounded
up to the base alignment of the block** (16 for std140 and std430
— the rounding is the *same* in both layouts; the difference is
in the struct/array-of-struct rules below).

- `float[4]` in std140: stride 16, total 64 B. Each `float` lives
  alone in a 16 B slot. (`vec4[1]` would also be 16 B; the slot
  is "16-byte aligned", so any 4-byte or 8-byte or 16-byte
  primitive can fit, but std140 forces the round-up to 16.)
- `vec4[4]` in std140: stride 16, total 64 B. No waste.

### 4.4 std140 vs std430

The two layouts are *almost* identical. The differences:

| Rule | std140 | std430 |
|------|--------|--------|
| Array stride rounding | Round up to 16. | **No** rounding. |
| Member offset within struct | Round up to the member's base align. | **No** rounding — back-to-back packing. |

In practice, std430 only differs from std140 for *array elements*
and for *struct members that are themselves structs*. For a flat
sequence of `vec4` and `mat4`, **the two layouts are identical**.
The project uses std140 exclusively (no SSBOs at the time of
writing); the rule here works for std430 identically because
no struct or non-`vec4` array members appear in any block.

This project has no SSBOs. If a future SSBO is added, `layout(std430, ...)`
is preferred for the packing benefits, and the rule below
continues to work without changes (every member is `vec4` /
`mat4` / `vec4[]`, so the std430 vs std140 difference does not
apply).

### 4.5 Push-constant packing (Vulkan §15.8.1)

Push constants are **not** std140. They are tightly packed with
only scalar alignment:

- A `vec3` is 12 B (no trailing pad).
- A `uint` immediately following a `float` shares the 4-byte slot
  alignment.
- A `vec4` is still 16 B aligned.
- Block size is the sum of member sizes, rounded up to 16 (the
  base alignment).

For a `mat4 model; uint materialIndex` push constant:

- `model` is at offset 0, size 64.
- `materialIndex` is at offset 64, size 4.
- Block size = 68, rounded up to 16 → 80 B (12 B of trailing pad
  is part of the push-constant range).

The CPU struct mirrors this by being 80 B (`Mat4` + `Vec4` with
the `uint` in the `.x` of the trailing `Vec4`). The
`bytemuck::Pod` derive accepts it because the trailing `Vec4` is
explicit, not implicit padding.

---

## 5. Rust `#[repr(C)]` reference

This section mirrors §4 from the Rust side.

### 5.1 Field layout

A `#[repr(C)]` struct's fields are laid out in **declaration
order**, with each field at the next offset that is a multiple of
the field's `align_of`:

```
struct S { a: T1, b: T2, c: T3 }
offset(a) = 0
offset(b) = round_up(size_of::<T1>(), align_of::<T2>())
offset(c) = round_up(offset(b) + size_of::<T2>(), align_of::<T3>())
size_of::<S>() = round_up(offset(c) + size_of::<T3>(), align_of::<S>())
align_of::<S>() = max(align_of::<T1>(), align_of::<T2>(), align_of::<T3>())
```

The final round-up is what `bytemuck::Pod` calls "trailing
padding" and rejects. See §6.

### 5.2 Key type sizes and alignments used here

| Rust type    | `size_of` | `align_of` | GLSL counterpart |
|--------------|-----------|------------|------------------|
| `f32`        | 4         | 4          | `float`          |
| `u32`, `i32` | 4         | 4          | `uint`, `int`    |
| `glam::Vec3` | **16**    | **16**     | `vec3` (drop-in) |
| `glam::Vec4` | 16        | 16         | `vec4`           |
| `glam::Mat4` | 64        | 16         | `mat4` (column-major) |

`glam::Vec3` is **`size_of = 16`, not 12**, because the
`bytemuck` feature of `glam` enables `#[repr(C)] align(16)` on
`Vec3` to make it a drop-in for `vec3` in std140 (which is also
16 B). This is the most important non-obvious fact in this
document: a Rust `Vec3` and a Rust `Vec4` have the **same
`size_of` and `align_of`**, so a Rust struct that uses `Vec3`
where the GLSL uses `vec3` is automatically std140-clean.

`glam::Mat4` is **column-major** in memory. `to_cols_array()`
returns `[col0, col1, col2, col3]`, which is the exact byte
layout the GLSL `mat4` expects (GLSL matrices are
column-major by default). `Mat4` and the GLSL `mat4` agree on
byte order without transposition.

### 5.3 Layout table for the project's types

For the rule in §2, only `Vec4`, `[Vec4; N]`, and `Mat4` fields
are used. Every layout is therefore a multiple of 16 with no
implicit padding:

| Field type         | Offset        | Size  |
|--------------------|---------------|-------|
| `Mat4`             | round-up to 16 | 64   |
| `Vec4`             | round-up to 16 | 16   |
| `[Vec4; N]`        | round-up to 16 | 16N  |

`#[repr(C)]` of any sequence of these is **struct-align 16,
struct-size a multiple of 16, no padding**. The std140 block on
the GPU side is the same number.

---

## 6. `bytemuck::Pod` semantics

`bytemuck::Pod` is a **marker trait**. The `#[derive(Pod)]`
macro emits a compile-time check that:

1. The type is `#[repr(C)]` (or `#[repr(transparent)]`).
2. All fields are themselves `Pod`.
3. The type is `Zeroable` (every bit-pattern is a valid value).
4. **The struct's `size_of` equals the sum of its field sizes**
   (no implicit padding).

The trait guarantees nothing about GPU layout. It only
guarantees that the CPU-side byte representation is "clean" and
can be safely transmuted to and from a byte slice via
`bytemuck::bytes_of`.

`bytemuck::Pod` does **not** understand GLSL std140 / std430 /
push-constant rules. The user is fully responsible for shaping
the CPU struct to match the GPU block byte-for-byte.

### 6.1 Three failure modes this project avoids

1. **Implicit padding breaks `Pod`.** Rust inserts padding to
   make each field's offset a multiple of its alignment, and to
   make the struct's `size_of` a multiple of the struct's
   alignment. If those two operations produce any byte that is
   not covered by a field declaration, `Pod` rejects the
   struct. The Vec4 rule (§2) prevents this entirely.

2. **Field reordering changes layout.** `#[repr(C)]` is the
   order-preserving layout (C-ABI compatible). Without
   `#[repr(C)]`, the Rust compiler may reorder fields; the
   `Pod` derive requires `#[repr(C)]` and forbids reordering
   inside the macro's body.

3. **CPU-GPU size drift.** A struct whose `size_of` is **not**
   the same as the std140 / push-constant block size on the
   GPU is a bug that only manifests as misaligned reads on the
   GPU. The project locks the size with a compile-time assert:

   ```rust
   const _: () = assert!(std::mem::size_of::<MyUBO>() == 64);
   ```

   or, equivalently:

   ```rust
   const _: [(); 64] = [(); std::mem::size_of::<MyUBO>()];
   ```

   The latter (array-of-`[(); N]`) form is preferred because
   Rust formats the size mismatch more clearly when the assert
   fires. Both are zero-cost: the const evaluation collapses at
   compile time, and the build fails if the size is wrong.

### 6.2 Project idioms

- **Construction.** `bytemuck::bytes_of(&value)` returns
  `&[u8]`. The `as_ptr()` and `len()` of that slice are used
  with `ptr::copy_nonoverlapping` to upload to a mapped
  buffer.
- **Buffer `range`.** The descriptor's
  `vk::DescriptorBufferInfo::range` must equal the std140 block
  size (the same number as the const assert). For this
  project, the const-locked size *is* the std140 block size
  (because the Vec4 rule guarantees the two are equal), so the
  range is set from `size_of::<T>()` directly.
- **Push-constant range.** The pipeline layout's
  `vk::PushConstantRange::size` must equal the push-constant
  struct's `size_of`. Vulkan accepts the range as a `u32`; the
  project uses `std::mem::size_of::<T>() as u32`.

---

## 7. Bit-cast interop: `f32::to_bits` / `f32::from_bits` ↔ `floatBitsToUint` / `uintBitsToFloat`

### 7.1 GLSL side (GLSL 4.5 §8.3)

The `floatBitsToUint` family is part of the GLSL core in 4.5 and
is always available in Vulkan:

| GLSL function        | Signature                        | Result |
|----------------------|----------------------------------|--------|
| `floatBitsToUint`    | `floatBitsToUint(float) -> uint`  | Bit-identical reinterpretation. |
| `floatBitsToInt`     | `floatBitsToInt(float) -> int`    | Bit-identical reinterpretation. |
| `uintBitsToFloat`    | `uintBitsToFloat(uint) -> float`  | Bit-identical reinterpretation. |
| `intBitsToFloat`     | `intBitsToFloat(int) -> float`    | Bit-identical reinterpretation. |

There is **no conversion**. The bit pattern is preserved exactly.
`f32::NAN` bit patterns (sign + exponent + mantissa) survive
intact through the round trip.

### 7.2 Rust side (stable since 1.20)

| Rust function       | Signature                  | Result |
|---------------------|----------------------------|--------|
| `f32::to_bits()`    | `fn to_bits(self) -> u32`  | Bit-identical reinterpretation. |
| `f32::from_bits()`  | `fn from_bits(u32) -> f32` | Bit-identical reinterpretation. |

`f32` and `u32` are both 32 bits, IEEE 754 is the bit pattern
for `f32`, and the round trip
`u32 -> f32::from_bits -> memcpy -> GPU -> floatBitsToUint -> u32`
is **bit-exact** for any value, including all NaN bit patterns.

For signed `i32` values, the round trip
`i32 -> (as u32) -> f32::from_bits -> memcpy -> GPU -> floatBitsToInt -> i32`
is also bit-exact, because `f32::from_bits` and `floatBitsToInt`
both treat their input as a 32-bit bit pattern with no signed
interpretation.

### 7.3 Project patterns

- **CPU → GPU: send a `u32`.** Pick a `Vec4` channel (`.x` is
  canonical). On the CPU, write `f32::from_bits(my_u32)` into
  the channel. The `memcpy` then places those 4 bytes into the
  GPU buffer. The shader reads
  `floatBitsToUint(<block>.<field>.x)`.
- **GPU → CPU: receive a `u32`.** The shader writes
  `uintBitsToFloat(my_u32)` into a `vec4` channel. The
  `memcpy` brings the bytes back to the CPU. The CPU reads
  `<field>.x.to_bits()`.
- **Signed values.** Use `as u32` on the way out and `as i32`
  on the way back. The bit pattern is preserved.

### 7.4 Why this is sound (and not undefined behavior)

Three independent layers of the spec guarantee the round trip:

1. **`f32::from_bits` is safe.** It is a `const fn` and is
   stable; the documentation states that it "reinterprets the
   bits" and is the inverse of `to_bits()`. There is no
   validation of the bit pattern (e.g. signaling NaN), so any
   `u32` is a valid input.
2. **`bytemuck::bytes_of` is safe on a `Pod` type.** It
   returns a `&[u8]` view of the struct's bytes; the struct
   contains only `Pod` fields, so every byte is defined.
3. **`floatBitsToUint` is a bit-identical re-interpret.** The
   GLSL spec §8.3 explicitly says "the floating-point
   parameter is converted to its unsigned integer bit pattern"
   with no conversion. The reverse is the same.

Combined: any `u32` value can be round-tripped
`u32 -> f32::from_bits -> bytes_of -> memcpy -> GPU -> floatBitsToUint -> u32`
without information loss.

---

## 8. Refactor table (before / after)

Each entry shows the old GLSL block + old Rust struct on the
left, and the new GLSL block + new Rust struct on the right.
The byte layouts are identical between the two; the only
difference is that the new version expresses the layout as
`Vec4`-shaped fields, eliminating implicit padding and making
the channel-to-type mapping explicit.

### 8.1 `GlobalUniforms` / `GlobalUBO` (pbr + skybox)

Used by `pbr.vert`, `pbr.frag`, and `skybox.vert`. Set 0 binding
0. Stage: `VERTEX | FRAGMENT`. Descriptor range: 176 B.

**Before:**

```glsl
layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view; mat4 proj;
    vec4 cameraPos;     // .w = 1.0 (homogeneous position) or 0.0 (dir)
    vec4 lightDir;      // .w = 0.0 (direction)
    float lightIntensity;
    float prefilterMaxLod;
} globals;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: glam::Mat4,
    pub proj: glam::Mat4,
    pub camera_pos: glam::Vec4,        // .w = 1.0 (homogeneous pos) or 0.0 (dir)
    pub light_dir: glam::Vec4,         // .w = 0.0
    pub light_intensity: f32,
    pub prefilter_max_lod: f32,
    pub _pad_tail: [f32; 2],           // explicit pod padding
}
// size_of == 176
```

**After:**

```glsl
layout(set = 0, binding = 0) uniform GlobalUBO {
    mat4 view; mat4 proj;
    vec4 cameraPos;     // .w = 1.0 (homogeneous position) or 0.0 (dir)
    vec4 lightDir;      // .w = 0.0 (direction)
    vec4 lightingPack;  // .x = lightIntensity, .y = prefilterMaxLod, .z..w = 0 (dead)
} globals;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlobalUniforms {
    pub view: glam::Mat4,
    pub proj: glam::Mat4,
    pub camera_pos: glam::Vec4,        // .w = 1.0 or 0.0
    pub light_dir: glam::Vec4,         // .w = 0.0
    pub lighting_pack: glam::Vec4,     // .x = light_intensity, .y = prefilter_max_lod
}
// size_of == 176
```

The new `lighting_pack` field absorbs the `_pad_tail: [f32; 2]`
that the old version needed for `bytemuck::Pod` compliance.
`.z` and `.w` of `lighting_pack` are always zero on the CPU
side (set implicitly by `Vec4::ZERO`) and ignored on the GPU
side. The const assert `assert!(size_of::<GlobalUniforms>() == 176)`
still passes; the std140 block is still 176 B.

**Setter API** (added under the new struct):

```rust
impl GlobalUniforms {
    pub fn set_light_intensity(&mut self, v: f32) { self.lighting_pack.x = v; }
    pub fn light_intensity(&self) -> f32 { self.lighting_pack.x }
    pub fn set_prefilter_max_lod(&mut self, v: f32) { self.lighting_pack.y = v; }
    pub fn prefilter_max_lod(&self) -> f32 { self.lighting_pack.y }
}
```

**GLSL reads** (in `pbr.frag`):

```glsl
// before
vec3 Lo = (... ) * NdotL * globals.lightIntensity * lightColor;
vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * globals.prefilterMaxLod).rgb;

// after
vec3 Lo = (... ) * NdotL * globals.lightingPack.x * lightColor;
vec3 prefilteredColor = textureLod(uPrefilterMap, R, roughness * globals.lightingPack.y).rgb;
```

`pbr.vert` and `skybox.vert` only read `globals.view` and
`globals.proj`, so they need no further changes.

### 8.2 `GpuMaterial` / `Material` (pbr materials)

Array element inside `MaterialBuffer` (`std140` UBO, set 0
binding 1). Element stride 48 B (already a multiple of 16, no
round-up). 64 elements, total buffer size 3072 B. Stage:
`FRAGMENT`. Descriptor range: full buffer size.

**Before:**

```glsl
struct Material {
    vec4 baseColorFactor;
    vec4 emissiveFactor;
    float metallicFactor;
    float roughnessFactor;
    float normalScale;
    float occlusionStrength;
};

layout(std140, set = 0, binding = 1) uniform MaterialBuffer {
    Material materials[64];
} materialBuffer;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuMaterial {
    pub base_color_factor: [f32; 4],
    pub emissive_factor: [f32; 4],
    pub metallic_factor: f32,
    pub roughness_factor: f32,
    pub normal_scale: f32,
    pub occlusion_strength: f32,
}
// size_of == 48
```

**After:**

```glsl
struct Material {
    vec4 baseColorFactor;
    vec4 emissiveFactor;     // .rgb used, .w = 0 (std140 alignment pad)
    vec4 factorPack;         // .x = metallicFactor, .y = roughnessFactor,
                             // .z = normalScale, .w = occlusionStrength
};
```

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GpuMaterial {
    pub base_color_factor: glam::Vec4,
    pub emissive_factor: glam::Vec4,   // .w = 0
    pub factor_pack: glam::Vec4,       // 4 floats packed
}
// size_of == 48
```

The 4 trailing `f32` fields collapse into one `Vec4`. The
element stride is unchanged (48 B), and the `bytemuck::Pod`
derive is satisfied without any explicit pad.

**GLSL reads** (in `pbr.frag`):

```glsl
// before
float metallic = clamp(mrSample.b * mat.metallicFactor, 0.0, 1.0);
float roughness = clamp(mrSample.g * mat.roughnessFactor, 0.045, 1.0);
normalSample = normalize(vec3(normalSample.xy * mat.normalScale, normalSample.z));
float occlusion = mix(1.0, aoSample, clamp(mat.occlusionStrength, 0.0, 1.0));

// after
float metallic = clamp(mrSample.b * mat.factorPack.x, 0.0, 1.0);
float roughness = clamp(mrSample.g * mat.factorPack.y, 0.045, 1.0);
normalSample = normalize(vec3(normalSample.xy * mat.factorPack.z, normalSample.z));
float occlusion = mix(1.0, aoSample, clamp(mat.factorPack.w, 0.0, 1.0));
```

### 8.3 `PushConstants` (PBR)

Per-draw push constant. Stage: `VERTEX | FRAGMENT`. Pipeline
push-constant range: 80 B.

**Before:**

```glsl
layout(push_constant) uniform PushConstants {
    mat4 model;
    uint materialIndex;
} pc;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: glam::Mat4,
    pub material_index: u32,
    pub _pad: [u32; 3],    // explicit pod padding
}
// size_of == 80
```

**After:**

```glsl
layout(push_constant) uniform PushConstants {
    mat4 model;
    vec4 tail;   // .x = floatBitsToUint(materialIndex), .yzw = 0 (dead)
} pc;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PushConstants {
    pub model: glam::Mat4,
    pub tail: glam::Vec4,   // .x = material_index (u32 via f32::from_bits)
}
// size_of == 80
```

The trailing `[u32; 3]` is gone; the `Vec4` is the explicit
`bytemuck::Pod` round-up. The first 68 B of the 80 B range are
the only data the shader reads; the remaining 12 B are
zero-initialized on the CPU side and dead on the GPU side.

**GLSL reads** (in `pbr.frag`):

```glsl
// before
Material mat = materialBuffer.materials[pc.materialIndex];

// after
Material mat = materialBuffer.materials[floatBitsToUint(pc.tail.x)];
```

`pbr.vert` does not read `materialIndex`; it only uses
`pc.model`. No change needed.

### 8.4 `PostProcessUBO` (bright, blur, composite)

Per-frame UBO shared by `bright.frag`, `blur.frag`, and
`composite.frag`. Set 1 binding 0. Stage: `FRAGMENT`. Descriptor
range: 64 B.

**Before:**

```glsl
layout(set = 1, binding = 0) uniform PostProcessUBO {
    float exposure;
    float bloom_threshold;
    float bloom_knee;
    float bloom_intensity;
    vec4 bloom_weights[2];
    uint  tonemap_op;
} pp;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PostProcessUBO {
    pub exposure: f32,            // offset 0
    pub bloom_threshold: f32,     // offset 4
    pub bloom_knee: f32,          // offset 8
    pub bloom_intensity: f32,     // offset 12
    pub bloom_weights: [glam::Vec4; 2], // offset 16, 32 B
    pub tonemap_op: u32,          // offset 48
    pub _pad: [u32; 3],           // offset 52 (std140 block round-up)
}
// size_of == 64
```

**After:**

```glsl
layout(set = 1, binding = 0) uniform PostProcessUBO {
    vec4 exposurePack;        // .x = exposure, .y = bloom_threshold,
                              // .z = bloom_knee, .w = bloom_intensity
    vec4 bloom_weights[2];    // 8 logical weights packed in .xyzw of each
    vec4 tonemapPack;         // .x = floatBitsToUint(tonemap_op), .yzw = 0 (dead)
} pp;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct PostProcessUBO {
    pub exposure_pack: glam::Vec4,   // 4 floats packed
    pub bloom_weights: [glam::Vec4; 2],
    pub tonemap_pack: glam::Vec4,    // .x = tonemap_op (u32 via f32::from_bits)
}
// size_of == 64
```

The four leading `f32` fields collapse into `exposure_pack: Vec4`,
and the trailing `_pad: [u32; 3]` is replaced by the `.yz`
channels of `tonemap_pack` (always zero on the CPU side) plus
the `.w` channel (also zero, used as the std140 block round-up).

**GLSL reads** (in `composite.frag`):

```glsl
// before
bloom *= pp.bloom_intensity;
color *= pow(2.0, pp.exposure);
if (pp.tonemap_op == 1u) { ... } else if (pp.tonemap_op == 2u) { ... } else { ... }

// after
bloom *= pp.exposurePack.w;
color *= pow(2.0, pp.exposurePack.x);
if (floatBitsToUint(pp.tonemapPack.x) == 1u) { ... }
else if (floatBitsToUint(pp.tonemapPack.x) == 2u) { ... }
else { ... }
```

**`bright.frag`** reads `bloom_threshold` and `bloom_knee`:

```glsl
// before
float threshold = pp.bloom_threshold;
float knee = pp.bloom_knee * threshold + 1e-5;

// after
float threshold = pp.exposurePack.y;
float knee = pp.exposurePack.z * threshold + 1e-5;
```

`blur.frag` does not read any of the scalar fields, only
`bloom_weights` (and via push constants, its own `texel_size`
and `direction`). No change beyond the block declaration.

### 8.5 `BlurPushConstants` (blur)

Push constant for the 9-tap Gaussian. Stage: `FRAGMENT`. Pipeline
range: 16 B.

**Before:**

```glsl
layout(push_constant) uniform BlurPC {
    vec2 uTexelSize;  // 1.0 / extent of the input image
    int  uDirection;  // 0 = horizontal, 1 = vertical
} pc;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BlurPushConstants {
    pub texel_size: [f32; 2],    // offset 0
    pub direction: i32,          // offset 8
}
// size_of == 12
```

**After:**

```glsl
layout(push_constant) uniform BlurPC {
    vec4 params;   // .xy = uTexelSize, .z = intBitsToFloat(uDirection) (i32),
                   // .w = 0 (dead)
} pc;
```

```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct BlurPushConstants {
    pub params: glam::Vec4,   // .xy = texel_size, .z = direction (i32 via f32::from_bits)
}
// size_of == 16
```

The struct grows from 12 B to 16 B to satisfy the
`bytemuck::Pod` rule (the trailing `i32` no longer leaves 12 B
of implicit pad). The pipeline range is set from `size_of` and
automatically becomes 16 B; the shader only reads the first
12 B (`.xyz`), the `.w` is dead.

**GLSL reads** (in `blur.frag`):

```glsl
// before
vec2 step = (pc.uDirection == 0)
    ? vec2(pc.uTexelSize.x, 0.0)
    : vec2(0.0, pc.uTexelSize.y);

// after
vec2 step = (floatBitsToInt(pc.params.z) == 0)
    ? vec2(pc.params.x, 0.0)
    : vec2(0.0, pc.params.y);
```

---

## 9. The setter / getter convention

Every scalar that crosses the CPU/GPU boundary is exposed by
named `set_*` and `*` methods on the struct. The methods
encapsulate the bit-cast and the channel choice, so the call
site reads as if the field were a normal typed field:

```rust
// CPU side — looks like ordinary field access.
let mut ubo = PostProcessUBO::default();
ubo.set_tonemap_op(2);                  // u32 -> Vec4.x via f32::from_bits
ubo.set_exposure(1.5);                  // f32 -> Vec4.x
let op: u32 = ubo.tonemap_op();         // Vec4.x -> u32 via to_bits
let exp: f32 = ubo.exposure();          // Vec4.x -> f32
```

```glsl
// GPU side — looks like ordinary field access.
if (floatBitsToUint(pp.tonemapPack.x) == 2u) {
    mapped = aces(color);
}
color *= pow(2.0, pp.exposurePack.x);
```

The convention has three rules:

1. **The struct field is always a `Vec4`-shaped group.**
   Channel `.x` is canonical for the first scalar in a group.
   Channel assignments are documented in the field's doc
   comment.
2. **The setter is `set_<name>(&mut self, v: T)`.** It accepts
   the natural Rust type (`f32`, `u32`, `i32`, …) and writes
   the bit-cast value into the assigned channel. The trait
   `T` is the type as the user thinks of it, not the storage
   type.
3. **The getter is `<name>(&self) -> T`.** It reads the
   channel, applies the bit-cast, and returns the natural
   type. Unused getters may be marked `#[allow(dead_code)]` if
   the project's `set_*` / `*` usage is asymmetric.

For arrays of weights, the convention extends naturally:

```rust
// 8 weights packed into 2 Vec4s.
pub fn set_bloom_weights(&mut self, weights: &[f32; 8]) { ... }
```

The shader reads the weights via `pp.bloom_weights[i].xyzw`,
not as a flat `float[]`, because the storage is `vec4[]` by
the Vec4 rule.

---

## 10. The "add a new struct" checklist

When adding any new UBO, SSBO, or push-constant block to the
project, follow these five steps in order. Steps 1-3 are
**the rule**; steps 4-5 are validation.

1. **Declare the GLSL block with `vec4` / `vec4[]` / `mat4`
   members only.** Decide the channel-to-type mapping for any
   packed scalars. **Identify every free channel** (`.w` of a 3D
   vector, `.y`/`.z`/`.w` of a single-purpose pack, or a trailing
   round-up `Vec4`) and document it in a comment on the GLSL
   block, even if no current consumer reads it. Example:

   ```glsl
   layout(set = N, binding = M) uniform MyBlock {
       mat4 transform;
       vec4 colors[4];           // .rgba per slot
       vec4 params;              // .x = threshold (f32), .y = mode (uint),
                                 // .z = blend (f32), .w reserved (per channel-reuse policy)
   } myBlock;
   ```

   **Do not introduce a channel that the GLSL block has not
   declared.** A GLSL `vec4 foo;` declaration implicitly reserves
   all four channels; the shader is free to ignore them but the
   CPU is not free to repurpose them without updating the GLSL
   comment.

2. **Declare the matching Rust struct as
   `#[repr(C)] #[derive(Clone, Copy, Pod, Zeroable)]`.** All
   fields are `glam::Mat4`, `glam::Vec4`, or `[glam::Vec4;
   N]`. Mirror the GLSL field-for-field, including the
   reserved channels. Add a const assert to lock the size to the
   std140 block size:

   ```rust
   const _: [(); 96] = [(); std::mem::size_of::<MyBlock>()];
   // Or, for non-array-locked sizes:
   const _: () = assert!(std::mem::size_of::<MyBlock>() == 96);
   ```

3. **Add `set_*` / `*` methods for every scalar that crosses
   the boundary, including reserved channels** (use
   `#[allow(dead_code)]` until a consumer exists — the API
   surface is part of the contract). Use `f32::from_bits` for
   `u32` values and `f32::from_bits(v as u32)` for `i32` values.
   Use `to_bits() as i32` for the reverse. Document the
   channel-to-type mapping in each method's doc comment.

4. **Wire the descriptor `range` to the const-locked size.** For
   UBOs and SSBOs, set
   `vk::DescriptorBufferInfo::range(size_of::<MyBlock>())`. For
   push constants, set
   `vk::PushConstantRange::size(size_of::<MyBlock>() as u32)`.

5. **Verify:** run `cargo build`. The const assert will fail
   at compile time if the struct shape drifts from the GLSL
   block. Then run `cargo run` and confirm the validation
   layer is silent about shader-buffer layout (it reports
   mismatches in `vk::CreateBuffer` or
   `vk::UpdateDescriptorSets` calls).

If a future block has a more complex layout (e.g. an array of
structs where the inner struct is not a flat sequence of
`Vec4`s), the rule still applies: reshape the inner struct to
a `Vec4`-only sequence first, then the array stride is
trivial. The rule does not permit any other shape.

---

## 11. Authoritative references

### Primary

- **Vulkan 1.3 Specification** —
  <https://docs.vulkan.org/spec/latest/>
  - §14.5.2 *Buffer Views* and §15 (Resource Descriptors)
  - §15.6.4 *Standard Buffer Layout* — defines std140 and
    std430. <https://docs.vulkan.org/spec/latest/chapters/interfaces.html#interfaces-resources-standard-layout>
  - §15.8.1 *Push Constant Interface* — defines push-constant
    packing.
  - §15.5.2 *Bit-Interpretation Functions* — defines
    `floatBitsToUint` and friends on the SPIR-V side.
- **Vulkan Guide, *Shader Memory Layout / Standard Buffer Layout*** —
  <https://docs.vulkan.org/guide/latest/shader_memory_layout.html>
- **GLSL 4.5 Specification** (PDF) —
  <https://registry.khronos.org/OpenGL/specs/gl/glspec45.core.pdf>
  - §4.1.7 *Uniform Variables* — defines `layout(...)` qualifiers
    including `push_constant`.
  - §7.6.2.2 *Standard Uniform Block Layout* — defines std140
    in full detail with worked examples.
  - §8.3 *Conversion and Scalar Built-Ins* — defines
    `floatBitsToUint` / `uintBitsToFloat` / signed variants.

### Secondary

- **Rust Reference, *Type Layout — `repr(C)` structs*** —
  <https://doc.rust-lang.org/reference/type-layout.html#reprc-structs>
- **`f32::to_bits` / `f32::from_bits`** —
  <https://doc.rust-lang.org/std/primitive.f32.html#method.to_bits>
- **`bytemuck::Pod`** —
  <https://docs.rs/bytemuck/latest/bytemuck/trait.Pod.html>
- **`bytemuck::bytes_of`** —
  <https://docs.rs/bytemuck/latest/bytemuck/fn.bytes_of.html>
- **`glam` Vec3/Vec4/Mat4 layout guarantees** — the
  `bytemuck` feature of `glam` asserts them at compile time.

### Project references

- `src/vulkan/pbr_ubo.rs` — `GlobalUniforms`, `PushConstants`.
- `src/vulkan/postprocess/ubo.rs` — `PostProcessUBO`,
  `BlurPushConstants`.
- `src/scene/material.rs` — `GpuMaterial`.
- `docs/winding_orientation.md` — the project's other
  normative layout document (framebuffer-space winding).
