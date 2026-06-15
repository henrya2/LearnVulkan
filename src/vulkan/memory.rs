//! Centralized memory allocation via the `gpu-allocator` crate.
//!
//! This module replaces every direct `vkAllocateMemory` / `vkBindBufferMemory` /
//! `vkBindImageMemory` pair in the project with [`MemoryAllocator`].
//!
//! # Drop-order contract
//!
//! 1. The owning struct (`OwnedBuffer`, `OwnedImage`, `Texture`, `Cubemap`,
//!    `BrdfLut`, `BloomPyramid`, `PostProcessResources`, `Renderer`) calls
//!    `destroy(&ash::Device, &mut MemoryAllocator)` to destroy its
//!    `vk::Buffer`/`vk::Image` and free the underlying `Allocation`.
//! 2. `App::drop` calls `Renderer::destroy(...)` explicitly before
//!    `ManuallyDrop::drop` on the renderer.
//! 3. `VulkanContext::Drop` drops the `MemoryAllocator` *before* the
//!    `vk::Device`.
//!
//! `Allocator::Drop` is a safety net: it frees any still-live `VkDeviceMemory`
//! blocks (should be zero after `destroy`). It does NOT free `Allocation`s.
//! `Allocation` has no `Drop` — it must be freed via `allocator.free(...)`.
//!
//! # Why no `Drop` on owning structs
//!
//! `Drop::drop(&mut self)` cannot access the `MemoryAllocator` (no context
//! available). All resources that own `Allocation`s therefore implement
//! explicit `destroy(&mut self, device, allocator)` methods, called from
//! `App::drop`. The previous `Drop for Renderer` is replaced by the same
//! pattern.

use ash::vk;
use gpu_allocator::MemoryLocation;
use gpu_allocator::vulkan::{
    Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc,
};

/// Newtype wrapper around [`gpu_allocator::vulkan::Allocator`].
///
/// The newtype exists so the project's debug-naming story is uniform and
/// `&mut MemoryAllocator` is the single handle for every allocation site.
/// Does not implement `Drop` directly — its inner `Allocator` drops when
/// `ManuallyDrop<MemoryAllocator>` is explicitly dropped in
/// `VulkanContext::Drop`.
pub struct MemoryAllocator {
    pub inner: Allocator,
}

impl MemoryAllocator {
    /// Construct a new allocator bound to `device`.
    pub fn new(instance: ash::Instance, device: ash::Device, physical_device: vk::PhysicalDevice) -> Self {
        let inner = Allocator::new(&AllocatorCreateDesc {
            instance,
            device,
            physical_device,
            // `AllocatorDebugSettings` is `#[non_exhaustive]` — must use `Default`.
            debug_settings: Default::default(),
            buffer_device_address: false, // project does not use BDA
            allocation_sizes: Default::default(),
        })
        .expect("Failed to create gpu_allocator::Allocator");
        Self { inner }
    }

    // ---------------------------------------------------------------------
    // Buffer allocation
    // ---------------------------------------------------------------------

    /// Create a `vk::Buffer` plus its backing `Allocation` for the given
    /// `MemoryLocation`. Returns `OwnedBuffer` — call `destroy(device, allocator)`
    /// to free (the buffer is destroyed first, then `allocator.free(allocation)`).
    pub fn create_buffer(
        &mut self,
        device: &ash::Device,
        name: &str,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        location: MemoryLocation,
    ) -> OwnedBuffer {
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { device.create_buffer(&info, None) }
            .expect("vkCreateBuffer failed");
        let reqs = unsafe { device.get_buffer_memory_requirements(buffer) };
        let allocation = self
            .inner
            .allocate(&AllocationCreateDesc {
                name,
                requirements: reqs,
                location,
                linear: true, // buffers are linear-tiled
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })
            .expect("gpu-allocator failed to allocate buffer memory");
        unsafe {
            device
                .bind_buffer_memory(buffer, allocation.memory(), allocation.offset())
                .expect("vkBindBufferMemory failed");
        }
        OwnedBuffer {
            buffer,
            allocation: Some(allocation),
            size,
            mapped: None,
        }
    }

    /// Create a `HOST_VISIBLE | HOST_COHERENT` UBO buffer and return it alongside
    /// a persistently-mapped `*mut u8` pointer already at the correct
    /// sub-allocation offset. The pointer is valid until `destroy` is called.
    pub fn create_host_mapped_ubo(
        &mut self,
        device: &ash::Device,
        name: &str,
        size: vk::DeviceSize,
    ) -> OwnedBuffer {
        let mut buf = self.create_buffer(
            device,
            name,
            size,
            vk::BufferUsageFlags::UNIFORM_BUFFER,
            MemoryLocation::CpuToGpu,
        );
        // Resolve the mapped pointer (panic if the allocator couldn't map).
        let mapped = buf
            .allocation
            .as_ref()
            .expect("create_host_mapped_ubo: allocation missing after create_buffer")
            .mapped_ptr()
            .expect("create_host_mapped_ubo: CpuToGpu allocation not mapped")
            .as_ptr() as *mut u8;
        buf.mapped = Some(mapped);
        buf
    }

    // ---------------------------------------------------------------------
    // Image allocation
    // ---------------------------------------------------------------------

    /// Create a `vk::Image` plus its backing `Allocation` for the given
    /// `MemoryLocation`. Returns `OwnedImage`. The image's metadata
    /// (`extent`, `format`, `mip_levels`) is cached for downstream use.
    pub fn create_image(
        &mut self,
        device: &ash::Device,
        name: &str,
        image_info: &vk::ImageCreateInfo,
        location: MemoryLocation,
    ) -> OwnedImage {
        let image = unsafe { device.create_image(image_info, None) }
            .expect("vkCreateImage failed");
        let reqs = unsafe { device.get_image_memory_requirements(image) };
        let allocation = self
            .inner
            .allocate(&AllocationCreateDesc {
                name,
                requirements: reqs,
                location,
                linear: false, // images are optimal-tiled
                allocation_scheme: AllocationScheme::GpuAllocatorManaged,
            })
            .expect("gpu-allocator failed to allocate image memory");
        unsafe {
            device
                .bind_image_memory(image, allocation.memory(), allocation.offset())
                .expect("vkBindImageMemory failed");
        }
        OwnedImage {
            image,
            allocation: Some(allocation),
            extent: image_info.extent,
            format: image_info.format,
            mip_levels: image_info.mip_levels,
        }
    }

    /// Create an image whose backing memory is dedicated (one `VkDeviceMemory`
    /// per image). Use for large or resize-churning images (depth, bloom
    /// mip/temp, env/prefilter cubemaps) so the sub-allocator pool is not
    /// affected by them.
    pub fn create_dedicated_image(
        &mut self,
        device: &ash::Device,
        name: &str,
        image_info: &vk::ImageCreateInfo,
    ) -> OwnedImage {
        let image = unsafe { device.create_image(image_info, None) }
            .expect("vkCreateImage failed");
        let reqs = unsafe { device.get_image_memory_requirements(image) };
        let allocation = self
            .inner
            .allocate(&AllocationCreateDesc {
                name,
                requirements: reqs,
                location: MemoryLocation::GpuOnly,
                linear: false,
                allocation_scheme: AllocationScheme::DedicatedImage(image),
            })
            .expect("gpu-allocator failed to allocate dedicated image memory");
        unsafe {
            device
                .bind_image_memory(image, allocation.memory(), allocation.offset())
                .expect("vkBindImageMemory failed");
        }
        OwnedImage {
            image,
            allocation: Some(allocation),
            extent: image_info.extent,
            format: image_info.format,
            mip_levels: image_info.mip_levels,
        }
    }
}

// ---------------------------------------------------------------------------
// OwnedBuffer
// ---------------------------------------------------------------------------

/// Owns a `vk::Buffer` and its backing `Allocation`. No `Drop` impl — call
/// `destroy(device, allocator)` exactly once before the allocator drops.
pub struct OwnedBuffer {
    pub buffer: vk::Buffer,
    pub allocation: Option<Allocation>,
    pub size: vk::DeviceSize,
    /// Persistently-mapped CPU pointer set by [`MemoryAllocator::create_host_mapped_ubo`].
    /// `None` for non-mapped buffers.
    mapped: Option<*mut u8>,
}

// Safety: the *mut u8 is just a pointer; no Send/Sync inferred. Send/Sync for
// OwnedBuffer follows automatically from vk::Buffer and Allocation which are
// both Send/Sync (gpu_allocator::vulkan::Allocation is Send + Sync).
unsafe impl Send for OwnedBuffer {}
unsafe impl Sync for OwnedBuffer {}

impl OwnedBuffer {
    /// Returns the persistently-mapped CPU pointer (already at the correct
    /// sub-allocation offset). Panics if `create_host_mapped_ubo` was not used.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped
            .expect("OwnedBuffer::mapped_ptr called on non-host-mapped buffer")
    }

    /// Destroy the buffer and free its allocation through the allocator.
    /// Must be called exactly once before the allocator is dropped.
    pub fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe { device.destroy_buffer(self.buffer, None) };
        if let Some(allocation) = self.allocation.take() {
            allocator
                .inner
                .free(allocation)
                .expect("Failed to free buffer allocation");
        }
    }
}

// ---------------------------------------------------------------------------
// OwnedImage
// ---------------------------------------------------------------------------

/// Owns a `vk::Image` and its backing `Allocation`. No `Drop` impl — call
/// `destroy(device, allocator)` exactly once before the allocator drops.
pub struct OwnedImage {
    pub image: vk::Image,
    pub allocation: Option<Allocation>,
    pub extent: vk::Extent3D,
    pub format: vk::Format,
    pub mip_levels: u32,
}

unsafe impl Send for OwnedImage {}
unsafe impl Sync for OwnedImage {}

impl OwnedImage {
    /// Destroy the image and free its allocation through the allocator.
    /// Must be called exactly once before the allocator is dropped.
    pub fn destroy(&mut self, device: &ash::Device, allocator: &mut MemoryAllocator) {
        unsafe { device.destroy_image(self.image, None) };
        if let Some(allocation) = self.allocation.take() {
            allocator
                .inner
                .free(allocation)
                .expect("Failed to free image allocation");
        }
    }
}
