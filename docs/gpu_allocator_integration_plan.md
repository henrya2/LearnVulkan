# `gpu-allocator` integration plan

Target: replace every `vkAllocateMemory` + `vkBindBufferMemory` / `vkBindImageMemory`
pair in this project with `gpu-allocator` 0.28 (`gpu_allocator::vulkan::Allocator`)
without changing any of the contracts documented in `CODEBUDDY.md`
(debug-marker naming, RenderDoc labels, drop order, shader buffer layout, etc.).

The plan is staged so each phase is independently runnable and the diff per phase
is small. Do not skip phases — they build on the previous one's wiring.

---

## 0. Pre-flight

### 0.1 Crate choice

Pin `gpu-allocator = "0.28"` in `Cargo.toml`. The 0.27+ API exposes a unified
`allocate(&AllocationCreateDesc)` that requires the user to create the
`vk::Buffer` / `vk::Image` first and pass `get_buffer_memory_requirements` /
`get_image_memory_requirements` into the descriptor. The API is identical
between 0.27 and 0.28 for everything used in this project. Targeting 0.28
avoids having an immediate upgrade step.

If a 0.25 lockfile is required for some reason, the `allocate_buffer` /
`allocate_image` convenience methods exist but are deprecated; the refactor
in this plan works for either version with the binding logic rewritten in 0.25
form. Do not support both in the same diff.

### 0.2 What is NOT changing

- All UBO / push-constant struct layouts (`GlobalUniforms`, `PushConstants`,
  `PostProcessUBO`, `BlurPushConstants`, `GpuMaterial`).
- RenderDoc / `VK_EXT_debug_utils` object names.
- `ManuallyDrop<Renderer>` / `ManuallyDrop<VulkanContext>` ordering in
  `src/app.rs` — preserved, but `App::drop` gains an explicit
  `renderer.destroy(device, allocator)` call before the `ManuallyDrop` drops.
- The Y-flip viewport, the per-frame loop, the glTF loader's Z-negate.
- The shader `.spv` files.

### 0.3 Inventory of every allocation site to be refactored

Captured by `Grep "allocate_memory\|find_memory_type\|bind_image_memory\|bind_buffer_memory"`
in `src/vulkan/`:

| Site | Resource | Memory location | `vk::MemoryPropertyFlags` |
|---|---|---|---|
| `buffer.rs::create_buffer` | any buffer (staging or target) | caller-driven | caller-driven |
| `buffer.rs::create_device_local_buffer` | skybox VB/IB, scene VB/IB, material buffer | `GpuOnly` for target, `CpuToGpu` for staging | mixed |
| `texture.rs::from_rgba8_with_format` | texture image + staging | `GpuOnly` for image, `CpuToGpu` for staging | mixed |
| `cubemap.rs::Cubemap::create_empty` | IBL cubemap image | `GpuOnly` | `DEVICE_LOCAL` |
| `brdf_lut.rs::generate_brdf_lut` | BRDF LUT image | `GpuOnly` | `DEVICE_LOCAL` |
| `swapchain.rs::create_swapchain` (inline loop, not `find_memory_type`) | depth image | `GpuOnly` | `DEVICE_LOCAL` |
| `postprocess/resources.rs` | per-image HDR scene color | `GpuOnly` | `DEVICE_LOCAL` |
| `postprocess/pyramid.rs::create_mip_image` | bloom mip + temp images | `GpuOnly` | `DEVICE_LOCAL` |
| `renderer.rs` | 2× global UBO (per-frame) | `CpuToGpu` | `HOST_VISIBLE | HOST_COHERENT` |
| `postprocess/resources.rs` | 2× postprocess UBO (per-frame) | `CpuToGpu` | `HOST_VISIBLE | HOST_COHERENT` |

Per-frame, in steady state, there are 4 HOST_VISIBLE buffers and
roughly 20–30 DEVICE_LOCAL allocations (depending on swapchain image count and
glTF model). The current code makes one `vkAllocateMemory` per allocation; the
target code makes one per memory type per "live block" the allocator decides
to grow.

---

## 1. New module `src/vulkan/memory.rs`

A new file with a thin newtype around the allocator. The newtype exists so
the project's `Debug`/logging story is uniform and so the `Drop` impl is
predictable. **Note:** `gpu_allocator::vulkan::Allocator` implements `Drop` —
its `Drop` calls `vkFreeMemory` for any still-live backing blocks. This is a
safety net, not a correctness path: all `Allocation`s must be explicitly freed
via `allocator.free()` *before* the allocator is dropped. If they are not, any
`vk::Buffer`/`vk::Image` still bound to the freed memory become dangling
handles, which triggers validation errors at best and undefined behavior at
worst. See §9 for the full drop-order contract.

```rust
// src/vulkan/memory.rs (pseudocode — no edit yet)
use ash::vk;
use gpu_allocator::MemoryLocation;                                        // top-level, NOT vulkan::
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc, Allocation,
    AllocationCreateDesc, AllocationScheme};

pub struct MemoryAllocator {
    pub inner: Allocator,
}

impl MemoryAllocator {
    pub fn new(
        instance: ash::Instance,
        device: ash::Device,
        physical_device: vk::PhysicalDevice,
    ) -> Self {
        let inner = Allocator::new(&AllocatorCreateDesc {
            instance,
            device,
            physical_device,
            // AllocatorDebugSettings is #[non_exhaustive] — must use Default
            debug_settings: Default::default(),
            buffer_device_address: false, // project does not use BDA
            // AllocationSizes implements Default — uses 256 MB device / 64 MB host
            allocation_sizes: Default::default(),
        }).expect("Failed to create gpu_allocator::Allocator");
        Self { inner }
    }

    /// Allocate a `vk::Buffer` plus its backing `Allocation` for the given
    /// `MemoryLocation`. Returns `OwnedBuffer` — call `destroy(device, allocator)`
    /// to free (the buffer is destroyed first, then `allocator.free(allocation)`).
    /// `OwnedBuffer` does NOT implement `Drop` because `Allocation` has no Drop
    /// and freeing requires `&mut Allocator`.
    pub fn create_buffer(
        &mut self,
        device: &ash::Device,
        name: &str,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
    ) -> OwnedBuffer {
        let info = vk::BufferCreateInfo::default()
            .size(size).usage(usage).sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { device.create_buffer(&info, None).unwrap() };
        let reqs = unsafe { device.get_buffer_memory_requirements(buffer) };
        let alloc = self.inner.allocate(&AllocationCreateDesc {
            name,
            requirements: reqs,
            location,
            linear: true,
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        }).expect("gpu-allocator failed to allocate buffer memory");
        unsafe { device.bind_buffer_memory(buffer, alloc.memory(), alloc.offset()).unwrap() };
        OwnedBuffer { buffer, allocation: Some(alloc), size }
    }

    /// Create a HOST_VISIBLE | HOST_COHERENT UBO buffer and return it alongside
    /// a persistently-mapped `*mut u8` pointer already at the correct sub-allocation
    /// offset. The pointer is valid until `destroy` is called.
    pub fn create_host_mapped_ubo(
        &mut self,
        device: &ash::Device,
        name: &str,
        size: vk::DeviceSize,
    ) -> OwnedBuffer {
        let mut buf = self.create_buffer(
            device, name, size, vk::BufferUsageFlags::UNIFORM_BUFFER,
            MemoryLocation::CpuToGpu,
        );
        // Store the mapped pointer inside OwnedBuffer so callers retrieve it via
        // `mapped_ptr()`. Allocation::mapped_ptr() already accounts for the
        // sub-allocation offset.
        buf.mapped = buf.allocation.as_ref()
            .expect("create_host_mapped_ubo: allocation missing after create_buffer")
            .mapped_ptr()
            .map(|ptr| ptr.as_ptr() as *mut u8)
            .expect("create_host_mapped_ubo: CpuToGpu allocation not mapped");
        buf
    }

    // --- Image helpers ---
    //
    // Image creation is more varied than buffer creation (different mip counts,
    // array layers, cube compatibility, etc.). Three paths are provided:
    //
    //   create_image         — takes a pre-built vk::ImageCreateInfo (max flexibility)
    //   create_image_2d      — common 2D non-cube image (textures, scene color, BRDF LUT)
    //   create_image_cubemap — cube-compatible image (IBL cubemaps)
    //
    // All of them call the allocator's `allocate` and bind with the returned
    // offset, then return `OwnedImage { image, allocation, extent, format, mip_levels }`.

    pub fn create_image(
        &mut self,
        device: &ash::Device,
        name: &str,
        image_info: &vk::ImageCreateInfo,
        location: MemoryLocation,
    ) -> OwnedImage {
        let image = unsafe { device.create_image(image_info, None).unwrap() };
        let reqs = unsafe { device.get_image_memory_requirements(image) };
        let alloc = self.inner.allocate(&AllocationCreateDesc {
            name,
            requirements: reqs,
            location,
            linear: false, // images are always tiled (optimal)
            allocation_scheme: AllocationScheme::GpuAllocatorManaged,
        }).expect("gpu-allocator failed to allocate image memory");
        unsafe { device.bind_image_memory(image, alloc.memory(), alloc.offset()).unwrap() };
        OwnedImage {
            image,
            allocation: Some(alloc),
            extent: image_info.extent,
            format: image_info.format,
            mip_levels: image_info.mip_levels,
        }
    }

    /// Dedicated image variant. The `vk::Image` handle is passed to
    /// `AllocationScheme::DedicatedImage(image)` so the driver can optimize.
    pub fn create_dedicated_image(
        &mut self,
        device: &ash::Device,
        name: &str,
        image_info: &vk::ImageCreateInfo,
    ) -> OwnedImage {
        let image = unsafe { device.create_image(image_info, None).unwrap() };
        let reqs = unsafe { device.get_image_memory_requirements(image) };
        let alloc = self.inner.allocate(&AllocationCreateDesc {
            name,
            requirements: reqs,
            location: MemoryLocation::GpuOnly,
            linear: false,
            allocation_scheme: AllocationScheme::DedicatedImage(image),
        }).expect("gpu-allocator failed to allocate dedicated image memory");
        unsafe { device.bind_image_memory(image, alloc.memory(), alloc.offset()).unwrap() };
        OwnedImage {
            image,
            allocation: Some(alloc),
            extent: image_info.extent,
            format: image_info.format,
            mip_levels: image_info.mip_levels,
        }
    }
}
```

### OwnedBuffer / OwnedImage — no Drop; explicit `destroy` only

`Allocation` in gpu-allocator 0.28 has **no `Drop` impl** — you must call
`allocator.free(allocation: Allocation)` (takes `&mut Allocator`) to release
memory. `Allocator` itself **does** implement `Drop` — it frees any
still-outstanding `VkDeviceMemory` blocks — but this is a leak-prevention
safety net, not a correctness path. If `Allocation`s are not explicitly freed,
their bound `vk::Buffer`/`vk::Image` handles become dangling when the
allocator's `Drop` frees the backing memory.

Therefore `OwnedBuffer` and `OwnedImage` do NOT implement `Drop`.
They provide explicit `destroy` methods that take the resources needed for
cleanup:

```rust
/// OwnedBuffer in src/vulkan/memory.rs
pub struct OwnedBuffer {
    pub buffer: vk::Buffer,
    pub allocation: Option<Allocation>, // Option for take()-based cleanup
    pub size: vk::DeviceSize,
    mapped: Option<*mut u8>,            // set by create_host_mapped_ubo
}

impl OwnedBuffer {
    /// Returns the persistently-mapped CPU pointer (already at the correct
    /// sub-allocation offset). Panics if `create_host_mapped_ubo` was not used.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped.expect("OwnedBuffer::mapped_ptr called on non-host-mapped buffer")
    }

    /// Destroy the buffer and free its allocation through the allocator.
    /// Must be called before the allocator itself is dropped.
    /// Must be called at most once — double-destroy is UB.
    pub fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe { device.destroy_buffer(self.buffer, None); }
        if let Some(allocation) = self.allocation.take() {
            allocator.inner.free(allocation).expect("Failed to free buffer allocation");
        }
    }
}

/// OwnedImage in src/vulkan/memory.rs
pub struct OwnedImage {
    pub image: vk::Image,
    pub allocation: Option<Allocation>,
    pub extent: vk::Extent3D,
    pub format: vk::Format,
    pub mip_levels: u32,
}

impl OwnedImage {
    /// Destroy the image and free its allocation through the allocator.
    /// Must be called before the allocator itself is dropped.
    /// Must be called at most once — double-destroy is UB.
    pub fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe { device.destroy_image(self.image, None); }
        if let Some(allocation) = self.allocation.take() {
            allocator.inner.free(allocation).expect("Failed to free image allocation");
        }
    }
}
```

`Allocation` implements `Send` and `Default`; the `Option<Allocation>` pattern
supports `take()`-based cleanup (for swapchain resize paths) and the
`Default` impl means a `take()`d value is a safe dummy.

**Double-destroy is UB.** `destroy` must be called at most once per
`OwnedBuffer`/`OwnedImage`. The `buffer`/`image` fields are not wrapped in
`Option`, so calling `destroy` after a `take()`-based cleanup path would
double-destroy the `vk::Buffer`/`vk::Image` handle (UB). The `take()` pattern
is used for the `allocation` field only — the caller must ensure that after
`take()`, the owning struct is not used again (e.g. the entire struct is
replaced during swapchain resize). If double-destroy protection is needed,
wrap the `buffer`/`image` fields in `Option` and `take()` them in `destroy`,
but this adds complexity for no practical benefit in this project.

**Mapped pointer safety note:** `Allocation::mapped_ptr()` returns a pointer
already at the correct sub-allocation offset, and its valid range is exactly
`allocation.size()` bytes. When using `create_host_mapped_ubo`, the caller's
`memcpy` into the mapped pointer must not write more than `allocation.size()`
bytes. In practice, the UBO struct size (e.g. `size_of::<GlobalUniforms>()`)
is always ≤ `allocation.size()` (which includes alignment rounding), so this
is safe. However, the old code mapped the entire `VkDeviceMemory` and could
theoretically write past the sub-allocation boundary — the new code is
strictly safer.

**Explicit `unmap_memory` removal:** The current code in `ktx2_loader.rs`
calls `device.unmap_memory(staging.memory, ...)` before destroying the
staging buffer. With the allocator, unmapping is handled internally when
`allocator.free(allocation)` runs. All explicit `device.unmap_memory()` calls
must be removed when converting to the allocator path — calling `unmap_memory`
on a sub-allocated region would unmap the entire backing block, breaking
other co-located sub-allocations.

### 1.1 Why a newtype instead of using the raw `Allocator`

- Single place to assert "every allocation is named" before being returned
  (RenderDoc sees `GlobalUBO_Frame0` etc. — aligns with CODEBUDDY.md's debug-marker convention).
- The `&mut self` borrow lives in one place, so the rest of the project does
  not need to thread `&mut Allocator` through every call.
- The debug-name plumbing is consistent across buffers/images instead of
  scattered.

### 1.2 `memory.rs` does NOT own `vk::Device`

The allocator's `instance` and `device` fields are clones of the
`VulkanContext` handles. The actual lifetime is bound to `VulkanContext`.
The `MemoryAllocator` is wrapped in `ManuallyDrop` and explicitly dropped
*before* the device in `VulkanContext::Drop` (see §9.3).

### 1.3 Where the allocator lives

Add `MemoryAllocator` as a `ManuallyDrop<MemoryAllocator>` field of
`VulkanContext`. The allocator is tied to the device, not to the renderer,
and follows the existing pattern where `Renderer` already holds `ctx.device`
as a clone and receives `&ctx` at construction.

Accessing the allocator: `&mut ctx.allocator` yields `&mut ManuallyDrop<MemoryAllocator>`,
which dereferences to `&mut MemoryAllocator` via `DerefMut`. All call sites
use `&mut *ctx.allocator` or `&mut ctx.allocator.inner` to get the `&mut`
reference needed by `destroy` and `allocate`.

### 1.4 Renderer must NOT implement `Drop` — use explicit `destroy`

**This is the critical architectural decision for the whole refactor.**

The current `Renderer` implements `Drop`, which destroys all device objects.
With the allocator, every `OwnedBuffer::destroy` / `OwnedImage::destroy` call
requires `&mut MemoryAllocator` — but `Drop::drop(&mut self)` only has
`&mut self`, with no access to `ctx.allocator`. There is no way to pass the
allocator into `Drop`.

Attempting to store `&'a MemoryAllocator` in `Renderer` does not help:
- `&'a` is immutable — you need `&mut` for `destroy`.
- `&'a mut` would make `Renderer` non-`Clone` and would conflict with the
  existing `&ctx` borrows in `Renderer::new`.
- `Rc<RefCell<MemoryAllocator>>` introduces runtime borrow checking and
  obscures the lifetime contract.

**Solution: Replace `Drop for Renderer` with an explicit
`Renderer::destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`
method.** `App::drop` calls `renderer.destroy(&device, &mut ctx.allocator)`
before `ManuallyDrop::drop(&mut self.renderer)`. The `Drop for Renderer` impl
is removed entirely — or reduced to a `log::warn!` that fires if `destroy`
was not called (debug builds only), matching the "explicit cleanup" pattern
already used by `OwnedBuffer` and `OwnedImage`.

This pattern is consistent throughout the codebase:
- `OwnedBuffer` — no `Drop`, explicit `destroy(device, allocator)`
- `OwnedImage` — no `Drop`, explicit `destroy(device, allocator)`
- `Texture` — no `Drop`, explicit `destroy(device, allocator)`
- `Cubemap` — no `Drop`, explicit `destroy(device, allocator)`
- `BloomPyramid` — no `Drop`, explicit `destroy(device, allocator)`
- `PostProcessResources` — no `Drop`, explicit `destroy(device, allocator)`
- `Renderer` — no `Drop`, explicit `destroy(device, allocator)`

Every `destroy` call site passes `&mut ctx.allocator` alongside the device
handle, and the call originates from `App::drop` which owns both `ctx` and
`renderer`.

### 1.5 `recreate_swapchain` needs `&mut` access to the allocator

The current `recreate_swapchain(&mut self, ctx: &VulkanContext)` takes an
immutable `ctx`. It needs `&mut ctx.allocator` to:
- Destroy old `OwnedImage`/`OwnedBuffer` resources (depth, scene color, bloom)
- Create new ones

Change the signature to `recreate_swapchain(&mut self, ctx: &mut VulkanContext)`.
`App::draw_frame` calls `recreate_swapchain` — it must thread `&mut ctx` through.
Since `App` owns both `ctx` and `renderer` via `ManuallyDrop`, the `&mut`
borrow is available. The `device_wait_idle()` call at the top of
`recreate_swapchain` ensures no GPU work is in flight, so the `&mut` to the
allocator does not race with any ongoing allocation.

Alternatively, split the allocator out as a separate parameter:
`recreate_swapchain(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`.
This keeps `VulkanContext` immutable but requires passing more arguments. Pick
whichever is cleaner at implementation time — the `&mut VulkanContext`
approach is simpler and consistent with the rest of the refactor.

**General `&mut VulkanContext` propagation:** The `Renderer::new` constructor
currently takes `ctx: &VulkanContext`. Since it calls `create_device_local_buffer`
(which needs the allocator), it must take `ctx: &mut VulkanContext` instead.
Similarly, any function that creates buffers or images (texture loading,
cubemap creation, BRDF LUT generation) needs `&mut` access. The propagation
is straightforward: every function that currently takes `&VulkanContext` and
calls any allocation function must be updated to `&mut VulkanContext`. The
only functions that can remain immutable are those that only read from the
context (e.g. querying queue family indices).

---

## 2. Phase 1 — `buffer.rs` (do this first; everything else copies the pattern)

### 2.1 Refactor `create_buffer` to take the allocator

The function's existing 6-arg signature is replaced by a 5-arg one that takes
`&mut MemoryAllocator` plus a `MemoryLocation`. The `properties: vk::MemoryPropertyFlags`
parameter is removed (the allocator picks the right memory type from
`MemoryLocation`; the explicit flags were a workaround for the lack of
abstraction).

`find_memory_type` is left in `buffer.rs` for now but marked `#[allow(dead_code)]`.
It will be deleted in the final phase (Phase 7) after all call sites are
migrated. This keeps every intermediate commit buildable.

### 2.2 Refactor `create_device_local_buffer`

Same idea: the staging buffer is now `allocator.create_buffer(... MemoryLocation::CpuToGpu)`,
and the target is `allocator.create_buffer(... MemoryLocation::GpuOnly)`. The
`staging.destroy(device)` becomes `staging.destroy(device, allocator)`.

The function currently takes `ctx: &VulkanContext` (immutable). It needs
`&mut` access to the allocator for `create_buffer` and `destroy`. Change the
signature to `ctx: &mut VulkanContext` so it can access `&mut ctx.allocator`.
All callers of `create_device_local_buffer` (skybox VB/IB in `renderer.rs`,
scene VB/IB and material buffer in `gltf_loader.rs`) must be updated
accordingly. The `Renderer::new` constructor already takes `&VulkanContext`;
it will need `&mut VulkanContext` (or a separate allocator parameter) to
call `create_device_local_buffer`.

`with_one_time_command` is unchanged — it operates on `vk::CommandBuffer`,
not memory.

### 2.3 Refactor the global UBO allocation in `renderer.rs`

Currently:

```rust
let buf = create_buffer(device, alloc_size, UNIFORM_BUFFER,
    HOST_VISIBLE | HOST_COHERENT, ...);
let ptr = device.map_memory(buf.memory, 0, alloc_size, EMPTY).unwrap() as *mut u8;
global_uniforms.push(buf);
global_mapped.push(ptr);
```

Becomes:

```rust
let mut owned = ctx.allocator.create_host_mapped_ubo(
    &ctx.device, "GlobalUBO_FrameN", alloc_size);
let ptr = owned.mapped_ptr(); // *mut u8, already at correct sub-allocation offset
global_uniforms.push(owned);  // store OwnedBuffer, not GpuBuffer
global_mapped.push(ptr);
```

`Allocation::mapped_ptr()` returns `Option<NonNull<c_void>>`. The
`create_host_mapped_ubo` helper resolves the `Option` (panic if unmapped),
converts to `*mut u8`, and stores it in `OwnedBuffer.mapped`. Callers use
`owned.mapped_ptr() -> *mut u8` — the same interface the existing code
expects.

The `*mut u8` mapping in `global_mapped` is unchanged (it is read every frame
in `draw_frame` for the `memcpy`). The pointer is valid until `owned.destroy()`
is called. The destroy pattern in `Renderer::destroy` becomes:

```rust
// In Renderer::destroy(device, allocator), not in Drop
for mut owned in self.global_uniforms.drain(..) {
    // unmapping is handled internally by the allocator when the Allocation is freed
    owned.destroy(device, allocator);
}
// Similarly for all other OwnedBuffer/OwnedImage fields
```

### 2.4 The `GpuBuffer` struct is replaced by `OwnedBuffer`

`GpuBuffer { buffer, memory, size }` in `src/vulkan/buffer.rs` becomes
`OwnedBuffer { buffer, allocation: Option<Allocation>, size, mapped }` (defined
in `src/vulkan/memory.rs`). The `destroy(&ash::Device)` method is replaced by
`destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`.
`OwnedBuffer` does NOT implement `Drop` — see §1 for the rationale.

Every place that called `GpuBuffer::destroy` is rewritten to call
`owned.destroy(device, allocator)` with the allocator argument added.
This is most of the affected code. Use `compiler errors as a checklist` — the
`GpuBuffer` rename forces every caller to be updated before the project
builds again. Do not silence the errors with `#[allow(dead_code)]`; if the
project builds with a stray `GpuBuffer` it means a path was missed.

---

## 3. Phase 2 — `texture.rs::from_rgba8_with_format`

The function does two things: a transient staging buffer (small, host-visible)
and a persistent device-local image (potentially large for a 4K texture).

- Staging: `allocator.create_buffer(..., TRANSFER_SRC, CpuToGpu)` — destroyed
  at end of scope with `staging.destroy(device, allocator)`.
- Image: `allocator.create_image(..., TRANSFER_DST | TRANSFER_SRC | SAMPLED, GpuOnly)`
  using the `create_image` helper which takes a pre-built `vk::ImageCreateInfo`.

`mip_levels > 1` and the blit chain are unchanged. The final layout
transition to `SHADER_READ_ONLY_OPTIMAL` is unchanged.

The `Texture { image, memory, view, sampler }` struct is renamed
`Texture { image: vk::Image, allocation: Option<Allocation>, view, sampler }`.
The `destroy(&ash::Device)` method is replaced by `destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`.
The fallback textures in `gltf_loader.rs::create_fallback_textures` follow the
same pattern.

---

## 4. Phase 3 — `cubemap.rs::Cubemap::create_empty`

Used by IBL (`load_ktx2_cubemap`) and indirectly by `BrdfLut`. The cubemap
image is large (1024–2048 px square × 6 faces × mip chain) and the IBL
prefilter cubemap is the largest allocation in steady state. **Use
`AllocationScheme::DedicatedImage(image)` for the env cubemap and prefilter
cubemap** — they are big enough that a sub-allocated block would force the
DEVICE_LOCAL pool to grow a large hole for one user, defeating the
sub-allocator. The irradiance cubemap (smaller) and BRDF LUT (512×512) can
stay managed (`GpuAllocatorManaged`).

`Cubemap::create_empty` currently takes `(device, instance, physical_device, size, mip_levels, format, usage)`.
The signature is updated to `(device, allocator, size, mip_levels, format, usage)` — the
`instance` and `physical_device` parameters are removed. Callers that need updating:
- `src/vulkan/ktx2_loader.rs::load_ktx2_cubemap` (called 3 times from `ibl.rs`
  for env, irradiance, and prefilter cubemaps)

`Cubemap` gains a `destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`
method that destroys the sampler, view, image, and frees the allocation.

The 6-face upload loop in `load_ktx2_cubemap` continues to use a per-level
staging buffer; that staging buffer is the textbook sub-allocation win
(small, transient, freed in the same scope). Convert it to the
`MemoryAllocator::create_buffer(... CpuToGpu)` path. The per-mip-level staging
buffer lifecycle is:
1. `allocator.create_buffer(..., CpuToGpu)` — creates buffer + allocation
2. Use `allocation.mapped_ptr()` to get the CPU pointer (already at correct offset)
3. Copy per-face data via `ptr::copy_nonoverlapping`
4. Submit one-time command (unchanged `with_one_time_command`)
5. `staging.destroy(device, allocator)` at end of loop iteration

`ktx2_loader.rs::load_ktx2_cubemap` does not need its own allocation logic
once `Cubemap::create_empty` is updated.

---

## 5. Phase 4 — `brdf_lut.rs::generate_brdf_lut`

Replace the inline `vkAllocateMemory` block with
`ctx.allocator.create_image(&ctx.device, "BRDF_LUT", &image_info, MemoryLocation::GpuOnly)`.
The image is 512×512 R16G16_SFLOAT — managed allocation is fine, not dedicated.
The temporary render pass, pipeline, and framebuffer that this function creates
are unchanged (non-memory resources).

`BrdfLut` gains `destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`
instead of the current `destroy(&ash::Device)`. The function's signature
(`generate_brdf_lut(ctx, command_pool)`) is unchanged — it accesses the
allocator through `&ctx.allocator`. Callers in `ibl.rs` are unaffected.

---

## 6. Phase 5 — `swapchain.rs::create_swapchain` (depth image)

The depth image is one of the larger DEVICE_LOCAL allocations and is
recreated on every swapchain resize (so it churns the sub-allocator). Two
choices:

- **A. Sub-allocate the depth image.** Pros: same code path for all images,
  simpler. Cons: a swapchain resize (e.g. window drag) frees a ~4 MB
  allocation and immediately re-allocates one, which is a sub-allocator
  shrink-and-grow event.
- **B. Use `AllocationScheme::DedicatedImage(image)` for the depth image.**
  Pros: the sub-allocator pool is unaffected by resize. Cons: one extra
  `VkDeviceMemory` per swapchain.

**Pick B.** A drag-resize storm sub-allocates 30+ times per second; with
`DedicatedImage(image)` the resize has no effect on the rest of the DEVICE_LOCAL
pool. Note: `DedicatedImage` requires the `vk::Image` handle — order is
`create_image → get_memory_requirements → allocate(DedicatedImage(image)) → bind`.

The function currently takes `(instance, device, physical_device, surface_loader, surface, ...)`.
Change to `(device, allocator, surface_loader, surface, ...)` — the
`instance` and `physical_device` parameters are removed (they are no longer
needed for memory type selection), and `allocator: &mut MemoryAllocator` is
added. The `recreate_swapchain` method signature in `Renderer` changes to
`recreate_swapchain(&mut self, ctx: &mut VulkanContext)` to provide `&mut`
access to the allocator (see §1.5 for rationale).

The depth image's current memory-type selection is an **inline loop** in
`create_swapchain` (lines 157-196), not the shared `find_memory_type` from
`buffer.rs`. This inline loop is replaced by the allocator call.

`cleanup_swapchain` becomes: `depth_owned.destroy(device, allocator)`.
The depth image + view are destroyed as part of the `OwnedImage` destroy.

---

## 7. Phase 6 — `postprocess/resources.rs`

Two changes:

### 7.1 The per-swapchain HDR scene color images

Currently one `vkAllocateMemory` per image. These are 800×600×8 bytes × 8 B
(typically), so each is ~3.7 MB. Sub-allocate them; the entire set fits in
a single DEVICE_LOCAL block on most GPUs. The existing `scene_memories`
field becomes `scene_allocations: Vec<Allocation>` (or is dropped and the
allocation lives inside `OwnedImage`).

### 7.2 The postprocess UBO (per-frame)

Identical to the global UBO refactor in §2.3. Use `create_host_mapped_ubo`.

### 7.3 The `destroy` method

Becomes `destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`.
The existing explicit destroy calls inside are updated to pass `allocator` to
each `OwnedBuffer`/`OwnedImage::destroy` method.

The `ReusePostProcessResources` resize path (called from
`Renderer::recreate_swapchain`) needs special care: the old
`PostProcessResources` must be fully destroyed *before* the new one is
constructed, so the freed `Allocation`s return memory to the pool before
the new `create_image` / `create_buffer` calls go to the allocator. The
existing `let old_pp = self.postprocess.take(); if let Some(mut old) = old_pp
{ old.destroy(&self.device); }` pattern works; rewrite to pass `allocator`
as the second argument and let the owning structs do the work.

Since `recreate_swapchain` now takes `&mut VulkanContext` (§1.5), the
allocator is available as `&mut ctx.allocator`. The full resize flow in
`Renderer::recreate_swapchain` is:

```rust
fn recreate_swapchain(&mut self, ctx: &mut VulkanContext) {
    unsafe { ctx.device.device_wait_idle().unwrap() };
    // Destroy old swapchain resources (depth, framebuffers, etc.)
    cleanup_swapchain(&ctx.device, &mut self.swapchain, &mut ctx.allocator);
    // Destroy old postprocess resources (returns allocations to the pool)
    if let Some(mut old_pp) = self.postprocess.take() {
        old_pp.destroy(&ctx.device, &mut ctx.allocator);
    }
    // Create new swapchain + postprocess resources (reuses freed memory)
    self.swapchain = create_swapchain(&ctx.device, &mut ctx.allocator, ...);
    self.postprocess = Some(PostProcessResources::new(ctx, ...));
}
```

---

## 8. Phase 7 — `postprocess/pyramid.rs::create_mip_image`

The two images (mip + temp) are each 8-mip pyramids of HDR-16 color. Both
are recreated on swapchain resize, so they churn the sub-allocator — the
same situation as the depth image. Use `AllocationScheme::DedicatedImage(image)`
for both to isolate the churn from the main pool. This costs two `VkDeviceMemory`
handles total and is the simpler choice. Order: `create_image → get_memory_requirements → allocate(DedicatedImage(image)) → bind_image_memory`.

`BloomPyramid` struct changes from:
```rust
pub struct BloomPyramid {
    pub mip_views: Vec<vk::ImageView>,
    pub temp_views: Vec<vk::ImageView>,
    mip_image: vk::Image,
    mip_memory: vk::DeviceMemory,       // removed
    temp_image: vk::Image,
    temp_memory: vk::DeviceMemory,      // removed
    pub sampler: vk::Sampler,
}
```

To:
```rust
pub struct BloomPyramid {
    pub mip_views: Vec<vk::ImageView>,
    pub temp_views: Vec<vk::ImageView>,
    pub mip: OwnedImage,                // owns image + allocation
    pub temp: OwnedImage,               // owns image + allocation
    pub sampler: vk::Sampler,
}
```

`BloomPyramid::destroy(&self, device)` becomes `destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator)`.
It calls `self.mip.destroy(device, allocator)`, `self.temp.destroy(device, allocator)`,
then destroys views and sampler.

In this phase, `find_memory_type` (which is now dead code in all callers) is
deleted from `buffer.rs`.

---

## 9. Drop-order refactor

After every phase, the `App::drop` ordering must remain:

1. `Renderer::destroy(device, allocator)` is called explicitly — frees all
   `Allocation`s through the allocator, destroys all Vulkan objects.
2. `ManuallyDrop::drop(&mut self.renderer)` — runs Renderer's `Drop` (which
   is either empty or a debug-build-only `log::warn!` guard).
3. `ManuallyDrop::drop(&mut self.ctx)` — runs `VulkanContext`'s `Drop`, which
   destroys the allocator (freeing any remaining backing blocks — should be
   zero), then destroys the device, then the instance.
4. `Arc<Window>` is released last.

### 9.1 Why Renderer does NOT use `Drop`

`Renderer::destroy` requires `&mut MemoryAllocator` to call
`allocator.free()` on every `Allocation`. `Drop::drop(&mut self)` has no
mechanism to access the allocator. All alternatives have fatal flaws:

| Approach | Why it doesn't work |
|---|---|
| Store `&'a MemoryAllocator` in Renderer | Immutable — can't call `allocator.free()` which takes `&mut` |
| Store `&'a mut MemoryAllocator` | Makes Renderer non-`Clone`, conflicts with `Renderer::new` borrows |
| `Rc<RefCell<MemoryAllocator>>` | Runtime borrow checking, obscures lifetime contract, can panic at runtime |
| Store `Allocator` in Renderer (option B from original §1.3) | Allocator is device-level, not renderer-level; swapchain resize needs it too |

The explicit `destroy(device, allocator)` pattern is the same pattern used by
`OwnedBuffer`, `OwnedImage`, `Texture`, `Cubemap`, `BloomPyramid`, and
`PostProcessResources`. Consistency across the codebase is a feature.

### 9.2 `App::drop` implementation

```rust
impl Drop for App {
    fn drop(&mut self) {
        // 1. Explicitly destroy all renderer resources (requires allocator)
        self.renderer.destroy(&self.ctx.device, &mut self.ctx.allocator);
        // 2. Drop the Renderer struct (its Drop is empty or a debug guard)
        unsafe { ManuallyDrop::drop(&mut self.renderer); }
        // 3. Drop VulkanContext (allocator → device → instance)
        unsafe { ManuallyDrop::drop(&mut self.ctx); }
    }
}
```

### 9.3 `VulkanContext` field ordering for Drop

The `MemoryAllocator` is a field of `VulkanContext`. Rust drops struct fields
in declaration order. The allocator must be dropped **before** the device,
because `Allocator::Drop` calls `vkFreeMemory` which requires a live device.

**The allocator field must be declared BEFORE the `device` field** in the
`VulkanContext` struct so that Rust's automatic drop order destroys the
allocator first. The existing `impl Drop for VulkanContext` in
`src/vulkan/context.rs` does manual cleanup (debug messenger → surface →
device → instance). It must be updated to also drop the allocator explicitly
before destroying the device:

```rust
impl Drop for VulkanContext {
    fn drop(&mut self) {
        unsafe {
            if let Some(ref du) = self.debug_utils {
                du.loader.destroy_debug_utils_messenger(du.messenger, None);
            }
            self.surface_loader.destroy_surface(self.surface, None);
            // Drop the allocator before the device — this frees any remaining
            // VkDeviceMemory blocks (should be zero if destroy was called correctly).
            // ManuallyDrop is not needed: the allocator is a regular field that
            // drops when we drop `self.allocator` via ptr::drop_in_place or
            // by reordering the Drop logic.
            drop(std::mem::take(&mut self.allocator));  // Option wrapping or explicit drop
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
```

Alternatively, wrap the allocator in `ManuallyDrop` and explicitly drop it:

```rust
pub struct VulkanContext {
    // ... other fields ...
    pub allocator: ManuallyDrop<MemoryAllocator>,  // dropped before device
    pub device: ash::Device,
    // ... other fields ...
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        unsafe {
            // ... debug utils, surface ...
            ManuallyDrop::drop(&mut self.allocator);  // free all blocks
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}
```

The `ManuallyDrop` approach is preferred because it makes the ordering
explicit and auditable, matching the existing pattern in `App`.

### 9.4 Safety net: `Allocator::Drop` behavior

If `Renderer::destroy` is called correctly, all `Allocation`s are freed before
the allocator drops. If a bug causes some `Allocation`s to be leaked (not
freed), the allocator's `Drop` frees the underlying `VkDeviceMemory` blocks.
The `vk::Buffer`/`vk::Image` handles bound to that memory become dangling,
but since `device.destroy_device(None)` destroys all child objects anyway,
this is a "double-free at worst" scenario that Vulkan validation layers will
catch. The allocator's `Drop` is a safety net, not a correctness path.

### 9.5 The manual `device.device_wait_idle()` guarantee

The current `device_wait_idle()` at the top of `Renderer::destroy` (carried
over from the old `Drop` impl) guarantees the GPU is idle before any
destroy/free runs, so a frame-in-flight `vkFreeMemory` violation is not a
concern. This must be preserved — it is the first thing `Renderer::destroy`
does.

---

## 10. Debug naming (RenderDoc / validation)

`gpu-allocator` does not name `vk::DeviceMemory` for you. The project
already uses `VK_EXT_debug_utils` for the `vk::Buffer` / `vk::Image` /
`vk::DeviceMemory` handles, so add the memory name through the project's
existing `DebugMarker::set_object_name` channel. The name comes from the
`name: &str` in the `AllocationCreateDesc`.

**Two levels of naming:**

1. **Per-allocation names** (the `name` field in `AllocationCreateDesc`):
   These are consumed by the allocator for its own leak-reporting. They are
   NOT applied to `vk::DeviceMemory` handles. Apply the name to the
   `vk::Buffer` / `vk::Image` handle via `DebugMarker::set_object_name`
   inside `MemoryAllocator::create_buffer` / `create_image`, right after
   creation and binding.

2. **Per-block `vk::DeviceMemory` names** (the backing memory of
   sub-allocated blocks): These are created internally by the allocator and
   are not directly accessible. Do not attempt to name them — RenderDoc's
   "Vulkan Memory" view groups by `VkDeviceMemory` already, and the
   `vk::Buffer`/`vk::Image` names on the resources that use the block are
   sufficient for debugging. Dedicated allocations (depth, bloom) get their
   own `VkDeviceMemory`, so their resource names effectively name the block.

Validate the naming by capturing one frame in RenderDoc before and after the
refactor and checking the "Memory" tab has a sensible count of `VkDeviceMemory`
handles (target: ~6–10 instead of ~30+) and that all resources are named.

---

## 11. Common pitfalls and mitigations

| # | Pitfall | Symptom | Mitigation |
|---|---|---|---|
| 1 | `Allocator` dropped after `vk::Device` | Validation: "device lost" or segfault at shutdown | Put the allocator in `VulkanContext` (option A in §1.3). |
| 2 | `Allocation` freed while its `vk::Buffer`/`vk::Image` is alive | Validation: "cannot free memory with objects bound" or segfault | `OwnedBuffer::destroy` and `OwnedImage::destroy` call `vkDestroy*` first, then `allocator.free(allocation)`. Both must be called explicitly before the allocator is dropped. |
| 3 | `vk::Buffer` / `vk::Image` recreated by `gpu_allocator` after an OOM | Panics / `unwrap` in `allocate` | Use `?` or `.unwrap_or_else(|e| log_and_recover(e))`; for the project, propagate and surface a single line via `eprintln!` before the process exits. |
| 4 | `vk::Image` sub-allocated but image needs `LAZILY_ALLOCATED` (presentable) | Validation: image-memory-type incompatible | The project's images are color/depth, never presentable; the `PRESENT` flag is on the swapchain images which are owned by Vulkan, not us. |
| 5 | `vk::Buffer` with `SHADER_DEVICE_ADDRESS` requires a specific memory type | The new allocator field `buffer_device_address: true` plus `MemoryLocation::GpuOnly` | The project does not use `SHADER_DEVICE_ADDRESS`. Leave the flag `false` in `AllocatorCreateDesc`. |
| 6 | `vk::Image` linear vs optimal tiling | `AllocationCreateDesc::linear` only applies to buffers; the allocator checks requirements | For images, leave `linear` to whatever the caller set (project code is always `optimal`). |
| 7 | `vk::Image` shared between sub-allocated blocks has a non-zero `allocation.offset()` | Bind succeeds (offset is honored) but the user uses `image` as if the offset were 0 | The allocator handles this; user never touches offset directly. |
| 8 | `vkMapMemory` per-frame for UBOs (current code does once at startup) | Spurious validation: "memory is already mapped" | `Allocation::mapped_ptr()` returns a pointer already at the correct sub-allocation offset. The UBO is mapped once in `create_host_mapped_ubo`; the `OwnedBuffer.mapped` field stores it. The pointer is invalidated when `allocator.free(allocation)` runs in `destroy`. |
| 9 | Sub-allocator grows the HOST_VISIBLE block forever when a buffer is destroyed and a slightly larger one is allocated immediately | Visible as growing RSS; not a crash | Use the `AllocationSizes` field on `AllocatorCreateDesc` to set per-type block growth factors; or call `allocator.generate_report()` periodically to see block sizes. |
| 10 | `vk::Image` re-bind after `vkResetCommandPool` (not actually a problem, but easy to suspect) | n/a | The allocator binds once at allocation; `vkResetCommandPool` does not touch image bindings. |
| 11 | Memory type selection differs from `find_memory_type` (e.g. the project picked type 5, the allocator picks type 7) | Visual: identical, but RenderDoc memory view looks different | This is expected. The allocator is free to use any compatible memory type. Validation layers verify compatibility. |
| 12 | Dedicated allocation for a small image wastes a `VkDeviceMemory` | None functionally, but a developer reading the memory view sees N extra handles | Only use `Dedicated` for images >~1 MB or ones recreated on resize. |
| 13 | `vk::Buffer` with `usage = SHADER_DEVICE_ADDRESS` not bound to `DEVICE_LOCAL` | Validation: "Buffer with VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT must be bound to device-local memory" | The project does not use this usage. If a future buffer does, document it in the calling site. |
| 14 | `vk::Image` with `usage = COLOR_ATTACHMENT` lazily sub-allocated to a `LAZILY_ALLOCATED` block | Validation: image must be in DEVICE_LOCAL for color attachment | The allocator respects `requirements`; the project's `requirements.memory_type_bits` always include DEVICE_LOCAL. |
| 15 | First-frame allocation storm (every buffer + every image at startup) | One-time hiccup | Acceptable. If it shows in profile, mark the slow path with a debug label. |
| 16 | `vkDestroyImage` called twice (once by the project's old `destroy` path, once by the new `destroy`) | Validation: "object has already been destroyed" | Delete every call site of the old `destroy` method in the same commit as the rename to `OwnedImage::destroy`. The new method takes `(device, allocator)` — the old one took only `(device)` — so the compiler catches mismatches. |
| 17 | `vkFreeMemory` called with a memory that is not `Allocation::memory()` (e.g. confusion after a swapchain resize) | Validation: "invalid memory handle" | The `Allocation` struct is the source of truth for the `vk::DeviceMemory` handle. The owning struct stores it; `destroy` calls `allocator.free`. |
| 18 | `Renderer` implements `Drop` but needs `&mut MemoryAllocator` for cleanup | Won't compile / can't access allocator | **Renderer must NOT implement `Drop`.** Use explicit `Renderer::destroy(device, allocator)` called from `App::drop`. See §1.4 and §9. |
| 19 | `device.unmap_memory()` called on a sub-allocated staging buffer | Validation: "memory is not mapped" or unmaps the entire backing block, breaking co-located sub-allocations | Remove all explicit `device.unmap_memory()` calls when converting to the allocator. The allocator handles mapping internally via `Allocation::mapped_ptr()`; unmapping happens when `allocator.free(allocation)` is called. |
| 20 | `AllocationScheme::DedicatedBuffer` used unnecessarily | Wastes a `VkDeviceMemory` handle per buffer | The project never uses `DedicatedBuffer`. All buffers are small enough for sub-allocation. Only `DedicatedImage` is used for large or resize-churning images (depth, bloom mip/temp, env/prefilter cubemaps). |
| 21 | `&mut VulkanContext` borrow conflicts during `Renderer::new` | Won't compile: `App` borrows `ctx` mutably for `Renderer::new`, but `ctx.device` is also needed immutably inside | The allocator is a field of `VulkanContext`, so `&mut ctx` gives mutable access to both the allocator and the device. Inside `Renderer::new`, use `&ctx.device` (reborrow from `&mut ctx`) for read-only device operations and `&mut ctx.allocator` for allocation. Rust allows reborrowing: `&ctx.device` reborrows immutably from the `&mut ctx`, which is sound because the device and allocator are separate fields. |

---

## 12. Test cases

The primary test gate is the `--run-frames` CLI flag (§12.3), which runs the
binary with validation layers and checks for clean shutdown. The `#[ignore]`
unit tests (§12.1) are supplementary — they require a GPU and are not run in
CI. The integration tests (§12.2) are deferred; the `--run-frames` approach
is more reliable on Windows.

### 12.1 Unit tests (in `src/vulkan/memory.rs`)

These tests require a GPU and are marked `#[ignore]`. Run with
`cargo test -- --ignored`. They verify allocator behavior in isolation.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::context::VulkanContext;

    // Helper: build a VulkanContext with validation. Marked `#[ignore]` so
    // `cargo test` does not require a GPU; run with `cargo test -- --ignored`.
    fn make_ctx() -> VulkanContext {
        // You will need a headless surface (e.g. a 1×1 hidden window) or
        // a null surface. The `ash-window` crate can do this with a
        // `RawDisplayHandle` from a `winit::event_loop::EventLoop`. For
        // unit-test ergonomics, an existing context can be created lazily
        // by gating on `std::env::var("LEARN_VK_TEST_GPU").is_ok()`.
        todo!("Construct a headless VulkanContext; see §12.3")
    }

    #[test]
    #[ignore] // GPU required
    fn allocator_creates_and_drops() {
        let mut ctx = make_ctx();
        // Use the allocator that's already a field of VulkanContext
        let mut buf = ctx.allocator.create_buffer(
            &ctx.device, "smoke", 64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            MemoryLocation::CpuToGpu,
        );
        buf.destroy(&ctx.device, &mut ctx.allocator);
        // (VulkanContext drops here, destroying the allocator before the device.)
    }

    #[test]
    #[ignore]
    fn sub_allocation_does_not_grow_block_per_request() {
        let mut ctx = make_ctx();
        // Allocate 10 small HOST_VISIBLE buffers. Expect 1 distinct
        // VkDeviceMemory (block) backing all 10.
        let mut bufs: Vec<OwnedBuffer> = (0..10)
            .map(|i| ctx.allocator.create_buffer(
                &ctx.device, &format!("h{}", i), 64,
                vk::BufferUsageFlags::STORAGE_BUFFER,
                MemoryLocation::CpuToGpu,
            ))
            .collect();
        let first_memory = bufs[0].allocation.as_ref().unwrap().memory();
        for b in &bufs[1..] {
            assert_eq!(
                b.allocation.as_ref().unwrap().memory(),
                first_memory,
                "expected sub-allocation into a single VkDeviceMemory"
            );
        }
        for mut b in bufs.drain(..) { b.destroy(&ctx.device, &mut ctx.allocator); }
    }

    #[test]
    #[ignore]
    fn dedicated_image_uses_own_block() {
        let mut ctx = make_ctx();
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::R16G16B16A16_SFLOAT)
            .extent(vk::Extent3D { width: 1024, height: 1024, depth: 1 })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut img = ctx.allocator.create_dedicated_image(
            &ctx.device, "dedicated", &info,
        );
        // A dedicated allocation's offset is 0 and size == memory.size
        let a = img.allocation.as_ref().unwrap();
        assert_eq!(a.offset(), 0);
        assert!(a.size() >= 1024 * 1024 * 8);
        img.destroy(&ctx.device, &mut ctx.allocator);
    }
}
```

**The negative test `drop_after_device_panics_cleanly` is removed.**
Dropping the allocator before freeing its allocations causes the allocator's
`Drop` to free `VkDeviceMemory` while `vk::Buffer`/`vk::Image` handles are
still bound — this triggers validation errors and is undefined behavior. It
is not a valid test scenario. The ordering invariant (§9) is enforced by the
`App::drop` implementation and the `Renderer::destroy` explicit call pattern.

### 12.2 Integration tests — deferred

Integration tests that drive the full rendering loop from `#[test]` functions
are impractical on Windows with winit 0.30 (no headless surface). The
`--run-frames` CLI flag (§12.3) provides the same coverage more reliably.
If headless rendering becomes feasible in the future, add tests for:
- 60-frame clean shutdown
- 5 swapchain resizes without block leaks (check `allocator.generate_report()`)
- `VkDeviceMemory` count < 16 after warmup

### 12.3 Test ergonomics: `--run-frames` CLI flag

A "headless" `VulkanContext` is awkward on Windows. Pragmatic
approach: add a CLI flag to the existing binary:

```rust
// src/main.rs — parse in main() before event loop construction
struct RunConfig {
    run_frames: Option<u32>,  // Some(N) => exit after N frames, write sentinel
    // ... existing fields (resolution, validation, gpu_assisted)
}
```

The `AppHandler` (which implements `winit::application::ApplicationHandler`)
gains a `run_frames: Option<u32>` field. In `window_event` (specifically
`RedrawRequested`), after rendering, decrement the counter. When it reaches 0:
- Write a sentinel line to stdout (`"RUN_FRAMES_DONE"`)
- Call `event_loop.exit()` to stop the event loop

The current `main.rs` already tracks `enable_validation`, `enable_gpu_assisted`,
and `width`/`height` in the `AppHandler`. Adding a `run_frames` counter is
a straightforward extension. The existing `ControlFlow::Poll` ensures the
event loop processes `RedrawRequested` events continuously.

The CI / pre-push hook then runs:

```bash
cargo build
cargo run --release -- --validation --run-frames --frames=120
```

A clean exit code, no validation errors in stderr, and a sentinel stdout
line confirm a working integration. This is the project's existing pattern
for "build + run + check," and is the most reliable gate.

### 12.4 Manual RenderDoc capture

- Capture one frame. In the "Memory" tab, count the `VkDeviceMemory`
  handles.
- Pre-refactor baseline: ~30+ handles.
- Post-refactor target: ~6–10 handles, dominated by
  - 1× HOST_VISIBLE (the 4 UBO sub-allocations)
  - 4–6× DEVICE_LOCAL (one per texture format family, IBL cubemaps,
    scene color, skybox buffers, scene VB/IB, material buffer)
  - 2–3× DEVICE_LOCAL dedicated (depth, bloom mip, bloom temp)

A frame that fails to match this profile is a regression.

---

## 13. Verification checklist (post-refactor)

Run these in order. Each is a binary signal (pass/fail) that catches a
specific class of regression.

1. `cargo build` succeeds with zero new warnings.
2. `cargo test` (no `--ignored`) is green (compile-time unit tests only).
3. `cargo run -- --validation --run-frames --frames=120` exits 0 with
   no validation errors in stderr.
4. `cargo run --release -- --validation --run-frames --frames=600` exits 0
   after 10 seconds of rendering (sustained stability).
5. Resize the window 5 times during a run; visual output stays correct.
6. Press `T` to cycle tonemap; visual output updates immediately.
7. Capture a frame in RenderDoc; confirm:
   - 1× first frame in flight
   - Bloom chain produces visible highlights
   - "Memory" tab shows the expected `VkDeviceMemory` count
8. `cargo run -- --gpu-assisted --validation` (or `--vgav`) does not
   surface any new gpu-assisted errors that pre-refactor did not also
   surface.
9. `cargo clippy --all-targets` is clean.
10. `git diff` shows: `Cargo.toml` (one line), one new file (`memory.rs`),
    every other modified file changes only allocation / destroy sites
    (the rest of the code is byte-identical to before).

---

## 14. File-by-file diff summary

| File | Change |
|---|---|
| `Cargo.toml` | +1 line: `gpu-allocator = "0.28"` |
| `src/vulkan/memory.rs` | New file. Owning structs `OwnedBuffer` / `OwnedImage` (no `Drop`; explicit `destroy(device, allocator)`), thin newtype `MemoryAllocator`. Image helper: `create_image` (takes `vk::ImageCreateInfo`), `create_dedicated_image`. |
| `src/vulkan/mod.rs` | +1 line: `pub mod memory;` |
| `src/vulkan/context.rs` | +1 field in `VulkanContext`: `pub allocator: ManuallyDrop<MemoryAllocator>`. Constructor builds it after the device. `Drop` ordering updated to drop allocator before device (see §9.3). |
| `src/vulkan/buffer.rs` | `GpuBuffer` removed. `find_memory_type` left as dead code until Phase 7, then deleted. `create_buffer` rewritten to pass allocator. `create_device_local_buffer` and `with_one_time_command` rewritten to use allocator. |
| `src/vulkan/texture.rs` | `Texture` struct becomes `image, allocation: Option<Allocation>, view, sampler`. `from_rgba8_with_format` uses the allocator. `destroy(&Device)` becomes `destroy(&mut self, device, allocator)`. |
| `src/vulkan/cubemap.rs` | `Cubemap::create_empty` takes `&mut MemoryAllocator`. Large cubemaps get `AllocationScheme::DedicatedImage(image)`. `destroy` method updated. |
| `src/vulkan/brdf_lut.rs` | Inline memory allocation block replaced by `ctx.allocator.create_image(...)`. `BrdfLut::destroy` updated. |
| `src/vulkan/ktx2_loader.rs` | Per-mip staging buffer uses `ctx.allocator.create_buffer(... CpuToGpu)` with `allocation.mapped_ptr()`. Destroyed explicitly at end of each loop iteration. |
| `src/vulkan/swapchain.rs` | Depth image becomes `OwnedImage` with `DedicatedImage(image)` scheme. Inline memory-type loop removed. `create_swapchain` signature gains `&mut MemoryAllocator`. |
| `src/vulkan/ibl.rs` | No change to its own code; benefits transitively from cubemap + brdf_lut changes. |
| `src/vulkan/postprocess/pyramid.rs` | `BloomPyramid` holds `mip: OwnedImage` and `temp: OwnedImage` with `DedicatedImage` scheme. `destroy` takes `allocator` parameter. |
| `src/vulkan/postprocess/resources.rs` | Scene color images use `OwnedImage` (managed allocation). Postprocess UBO uses `create_host_mapped_ubo`. `destroy` method takes `allocator` parameter. |
| `src/vulkan/postprocess/descriptors.rs` | No change. |
| `src/vulkan/postprocess/fullscreen.rs` | No change. |
| `src/vulkan/postprocess/passes.rs` | No change. |
| `src/vulkan/postprocess/ubo.rs` | No change. |
| `src/vulkan/pbr_ubo.rs` | No change. |
| `src/vulkan/descriptors.rs` | No change. |
| `src/vulkan/pipeline.rs` | No change. |
| `src/vulkan/debug_marker.rs` | No change. |
| `src/vulkan/renderer.rs` | Global UBO loop uses `ctx.allocator`. `Drop for Renderer` is **removed**; replaced by explicit `Renderer::destroy(device, allocator)` method called from `App::drop`. `name_debug_objects` keeps existing names. All resource cleanup moves from `Drop` to `destroy`. `recreate_swapchain` signature changes to take `&mut VulkanContext`. |
| `src/scene/gltf_loader.rs` | `create_device_local_buffer` calls stay; the staging buffer inside them is now allocator-driven. Fallback textures are allocator-driven. |
| `src/scene/model.rs` | No change. |
| `src/scene/material.rs` | No change. |
| `src/scene/scene_graph.rs` | No change. |
| `src/app.rs` | `App::drop` rewritten: calls `self.renderer.destroy(&self.ctx.device, &mut self.ctx.allocator)` before `ManuallyDrop::drop(&mut self.renderer)`. The `ManuallyDrop` chain ordering is preserved but the Renderer `Drop` is replaced by the explicit `destroy` call. |
| `src/main.rs` | +1 CLI flag (`--run-frames` / `--frames=N`) for the test harness in §12.3, threaded through `AppHandler` to auto-exit after N frames. |

The diff touches every file that has an `allocate_memory` call (7 files)
and adds one new module. Total expected diff: ~600 lines of new code in
`memory.rs` plus ~400 lines of edits across the 7 call-site files.

---

## 15. Risks of NOT testing all of the above

- **Silent device memory leak in the resize path.** If the old
  `PostProcessResources` is not fully destroyed before the new one is
  constructed, the old `Allocation`s do not return to the pool, and
  repeated resizes grow the DEVICE_LOCAL block until OOM. The `cargo run
  --run-frames --frames=120` test is the cheapest catch.
- **Frame-in-flight `vkFreeMemory` violation** if a buffer is destroyed
  before the GPU finishes using it. The current code already
  `device_wait_idle`s in `Renderer::destroy` and in `recreate_swapchain`,
  so this is preserved. A missed `device_wait_idle` would surface as
  validation errors in the `--validation --run-frames` test.
- **`vk::Image` / `vk::Buffer` ordering inversion at destroy time.** The
  `OwnedBuffer::destroy` / `OwnedImage::destroy` methods destroy the Vulkan
  resource first, then call `allocator.free(allocation)`. A flipped order
  triggers validation "cannot free memory with bindings." The unit test in
  §12.1 catches this.

---

## 16. End-to-end ordering for the implementer

1. Add `gpu-allocator = "0.28"` to `Cargo.toml`; `cargo build` should still succeed
   (the crate is unused).
2. Create `src/vulkan/memory.rs` with the newtype + `OwnedBuffer` / `OwnedImage`
   (with explicit `destroy(device, allocator)`, no `Drop`). No call sites changed yet.
3. Wire `MemoryAllocator` into `VulkanContext` as `ManuallyDrop<MemoryAllocator>`.
   Update `VulkanContext::Drop` to drop the allocator before the device (§9.3).
   `cargo build` should still succeed (the field is unused).
4. **Replace `Drop for Renderer` with `Renderer::destroy(device, allocator)`.**
   Update `App::drop` to call `renderer.destroy(&ctx.device, &mut ctx.allocator)`
   before `ManuallyDrop::drop(&mut self.renderer)`. The body of `destroy` is
   identical to the old `Drop` impl for now (no allocator calls yet).
   `cargo build` + `cargo run` should work identically to before. Commit.
5. Refactor `buffer.rs` to use the allocator. Keep `find_memory_type` with
   `#[allow(dead_code)]`. Update `create_buffer`, `create_device_local_buffer`.
   Update `Renderer::destroy` to pass `allocator` to `OwnedBuffer::destroy`.
   Run the §12.3 harness; capture RenderDoc; commit.
6. Refactor `texture.rs`. Update `Texture::destroy` to take `allocator`.
   Update `gltf_loader.rs` fallback textures. Run the harness; commit.
7. Refactor `cubemap.rs`, `brdf_lut.rs`, `ktx2_loader.rs`. Remove explicit
   `device.unmap_memory()` calls from `ktx2_loader.rs` (pitfall #19).
   Run the harness; commit.
8. Refactor `swapchain.rs`. Change `create_swapchain` signature to take
   `&mut MemoryAllocator`. Update `recreate_swapchain` to take `&mut VulkanContext`.
   The depth image's inline memory-type loop is replaced by the allocator.
   Run the harness; commit.
9. Refactor `postprocess/resources.rs` and `postprocess/pyramid.rs`. In this
   phase, also delete `find_memory_type` from `buffer.rs` (it is now dead
   code in all callers). Run the harness; commit.
10. Run `cargo run -- --gpu-assisted --validation --run-frames --frames=120`
    to verify no new GPU-assisted validation errors. Commit if clean.
11. Add the `#[ignore]` unit tests in `memory.rs` to the
    `cargo test -- --ignored` suite. Run on the dev box; commit.
12. `cargo clippy --all-targets`. Commit any unrelated cleanups (none
    expected, but `clippy` sometimes catches a borrow that was previously
    hidden by the `GpuBuffer` indirection).
13. Update `CODEBUDDY.md` with a one-paragraph note in the
    "Important Patterns" section: "Memory allocation is centralized in
    `src/vulkan/memory.rs`; new code MUST go through `MemoryAllocator`
    instead of `vkAllocateMemory` directly. Owned resources use explicit
    `destroy(device, allocator)` — do not implement `Drop` on owning structs
    because the allocator requires `&mut` access for freeing, and the
    allocator must outlive all allocations. `Renderer` uses the same pattern:
    no `Drop`, explicit `destroy(device, allocator)` called from `App::drop`."

That order keeps every commit individually runnable and revertable.
