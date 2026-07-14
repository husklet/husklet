//! The device-memory allocation table: the simulated unified-VA bump allocator + the
//! device-pointer → (backing buffer, byte offset) resolver.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`Alloc`, `next_ptr`, `mem_alloc`/`resolve`/`mem_free`). The buffer
//! *ids* are minted by [`super::context::CudaContext`] (one shared counter across allocations and
//! kernel-parameter buffers, exactly as the source did); this table owns the address arithmetic.

use super::device::DevicePtr;
use hl_gpu::BufferId;
use std::collections::HashMap;

/// One live device allocation: the backing hl-GPU buffer id + its size in bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Alloc {
    pub buffer: u32,
    pub size: u64,
}

/// Base device-pointer → allocation map with a monotonic, page-aligned bump cursor.
#[derive(Debug, Default)]
pub struct Allocations {
    map: HashMap<u64, Alloc>,
    next_ptr: u64,
}

/// Round `v` up to a multiple of `a` (a power of two).
fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

impl Allocations {
    /// A fresh table with the bump cursor started well above 0 and page-aligned, like a real allocator.
    pub fn new() -> Self {
        Self { map: HashMap::new(), next_ptr: 0x10_0000 }
    }

    /// Record a new allocation of `size` bytes backed by buffer id `buffer`, returning its device
    /// pointer. The cursor bumps with 256-byte alignment (CUDA guarantees ≥256 B alignment).
    pub fn record(&mut self, buffer: u32, size: u64) -> DevicePtr {
        let ptr = self.next_ptr;
        self.next_ptr = align_up(self.next_ptr + size.max(1), 256);
        self.map.insert(ptr, Alloc { buffer, size });
        DevicePtr(ptr)
    }

    /// Map a (possibly offset) device pointer back to (buffer id, byte offset). `None` for a dangling
    /// pointer — the translation-layer equivalent of `CUDA_ERROR_INVALID_VALUE`.
    pub fn resolve(&self, p: DevicePtr) -> Option<(BufferId, u64)> {
        for (&base, a) in &self.map {
            if p.0 >= base && p.0 < base + a.size.max(1) {
                return Some((BufferId(a.buffer), p.0 - base));
            }
        }
        None
    }

    /// Free the allocation whose base is exactly `p`, returning its backing buffer id. `None` if `p` is
    /// not an allocation base (double-free / bogus pointer).
    pub fn free(&mut self, p: DevicePtr) -> Option<u32> {
        self.map.remove(&p.0).map(|a| a.buffer)
    }

    /// Find the live allocation whose `[base, base+size)` range contains `p`, returning `(base, size)`.
    /// `None` for a dangling pointer. This is what the metadata queries (`cuPointerGetAttribute`,
    /// `cuMemGetAddressRange`) resolve against — unlike [`resolve`](Self::resolve) they need the
    /// allocation *base* and *size*, not the backing (buffer, offset).
    pub fn containing(&self, p: DevicePtr) -> Option<(u64, u64)> {
        self.map
            .iter()
            .find(|(&base, a)| p.0 >= base && p.0 < base + a.size.max(1))
            .map(|(&base, a)| (base, a.size))
    }

    /// Total bytes across every live allocation — the "used device memory" `cuMemGetInfo` subtracts
    /// from the device's total VRAM to report the free figure.
    pub fn total_bytes(&self) -> u64 {
        self.map.values().map(|a| a.size).sum()
    }

    /// Number of live allocations (diagnostics / tests).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
