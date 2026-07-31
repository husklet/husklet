//! Buffers, memory, images, samplers, instance/device objects, and fences.

use crate::model::instance::Instance;
use crate::model::memory::{
    BufferRec, BufferUsage, Format, ImageRec, ImageUsage, MemRec, SamplerRec,
};
use crate::model::queue::FenceRec;
use crate::*;
use hl_gpu::protocol::model::descriptor::{BufferDesc, SamplerDesc};
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureDim};
use hl_gpu::{BufferId, Cmd, CommandSink, GpuError, Result};

// ---- instance / device (pure object model — no IR) -----------------------------------------------

/// `vkCreateInstance` — build the instance exposing the hl physical device.
impl Instance {
    /// `vkCreateDevice` — build a logical device over this instance's physical device.
    pub fn create_device(&self) -> Device {
        Device::new(self.physical_device.clone())
    }
}

// ---- buffers / device memory ---------------------------------------------------------------------

/// `vkCreateBuffer` — mint an hl-GPU buffer id, translate the usage, and submit [`Cmd::CreateBuffer`].
/// `vk_usage` is a raw `VkBufferUsageFlags` bitset.
pub fn create_buffer(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vk_usage: u32,
    size: u64,
) -> Result<VkBuffer> {
    // Validate the size HERE, the way `allocate_memory` validates its allocation size and
    // `create_image` validates its extent. This path guarded neither bound, so it was the one sibling
    // of the three that answered a caller's mistake with a live handle: a zero-size buffer became a
    // real object whose reported memory requirement was 0 — indistinguishable from the 0 that
    // `vkGetBufferMemoryRequirements` reports for a buffer it has never heard of — and an absurd size
    // was accepted against the 2 GiB ceiling this same driver advertises as `maxBufferSize`, leaving
    // the contradiction to surface much later, somewhere else.
    if size == 0 {
        return Err(GpuError::Invalid(
            "vkCreateBuffer: size must be greater than 0",
        ));
    }
    if size > dev.physical_device.limits.max_buffer_size {
        return Err(GpuError::ResourceLimit(
            "vkCreateBuffer: size exceeds the advertised maxBufferSize",
        ));
    }
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let usage = BufferUsage(vk_usage).wire();
    sink.submit(&[Cmd::CreateBuffer(
        ir_id,
        BufferDesc {
            size,
            usage,
            label: format!("vkbuf{ir_id}"),
        },
    )])?;
    dev.buffers.insert(
        handle,
        BufferRec {
            ir_id,
            size,
            usage,
            bound_mem: None,
            bound_offset: 0,
        },
    );
    Ok(handle)
}

/// `vkDestroyBuffer` — destroy the backing hl-GPU buffer. No-op on `VK_NULL_HANDLE`/unknown handle.
pub fn destroy_buffer(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    buffer: VkBuffer,
) -> Result<()> {
    if let Some(b) = dev.buffers.remove(&buffer) {
        sink.submit(&[Cmd::DestroyBuffer(b.ir_id)])?;
    }
    Ok(())
}

/// `vkAllocateMemory` — allocate `size` bytes of host-visible unified memory (zeroed). No IR: the bytes
/// upload to the host lazily (mapped-flush at `vkQueueSubmit`).
///
/// A zero `allocationSize` is a usage error (VUID-VkMemoryAllocateInfo-allocationSize-00638). A request
/// that cannot be satisfied from the modeled unified heap ([`crate::model::instance::PhysicalDeviceDesc::
/// memory_heap_bytes`]) is a truthful `VK_ERROR_OUT_OF_DEVICE_MEMORY` analogue ([`GpuError::ResourceLimit`])
/// — NEVER a fake success, and (crucially) never a host `Vec` capacity-overflow/OOM abort from
/// `vec![0u8; huge as usize]`: the budget check rejects an over-heap size before any host allocation.
impl Device {
    pub fn allocate_memory(&mut self, size: u64) -> Result<VkDeviceMemory> {
        if size == 0 {
            return Err(GpuError::Invalid(
                "vkAllocateMemory: allocationSize must be greater than 0",
            ));
        }
        if size > self.physical_device.memory_heap_bytes {
            return Err(GpuError::ResourceLimit(
                "vkAllocateMemory: allocation exceeds the device memory heap",
            ));
        }
        let handle = self.alloc_handle();
        self.memories.insert(
            handle,
            MemRec {
                data: vec![0u8; size as usize],
                size,
                bound_buffers: Vec::new(),
                mapped: false,
                pending_flush: None,
            },
        );
        Ok(handle)
    }
}

/// `vkBindBufferMemory` — bind `memory` to `buffer` at `offset`. Errors on an unknown handle. No IR
/// (binding is bookkeeping; the buffer already has its backing hl-GPU id).
pub fn bind_buffer_memory(
    dev: &mut Device,
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    offset: u64,
) -> Result<()> {
    if !dev.memories.contains_key(&memory) {
        return Err(GpuError::Invalid(
            "vkBindBufferMemory: unknown VkDeviceMemory",
        ));
    }
    let b = dev
        .buffers
        .get_mut(&buffer)
        .ok_or(GpuError::Invalid("vkBindBufferMemory: unknown VkBuffer"))?;
    b.bound_mem = Some(memory);
    b.bound_offset = offset;
    // Record this buffer as bound into the allocation (many buffers may share one allocation — the
    // sub-allocating arena pattern; see `MemRec::bound_buffers`). Dedup so a re-bind of the same buffer
    // (legal before first use) does not duplicate its flush.
    let bound = &mut dev.memories.get_mut(&memory).unwrap().bound_buffers;
    if !bound.contains(&buffer) {
        bound.push(buffer);
    }
    Ok(())
}

/// `vkMapMemory` — mark `memory` mapped (its bytes now flush to the host at each submit). Errors on an
/// unknown handle.
impl Device {
    pub fn map_memory(&mut self, memory: VkDeviceMemory) -> Result<()> {
        self.memories
            .get_mut(&memory)
            .ok_or(GpuError::Invalid("vkMapMemory: unknown VkDeviceMemory"))?
            .mapped = true;
        Ok(())
    }
}

/// Write `bytes` into a mapped memory at `offset` (the app's `memcpy` into the mapped pointer). Errors
/// on an unknown/short range.
pub fn write_mapped(
    dev: &mut Device,
    memory: VkDeviceMemory,
    offset: u64,
    bytes: &[u8],
) -> Result<()> {
    let m = dev
        .memories
        .get_mut(&memory)
        .ok_or(GpuError::Invalid("write_mapped: unknown VkDeviceMemory"))?;
    // u64 checked math: a hostile `offset` near `u64::MAX` must be a truthful OutOfBounds, never an
    // `offset as usize + len` add-overflow panic.
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or(GpuError::OutOfBounds)?;
    if end > m.data.len() as u64 {
        return Err(GpuError::OutOfBounds);
    }
    let start = offset as usize;
    m.data[start..start + bytes.len()].copy_from_slice(bytes);
    Ok(())
}

/// The `VkDeviceSize` sentinel meaning "to the end of the allocation" (`VK_WHOLE_SIZE`).
const VK_WHOLE_SIZE: u64 = u64::MAX;

/// Refresh a host-visible mapped memory's staging bytes with the CURRENT device contents of the buffer
/// bound into it, reading them back over the sink — the device→host path, the SAME
/// [`CommandSink::read_buffer`] that serves cuda's `cuMemcpyDtoH` and GL's `glReadPixels`. This is what
/// makes GPU output observable through the mapped pointer: the staging bytes are the app's own last
/// upload, so without a readback a reader sees only its stale writes, never what the device computed.
///
/// `offset`/`size` bound the refreshed region (`size == VK_WHOLE_SIZE` → to the end of the allocation),
/// matching the `vkMapMemory` / `VkMappedMemoryRange` the app requested; only the sub-range that actually
/// overlaps the bound buffer's footprint in the allocation is read (the buffer offset is `mem_offset -
/// bound_offset`). Memory with NO bound buffer is host-only staging with no readable device source, so it
/// is left exactly as-is — data is never faked. Errors only if the sink's readback transport itself fails;
/// an unknown/unbound memory is a no-op success.
pub fn read_mapped(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    memory: VkDeviceMemory,
    offset: u64,
    size: u64,
) -> Result<()> {
    // Resolve EVERY buffer bound into this allocation + its footprint (many buffers may share one arena;
    // see `MemRec::bound_buffers`), plus the staging length.
    let Some((buffers, mem_len)) = dev.memories.get(&memory).map(|m| {
        let bufs: Vec<(u32, u64, u64)> = m
            .bound_buffers
            .iter()
            .filter_map(|h| {
                dev.buffers
                    .get(h)
                    .map(|b| (b.ir_id, b.size, b.bound_offset))
            })
            .collect();
        (bufs, m.data.len() as u64)
    }) else {
        return Ok(()); // unknown memory
    };
    if buffers.is_empty() {
        return Ok(()); // host-only staging with no device buffer to read back
    }

    // The requested map range, clamped to the allocation.
    let map_start = offset.min(mem_len);
    let map_end = if size == VK_WHOLE_SIZE {
        mem_len
    } else {
        offset.saturating_add(size).min(mem_len)
    };
    // Refresh each bound buffer whose footprint [bound_offset, bound_offset + buf_size) overlaps the range.
    for (ir_id, buf_size, bound_offset) in buffers {
        let start = map_start.max(bound_offset);
        let end = map_end.min(bound_offset.saturating_add(buf_size));
        if end <= start {
            continue; // this buffer's footprint does not overlap the mapped range
        }
        let read_off = start - bound_offset;
        let len = (end - start) as usize;
        let bytes = sink.read_buffer(BufferId(ir_id), read_off, len)?;
        let m = dev
            .memories
            .get_mut(&memory)
            .expect("memory validated above");
        let dst = start as usize;
        let n = bytes.len().min(m.data.len().saturating_sub(dst));
        m.data[dst..dst + n].copy_from_slice(&bytes[..n]);
    }
    Ok(())
}

/// `vkUnmapMemory` — clear the mapped flag, but CAPTURE the still-mapped bytes as a pending host→device
/// upload so the app's writes survive the unmap. Without this, a real app doing map → write → UNMAP
/// (staging before submit) would silently drop its upload: the still-mapped submit flush no longer sees
/// the memory, so the bytes never reach the device. The whole allocation is marked dirty `(0,
/// VK_WHOLE_SIZE)`; the next `vkQueueSubmit` flushes it (intersected with the bound buffer's footprint)
/// as a `Cmd::WriteBuffer` and clears the record. Unbound host-only staging has no device buffer to
/// upload to, so nothing is captured (a truthful no-op). Coalesced with the still-mapped path so a
/// buffer that is submitted while still mapped is never written twice.
impl Device {
    pub fn unmap_memory(&mut self, memory: VkDeviceMemory) {
        if let Some(m) = self.memories.get_mut(&memory) {
            m.mapped = false;
            if !m.bound_buffers.is_empty() {
                m.pending_flush = Some((0, VK_WHOLE_SIZE));
            }
        }
    }
}

/// Capture a dirtied mapped range `(offset, size)` as a pending host→device upload that must reach the
/// device at the next `vkQueueSubmit` even if the app unmaps first — the `vkFlushMappedMemoryRanges`
/// signal for the non-coherent contract. Only buffer-bound memory is captured (unbound host-only
/// staging has no device buffer, so it stays a truthful no-op); an unknown handle is ignored. Any range
/// already pending this submit is widened to cover both, so a sub-range flush followed by an unmap (or
/// another flush) never loses the earlier bytes.
pub fn capture_pending_upload(dev: &mut Device, memory: VkDeviceMemory, offset: u64, size: u64) {
    if let Some(m) = dev.memories.get_mut(&memory) {
        if !m.bound_buffers.is_empty() {
            m.pending_flush = Some(match m.pending_flush {
                Some(prev) => {
                    let start = prev.0.min(offset);
                    if prev.1 == VK_WHOLE_SIZE || size == VK_WHOLE_SIZE {
                        (start, VK_WHOLE_SIZE)
                    } else {
                        let end = prev
                            .0
                            .saturating_add(prev.1)
                            .max(offset.saturating_add(size));
                        (start, end - start)
                    }
                }
                None => (offset, size),
            });
        }
    }
}

// ---- images / samplers ---------------------------------------------------------------------------

/// `vkCreateImage` — mint an hl-GPU texture id, translate format/usage, and submit [`Cmd::CreateTexture`].
/// `vk_format` is a raw `VkFormat`; `vk_usage` a raw `VkImageUsageFlags`; `vk_samples` a raw
/// `VkSampleCountFlagBits` (whose bit VALUE is the sample count: `_1_BIT`=1, `_2_BIT`=2, `_4_BIT`=4, …).
/// The sample count threads to [`TextureDesc::sample_count`] so a real MSAA `VkImage` is materialized as a
/// multisampled texture (the executor honors it — `TextureDesc.sample_count` + `Enc::ResolveTexture`, #179);
/// `0`/`1` collapse to a single-sample texture so an existing single-sample app is byte-identical.
pub fn create_image(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    width: u32,
    height: u32,
    vk_format: u32,
    vk_usage: u32,
    vk_samples: u32,
) -> Result<VkImage> {
    create_image_layers(
        dev, sink, width, height, 1, 1, false, vk_format, vk_usage, vk_samples,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_image_layers(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    width: u32,
    height: u32,
    layers: u32,
    mip_levels: u32,
    cube: bool,
    vk_format: u32,
    vk_usage: u32,
    vk_samples: u32,
) -> Result<VkImage> {
    create_image_geometry(
        dev,
        sink,
        width,
        height,
        1,
        layers,
        mip_levels,
        if cube {
            TextureDim::Cube
        } else {
            TextureDim::D2
        },
        vk_format,
        vk_usage,
        vk_samples,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_image_geometry(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    width: u32,
    height: u32,
    depth: u32,
    layers: u32,
    mip_levels: u32,
    dim: TextureDim,
    vk_format: u32,
    vk_usage: u32,
    vk_samples: u32,
) -> Result<VkImage> {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    // A zero extent is a spec violation (VUID-VkImageCreateInfo-extent), and an extent past the modeled
    // `maxImageDimension2D` cannot be created — both truthful usage errors, never a fake success.
    if width == 0 || height == 0 || depth == 0 || layers == 0 {
        return Err(GpuError::Invalid(
            "vkCreateImage: image extent and array layers must be greater than 0",
        ));
    }
    if dim == TextureDim::D3 && layers != 1 {
        return Err(GpuError::Invalid(
            "vkCreateImage: 3D images require arrayLayers == 1",
        ));
    }
    if dim != TextureDim::D3 && depth != 1 {
        return Err(GpuError::Invalid(
            "vkCreateImage: non-3D images require extent.depth == 1",
        ));
    }
    let max_dim = if dim == TextureDim::D3 {
        dev.physical_device.limits.max_image_dimension_3d
    } else {
        dev.physical_device.limits.max_image_dimension_2d
    };
    if width > max_dim || height > max_dim || depth > max_dim {
        return Err(GpuError::Invalid(
            "vkCreateImage: image extent exceeds the dimension limit",
        ));
    }
    let format = Format(vk_format)
        .wire()
        .ok_or(GpuError::Invalid("vkCreateImage: unsupported VkFormat"))?;
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let usage = ImageUsage(vk_usage).wire();
    // `VkSampleCountFlagBits` encodes the count AS its bit value (1/2/4/8/16/32/64); an absent/`_1_BIT`
    // field is single-sample. Anything else threads through as the requested multisample count.
    let sample_count = vk_samples.max(1);
    sink.submit(&[Cmd::CreateTexture(
        ir_id,
        TextureDesc {
            width,
            height,
            depth: if dim == TextureDim::D3 { depth } else { layers },
            mip_levels: mip_levels.max(1),
            sample_count,
            dim,
            format,
            usage,
            label: format!("vkimg{ir_id}"),
        },
    )])?;
    dev.images.insert(
        handle,
        ImageRec {
            ir_id,
            width,
            height,
            depth,
            dim,
            layers,
            mip_levels: mip_levels.max(1),
            format,
            usage,
            sample_count,
            is_render_target: ImageUsage(vk_usage).is_render_target(),
        },
    );
    Ok(handle)
}

/// `vkCreateSampler` — translate the filter/address state and submit [`Cmd::CreateSampler`]. The `vk_*`
/// arguments are raw Vulkan enum values (`VkFilter`, `VkSamplerMipmapMode`, `VkSamplerAddressMode`).
pub fn create_sampler(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vk_min_filter: u32,
    vk_mag_filter: u32,
    vk_mipmap_mode: u32,
    vk_address_uvw: [u32; 3],
) -> VkSampler {
    let desc = SamplerDesc {
        min_filter: SamplerFilter::from_vk(vk_min_filter),
        mag_filter: SamplerFilter::from_vk(vk_mag_filter),
        mip_filter: SamplerFilter::from_vk(vk_mipmap_mode), // VkSamplerMipmapMode shares NEAREST=0/LINEAR=1
        address_u: SamplerAddress::from_vk(vk_address_uvw[0]),
        address_v: SamplerAddress::from_vk(vk_address_uvw[1]),
        address_w: SamplerAddress::from_vk(vk_address_uvw[2]),
        ..SamplerDesc::default()
    };
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    // Submit is infallible against the recording sink; a real sink would surface a transport error.
    let _ = sink.submit(&[Cmd::CreateSampler(ir_id, desc)]);
    dev.samplers.insert(handle, SamplerRec { ir_id });
    handle
}

/// `VkFilter` (0 = NEAREST, 1 = LINEAR) → hl-GPU [`Filter`].
struct SamplerFilter;
impl SamplerFilter {
    fn from_vk(v: u32) -> Filter {
        if v == 1 {
            Filter::Linear
        } else {
            Filter::Nearest
        }
    }
}

/// `VkSamplerAddressMode` → hl-GPU [`AddressMode`] (CLAMP_TO_BORDER / MIRROR_CLAMP fold to the nearest
/// supported neighbour — a bounded translation, ported from `memory.rs::ir_address`).
struct SamplerAddress;
impl SamplerAddress {
    fn from_vk(v: u32) -> AddressMode {
        match v {
            0 => AddressMode::Repeat,           // REPEAT
            1 | 4 => AddressMode::MirrorRepeat, // MIRRORED_REPEAT / MIRROR_CLAMP
            _ => AddressMode::ClampToEdge,      // CLAMP_TO_EDGE / CLAMP_TO_BORDER
        }
    }
}

// ---- image subresource layout --------------------------------------------------------------------

/// A `VkSubresourceLayout` for one tightly packed base image subresource.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SubresourceLayout {
    pub offset: u64,
    pub size: u64,
    pub row_pitch: u64,
    pub array_pitch: u64,
    pub depth_pitch: u64,
}

/// `vkGetImageSubresourceLayout` — report the tightly packed base-level layout. Depth slices and array
/// layers are distinct axes, matching Vulkan's storage geometry.
impl Device {
    pub fn image_subresource_layout(&self, image: VkImage) -> Result<SubresourceLayout> {
        let img = self.images.get(&image).ok_or(GpuError::Invalid(
            "vkGetImageSubresourceLayout: unknown VkImage",
        ))?;
        let bytes = img.format.bytes_per_texel().unwrap_or(4) as u64;
        let row_pitch = img.width as u64 * bytes;
        let depth_pitch = row_pitch * img.height as u64;
        let array_pitch = depth_pitch * img.depth as u64;
        Ok(SubresourceLayout {
            offset: 0,
            size: array_pitch * img.layers as u64,
            row_pitch,
            array_pitch,
            depth_pitch,
        })
    }
}

// ---- fences --------------------------------------------------------------------------------------

/// `vkCreateFence` — mint an hl-GPU fence id and submit [`Cmd::CreateFence`]. `signaled` reflects
/// `VK_FENCE_CREATE_SIGNALED_BIT`.
pub fn create_fence(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    signaled: bool,
) -> Result<VkFence> {
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateFence(ir_id)])?;
    dev.fences.insert(handle, FenceRec::new(ir_id, signaled));
    Ok(handle)
}
