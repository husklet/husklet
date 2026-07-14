//! Native buffer handle + the host↔device byte-transfer primitives every copy/readback path rests on.
//!
//! A protocol buffer becomes a single `wgpu::Buffer` allocated with the union of usages the IR can ask of
//! it (STORAGE | COPY_SRC | COPY_DST | VERTEX | INDEX | UNIFORM | INDIRECT). Host reads/writes are
//! CPU-mediated (staging-buffer readback + `queue.write_buffer`) so this backend is free of wgpu's 4-byte
//! copy-offset and 256-byte row-stride alignment rules at the protocol boundary — it reproduces the byte-
//! addressable semantics the CPU oracle guarantees, which the conformance suite's unaligned copies need.

use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol buffer. `size` is the logical (guest-visible) length; the wgpu
/// allocation is that rounded up to 4 bytes so storage-buffer / copy alignment always holds internally.
pub struct WgpuBuffer {
    pub buffer: wgpu::Buffer,
    pub size: u64,
}

/// Downcast a live buffer id to its native handle.
pub fn native<'a>(res: &'a SessionResources, id: u32) -> Result<&'a WgpuBuffer> {
    res.buffers
        .get(id)?
        .downcast_ref::<WgpuBuffer>()
        .ok_or(GpuError::Invalid("wgpu: buffer native type mismatch"))
}

fn round4(n: u64) -> u64 {
    n.div_ceil(4) * 4
}

impl WgpuExecutor {
    /// Allocate a zero-initialized device buffer for `size` logical bytes (wgpu zero-inits lazily).
    pub(crate) fn make_buffer(&self, size: u64) -> WgpuBuffer {
        let alloc = round4(size).max(4);
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-buffer"),
            size: alloc,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::UNIFORM
                | wgpu::BufferUsages::INDIRECT,
            mapped_at_creation: false,
        });
        WgpuBuffer { buffer, size }
    }

    /// Read `len` bytes at `offset` out of buffer `id`. Copies a 4-aligned superrange into a MAP_READ
    /// staging buffer, waits, and slices out the exact window — so any (unaligned) offset/len works.
    pub(crate) fn read_bytes(
        &self,
        res: &SessionResources,
        id: u32,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let b = native(res, id)?;
        let end = offset
            .checked_add(len as u64)
            .filter(|e| *e <= b.size)
            .ok_or(GpuError::OutOfBounds)?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let astart = offset & !3;
        let window = self.read_span(&b.buffer, astart, round4(end) - astart);
        let lo = (offset - astart) as usize;
        Ok(window[lo..lo + len].to_vec())
    }

    /// Copy `span` bytes at 4-aligned `astart` out of a wgpu buffer into a mapped staging buffer and return
    /// them. Operates on the device allocation directly (which is rounded up to 4 bytes), so an aligned
    /// window that runs into a buffer's tail padding is a valid read — the read-modify-write path relies on
    /// this to touch the 4-aligned window enclosing an unaligned logical range.
    fn read_span(&self, wbuf: &wgpu::Buffer, astart: u64, span: u64) -> Vec<u8> {
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hl-readback"),
            size: span,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        enc.copy_buffer_to_buffer(wbuf, astart, &staging, 0, span);
        self.gpu.queue.submit(Some(enc.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.gpu.device.poll(wgpu::Maintain::Wait);
        let mapped = slice.get_mapped_range();
        let out = mapped.to_vec();
        drop(mapped);
        staging.unmap();
        out
    }

    /// Write `data` into buffer `id` at `offset`. Aligned writes go straight through `queue.write_buffer`;
    /// an unaligned offset/length is a read-modify-write over the enclosing 4-aligned window so neighbour
    /// bytes are preserved (the byte-addressable guarantee the oracle makes).
    pub(crate) fn write_bytes(
        &self,
        res: &SessionResources,
        id: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<()> {
        let b = native(res, id)?;
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= b.size)
            .ok_or(GpuError::OutOfBounds)?;
        if data.is_empty() {
            return Ok(());
        }
        if offset % 4 == 0 && data.len() % 4 == 0 {
            self.gpu.queue.write_buffer(&b.buffer, offset, data);
            self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
            return Ok(());
        }
        // Unaligned: read the enclosing 4-aligned window (against the allocation, which may extend past
        // the logical size into tail padding), patch, write it back whole.
        let astart = offset & !3;
        let mut window = self.read_span(&b.buffer, astart, round4(end) - astart);
        let lo = (offset - astart) as usize;
        window[lo..lo + data.len()].copy_from_slice(data);
        self.gpu.queue.write_buffer(&b.buffer, astart, &window);
        self.gpu.queue.submit(None::<wgpu::CommandBuffer>);
        Ok(())
    }
}
