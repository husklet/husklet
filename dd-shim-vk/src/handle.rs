//! Loader-dispatchable-handle ABI, ported from the Khronos Vulkan-Loader / MoltenVK contract.
//!
//! A Vulkan **dispatchable** object (`VkInstance`, `VkPhysicalDevice`, `VkDevice`, `VkQueue`,
//! `VkCommandBuffer`) is, at the ABI, a pointer to a C struct whose FIRST pointer-sized slot the
//! loader owns: it overwrites that slot with a pointer to *its* dispatch table and validates a magic
//! value the ICD must stamp there at creation time. See:
//!   * Vulkan-Loader `docs/LoaderDriverInterface.md` §"Driver Dispatchable Object Creation"
//!   * Vulkan-Loader `include/vulkan/vk_icd.h` (`ICD_LOADER_MAGIC`, `set_loader_magic_value`)
//!   * MoltenVK `MVKVulkanAPIObject.h` (`MVKDispatchableVulkanAPIObject`: `VK_LOADER_DATA loaderData`
//!     as field 0, `set_loader_magic_value(&_icdRef)` in `getVkHandle()`).
//!
//! So every dispatchable object we hand back begins with `loader_data: usize` set to
//! [`ICD_LOADER_MAGIC`]; we NEVER read that slot back (the loader clobbers it), we only read the
//! fields after it. Getting this wrong is the classic `VK_ERROR_INCOMPATIBLE_DRIVER` / loader crash.
//!
//! Non-dispatchable objects (`VkCommandPool`, `VkDeviceMemory`, `VkBuffer`, …) are plain 64-bit
//! handles with no loader slot; we mint them as boxed-pointer bit patterns (`Box::into_raw as u64`).

use core::ffi::c_void;

/// `ICD_LOADER_MAGIC` from `vk_icd.h`. The loader checks `(loaderMagic & 0xffffffff) == this`.
pub const ICD_LOADER_MAGIC: usize = 0x01CD_C0DE;

/// A dispatchable ICD object: the loader-owned slot in field 0, then the ICD's own state `T`.
/// `#[repr(C)]` so field 0 is exactly the first pointer-sized word the loader reads/writes.
#[repr(C)]
pub struct Dispatchable<T> {
    /// Owned by the loader after creation — stamped with [`ICD_LOADER_MAGIC`], never read by us.
    pub loader_data: usize,
    pub inner: T,
}

impl<T> Dispatchable<T> {
    /// Box a new dispatchable object with the loader magic stamped, returning the raw handle the ICD
    /// returns to the loader. Mirrors MoltenVK's `alloc_icd_obj()` + `set_loader_magic_value`.
    pub fn new(inner: T) -> *mut c_void {
        let b = Box::new(Dispatchable {
            loader_data: ICD_LOADER_MAGIC,
            inner,
        });
        Box::into_raw(b) as *mut c_void
    }

    /// Borrow the ICD state behind a dispatchable handle the loader passed back. `None` for NULL.
    ///
    /// # Safety
    /// `h` must be a handle previously returned by [`Dispatchable::new`] for this `T` and not yet
    /// freed. The loader guarantees this for handles it routes to us.
    pub unsafe fn inner<'a>(h: *mut c_void) -> Option<&'a mut T> {
        (h as *mut Dispatchable<T>).as_mut().map(|d| &mut d.inner)
    }

    /// Reclaim and drop a dispatchable handle (the `vkDestroy*` / `vkFree*` path).
    ///
    /// # Safety
    /// Same contract as [`Dispatchable::inner`]; `h` must not be used afterward.
    pub unsafe fn free(h: *mut c_void) {
        if !h.is_null() {
            drop(Box::from_raw(h as *mut Dispatchable<T>));
        }
    }
}

/// Mint a non-dispatchable 64-bit handle from a boxed payload (`Box::into_raw` bit pattern).
pub fn nondispatch_new<T>(inner: T) -> u64 {
    Box::into_raw(Box::new(inner)) as u64
}

/// Borrow a non-dispatchable handle's payload.
///
/// # Safety
/// `h` must be a handle from [`nondispatch_new`] for this `T`, still live.
pub unsafe fn nondispatch_inner<'a, T>(h: u64) -> Option<&'a mut T> {
    (h as *mut T).as_mut()
}

/// Reclaim and drop a non-dispatchable handle.
///
/// # Safety
/// `h` must be from [`nondispatch_new`] for this `T` and unused afterward.
pub unsafe fn nondispatch_free<T>(h: u64) {
    if h != 0 {
        drop(Box::from_raw(h as *mut T));
    }
}
