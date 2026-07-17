//! The device-memory allocation table: the simulated unified-VA bump allocator + the
//! device-pointer → (backing buffer, byte offset) resolver.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`Alloc`, `next_ptr`, `mem_alloc`/`resolve`/`mem_free`). The buffer
//! *ids* are minted by [`super::context::CudaContext`] (one shared counter across allocations and
//! kernel-parameter buffers, exactly as the source did); this table owns the address arithmetic.

use super::device::DevicePtr;
use hl_gpu::BufferId;
use std::collections::{HashMap, HashSet};

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
    /// Bases that were allocated as *managed* (`cuMemAllocManaged`) — device memory that is also
    /// host-addressable in the model, so `cuPointerGetAttribute(IS_MANAGED)` answers truthfully.
    managed: HashSet<u64>,
    next_ptr: u64,
}

/// Round `v` up to a multiple of `a` (a power of two).
fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) & !(a - 1)
}

impl Allocations {
    /// A fresh table with the bump cursor started well above 0 and page-aligned, like a real allocator.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            managed: HashSet::new(),
            next_ptr: 0x10_0000,
        }
    }

    /// Record a new allocation of `size` bytes backed by buffer id `buffer`, returning its device
    /// pointer. The cursor bumps with 256-byte alignment (CUDA guarantees ≥256 B alignment).
    pub fn record(&mut self, buffer: u32, size: u64) -> DevicePtr {
        let ptr = self.next_ptr;
        self.next_ptr = align_up(self.next_ptr + size.max(1), 256);
        self.map.insert(ptr, Alloc { buffer, size });
        DevicePtr(ptr)
    }

    /// Record a *managed* allocation (`cuMemAllocManaged`): identical bookkeeping to [`record`](Self::record)
    /// but the base is flagged managed so pointer-attribute queries report it as unified/managed memory.
    pub fn record_managed(&mut self, buffer: u32, size: u64) -> DevicePtr {
        let p = self.record(buffer, size);
        self.managed.insert(p.0);
        p
    }

    /// Is the allocation *containing* `p` managed memory? `false` for a device (non-managed) allocation
    /// or a dangling pointer.
    pub fn is_managed(&self, p: DevicePtr) -> bool {
        self.map
            .iter()
            .find(|(&base, a)| p.0 >= base && p.0 < base + a.size.max(1))
            .map(|(&base, _)| self.managed.contains(&base))
            .unwrap_or(false)
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
        self.managed.remove(&p.0);
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

/// Host-side **pinned** and **registered** memory the model owns — the backing state for the driver's
/// host-memory entry points (`cuMemAllocHost` / `cuMemHostAlloc` / `cuMemFreeHost` / `cuMemHostRegister` /
/// `cuMemHostUnregister` / `cuMemHostGetDevicePointer`).
///
/// A *pinned* allocation is a real byte buffer we own; the caller gets a stable pointer into it (the
/// buffer is never resized after creation, so `Vec::as_ptr` stays valid for the buffer's lifetime). It is
/// therefore directly usable as a host copy source/destination. A *registered* range is guest-owned host
/// memory we only record the extent of. Either kind can be lazily mapped to a device buffer (for kernel
/// use) via [`device_mapping`](Self::device_mapping); the mapping is cached so a repeated
/// `cuMemHostGetDevicePointer` on the same host pointer returns the same device pointer.
#[derive(Debug, Default)]
pub struct HostMemory {
    /// Pinned allocations we own: host base address → the backing bytes (length is the allocation size).
    pinned: HashMap<u64, Vec<u8>>,
    /// Registered guest-owned ranges: host base address → byte length.
    registered: HashMap<u64, u64>,
    /// Host base address → the device buffer `cuMemHostGetDevicePointer` mapped it to
    /// (device buffer id, device pointer).
    mapped: HashMap<u64, (u32, u64)>,
}

impl HostMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// `cuMemAllocHost` / `cuMemHostAlloc`: allocate `size` (min 1) zeroed bytes we own, returning the
    /// stable base address the caller uses directly as page-locked host memory.
    pub fn alloc_pinned(&mut self, size: usize) -> u64 {
        let mut v = vec![0u8; size.max(1)];
        let base = v.as_mut_ptr() as u64;
        // Moving `v` into the map moves only the Vec header; the heap buffer `base` points at is stable.
        self.pinned.insert(base, v);
        base
    }

    /// `cuMemFreeHost`: drop a pinned allocation (freeing its bytes) and any device mapping it had.
    /// `true` iff `base` was a live pinned allocation we owned.
    pub fn free_pinned(&mut self, base: u64) -> bool {
        self.mapped.remove(&base);
        self.pinned.remove(&base).is_some()
    }

    /// `cuMemHostRegister`: record a guest-owned `[base, base+size)` range as page-locked. `false` if the
    /// base is already a live host allocation (→ `CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED`).
    pub fn register(&mut self, base: u64, size: u64) -> bool {
        if self.registered.contains_key(&base) || self.pinned.contains_key(&base) {
            return false;
        }
        self.registered.insert(base, size);
        true
    }

    /// `cuMemHostUnregister`: forget a previously registered range (and any device mapping). `false` if
    /// `base` was not a registered range (→ `CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED`).
    pub fn unregister(&mut self, base: u64) -> bool {
        self.mapped.remove(&base);
        self.registered.remove(&base).is_some()
    }

    /// The byte length of the host allocation (pinned or registered) based exactly at `base`, or `None`.
    pub fn size_of(&self, base: u64) -> Option<u64> {
        if let Some(v) = self.pinned.get(&base) {
            Some(v.len() as u64)
        } else {
            self.registered.get(&base).copied()
        }
    }

    /// Is `base` the base of a live host allocation (pinned or registered)?
    pub fn is_host_base(&self, base: u64) -> bool {
        self.pinned.contains_key(&base) || self.registered.contains_key(&base)
    }

    /// The cached device mapping for a host base, if `cuMemHostGetDevicePointer` already created one.
    pub fn device_mapping(&self, base: u64) -> Option<(u32, u64)> {
        self.mapped.get(&base).copied()
    }

    /// Cache the device buffer + pointer a host base was mapped to.
    pub fn set_device_mapping(&mut self, base: u64, buffer: u32, ptr: u64) {
        self.mapped.insert(base, (buffer, ptr));
    }
}
