//! The shim's process-global Vulkan state + the guest→host command sink.
//!
//! The `vk*` entry points are free `extern "C"` functions, so their shared mutable state lives behind a
//! process-global `Mutex`. The heavy lifting — the Vulkan→hl-GPU-IR lowering — is delegated to the
//! `hl_vulkan` service layer (`create`/`record`/`submit`/`present`), which mutates a [`Device`] and
//! submits protocol `Cmd`s through a [`hl_gpu::RemoteCommandSink`]. That sink is the single boundary to
//! the host GPU-exec service, connected lazily from `$HL_GPU_EXEC` on first submit.
//!
//! One simulated physical device, so the model is single-instance/single-device: the dispatchable
//! `VkInstance`/`VkPhysicalDevice`/`VkDevice`/`VkQueue` handles are loader-magic'd tokens routed to this
//! one global `State`. The `hl_vulkan::Device` owns the real object model (buffers/shaders/pipelines/
//! command buffers/…); this module only holds the connection + the instance/device presence.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use hl_gpu::RemoteCommandSink;
use hl_vulkan::{Device, Instance};

use crate::types::Dispatchable;
use core::ffi::c_void;

/// A `VkRenderPass`'s bring-up bookkeeping: whether its first color attachment clears (loadOp == CLEAR)
/// and that attachment's raw `VkFormat` (so a graphics pipeline created against this pass knows its one
/// color-target format). Objects the `hl_vulkan` object model does not itself carry live here in the shim.
#[derive(Clone, Copy)]
pub struct RenderPassRec {
    pub first_attachment_clears: bool,
    pub color_format_vk: u32,
}

/// Everything the shim tracks between `vk*` calls.
pub struct State {
    /// The current `VkInstance` (created by `vkCreateInstance`), holding the physical-device descriptor.
    pub instance: Option<Instance>,
    /// The logical device (created by `vkCreateDevice`) — the `hl_vulkan` object model + lowering target.
    pub device: Option<Device>,
    /// The guest→host boundary: encodes each lowered batch and ships it framed over `$HL_GPU_EXEC`.
    pub sink: RemoteCommandSink,

    /// `VkImageView` handle → the `VkImage` handle it views. The `hl_vulkan` model renders into images,
    /// so a view is a thin alias resolved back to its image at `vkCmdBeginRenderPass` (via a framebuffer).
    pub image_views: HashMap<u64, u64>,
    /// `VkRenderPass` handle → its bring-up bookkeeping (see [`RenderPassRec`]).
    pub render_passes: HashMap<u64, RenderPassRec>,
    /// `VkFramebuffer` handle → its attachment `VkImageView` handles (index 0 is the color target).
    pub framebuffers: HashMap<u64, Vec<u64>>,

    /// Live `VkSurfaceKHR` handles (the WSI surface model). A surface is an INSTANCE-level object created
    /// before any logical device, so it lives here in the shim state, not in the `hl_vulkan::Device`. The
    /// physical-device surface queries (`vkGetPhysicalDeviceSurface*KHR`) validate their handle against
    /// this set (an unknown one is `VK_ERROR_SURFACE_LOST_KHR`).
    pub surfaces: std::collections::HashSet<u64>,
    /// Monotonic non-dispatchable-handle counter for `VkSurfaceKHR` (never 0 == `VK_NULL_HANDLE`); kept
    /// on a distinct high base so surface handles never alias the device's object handles.
    next_surface: u64,

    /// Live `VkPrivateDataSlot` handles (the `VK_EXT_private_data` / core-1.3 slot objects). A slot is a
    /// pure host object; the per-object data it stores lives in [`Self::private_data`].
    pub private_data_slots: std::collections::HashSet<u64>,
    /// `(objectType, objectHandle, slot)` → the app's stored `u64` (`vkSetPrivateData`/`vkGetPrivateData`).
    /// An unset key reads back as 0 (the spec default).
    pub private_data: HashMap<(i32, u64, u64), u64>,
    /// Live `VkSamplerYcbcrConversion` handles (`VK_KHR_sampler_ycbcr_conversion` / core 1.1). The
    /// conversion is a pure host object referenced by a sampler's pNext; no IR is emitted for it.
    pub ycbcr_conversions: std::collections::HashSet<u64>,

    /// Stable loader-magic'd dispatchable tokens (a pointer, once minted, is reused so the loader's
    /// object identity is consistent across calls). `0` = not yet minted.
    phys_dev: usize,
    device_handle: usize,
    queue_handle: usize,
}

impl State {
    fn new() -> Self {
        State {
            instance: None,
            device: None,
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink: RemoteCommandSink::from_env(),
            image_views: HashMap::new(),
            render_passes: HashMap::new(),
            framebuffers: HashMap::new(),
            surfaces: std::collections::HashSet::new(),
            next_surface: 0,
            private_data_slots: std::collections::HashSet::new(),
            private_data: HashMap::new(),
            ycbcr_conversions: std::collections::HashSet::new(),
            phys_dev: 0,
            device_handle: 0,
            queue_handle: 0,
        }
    }

    /// Mint a fresh live `VkSurfaceKHR` handle (monotonic, never `VK_NULL_HANDLE`).
    pub fn mint_surface(&mut self) -> u64 {
        self.next_surface += 1;
        let handle = 0x5000_0000_0000_0000 + self.next_surface;
        self.surfaces.insert(handle);
        handle
    }

    /// Whether `surface` is a live (created, not destroyed) `VkSurfaceKHR`.
    pub fn surface_valid(&self, surface: u64) -> bool {
        surface != 0 && self.surfaces.contains(&surface)
    }

    /// The single physical-device dispatchable token, minted once and reused.
    pub fn phys_dev_handle(&mut self) -> *mut c_void {
        if self.phys_dev == 0 {
            self.phys_dev = Dispatchable::new(()) as usize;
        }
        self.phys_dev as *mut c_void
    }

    /// The logical-device dispatchable token, minted once and reused.
    pub fn device_token(&mut self) -> *mut c_void {
        if self.device_handle == 0 {
            self.device_handle = Dispatchable::new(()) as usize;
        }
        self.device_handle as *mut c_void
    }

    /// The single queue dispatchable token, minted once and reused.
    pub fn queue_token(&mut self) -> *mut c_void {
        if self.queue_handle == 0 {
            self.queue_handle = Dispatchable::new(()) as usize;
        }
        self.queue_handle as *mut c_void
    }

    /// The physical-device descriptor the property queries read (the instance's, or the default if a
    /// query races ahead of `vkCreateInstance`).
    pub fn physical_device(&self) -> hl_vulkan::PhysicalDeviceDesc {
        match &self.instance {
            Some(i) => i.physical_device.clone(),
            None => hl_vulkan::PhysicalDeviceDesc::hl_default(),
        }
    }

    /// Borrow the logical device mutably, if one has been created.
    pub fn device_mut(&mut self) -> Option<&mut Device> {
        self.device.as_mut()
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

/// Run `f` with exclusive access to the global shim state. Non-reentrant — never call [`with`] from
/// inside an `f` (the `Mutex` is not recursive); each entry point does exactly one `with`.
pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let m = STATE.get_or_init(|| Mutex::new(State::new()));
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}
