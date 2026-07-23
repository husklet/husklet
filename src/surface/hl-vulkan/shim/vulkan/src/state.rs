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

use hl_gpu::transport::DEFAULT_EXEC_SOCK;
use hl_gpu::RemoteCommandSink;
use hl_vulkan::adapter::wayland_app::WaylandAppPresenter;
use hl_vulkan::{Device, Instance};

use crate::types::Dispatchable;
use core::ffi::c_void;

/// A `VkRenderPass`'s bring-up bookkeeping: whether its first color attachment clears (loadOp == CLEAR)
/// and that attachment's raw `VkFormat` (so a graphics pipeline created against this pass knows its one
/// color-target format), plus the depth/stencil attachment (when the pass declares one) so the classic
/// `vkCmdBeginRenderPass` path can thread a real depth buffer — the mirror of the dynamic-rendering
/// `pDepthAttachment`. Objects the `hl_vulkan` object model does not itself carry live here in the shim.
#[derive(Clone, Copy)]
pub struct RenderPassRec {
    pub first_attachment_clears: bool,
    pub color_format_vk: u32,
    /// The depth/stencil attachment's bookkeeping, or `None` for a color-only pass.
    pub depth: Option<RenderPassDepth>,
}

/// The depth/stencil attachment of a classic `VkRenderPass` (from its `VkAttachmentDescription` table).
#[derive(Clone, Copy)]
pub struct RenderPassDepth {
    /// The attachment's slot in the render pass's attachment array — identical to its `VkImageView` index
    /// in a `VkFramebuffer` built for this pass (and its `pClearValues` index in the begin info), so
    /// `vkCmdBeginRenderPass` resolves the bound depth image view and its clear value from this index.
    pub index: u32,
    /// The depth attachment's raw `VkFormat` (a graphics pipeline created against this pass targets it).
    pub format_vk: u32,
    /// `loadOp == VK_ATTACHMENT_LOAD_OP_CLEAR` — whether the pass clears depth to its `clearValue` on begin.
    pub clear: bool,
}

/// The app's native wayland handles captured at `vkCreateWaylandSurfaceKHR` — the `wl_display*` /
/// `wl_surface*` (as raw `usize` addresses; never dereferenced in the shim). A swapchain built over a
/// wayland `VkSurfaceKHR` copies these so `vkQueuePresentKHR` can marshal the presented frame onto the
/// app's own `wl_surface` via [`WaylandAppPresenter`]. `surface == 0` marks a non-wayland surface.
#[derive(Clone, Copy)]
pub struct WaylandWindow {
    pub display: usize,
    pub surface: usize,
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

    /// `VkSurfaceKHR` → the app's captured wayland handles (only wayland surfaces; a wl `wl_surface*` of
    /// 0 or a non-wayland platform surface is simply absent). Populated by `vkCreateWaylandSurfaceKHR`.
    pub wayland_surfaces: HashMap<u64, WaylandWindow>,
    /// `VkSwapchainKHR` → the app's wayland window, copied from the swapchain's `VkSurfaceKHR` at
    /// `vkCreateSwapchainKHR`. Its presence is what routes `vkQueuePresentKHR`'s readback onto the app's
    /// `wl_surface` (absent ⇒ a headless/offscreen present: the readback still runs, the attach is skipped).
    pub swapchain_windows: HashMap<u64, WaylandWindow>,
    /// `VkSwapchainKHR` → its live [`WaylandAppPresenter`], or `None` if bring-up hit a *soft* error
    /// (libwayland/global absent) — cached so a soft-unavailable surface is not re-probed every frame.
    pub presenters: HashMap<u64, Option<WaylandAppPresenter>>,

    /// Live `VkPrivateDataSlot` handles (the `VK_EXT_private_data` / core-1.3 slot objects). A slot is a
    /// pure host object; the per-object data it stores lives in [`Self::private_data`].
    pub private_data_slots: std::collections::HashSet<u64>,
    /// `(objectType, objectHandle, slot)` → the app's stored `u64` (`vkSetPrivateData`/`vkGetPrivateData`).
    /// An unset key reads back as 0 (the spec default).
    pub private_data: HashMap<(i32, u64, u64), u64>,
    /// Live `VkSamplerYcbcrConversion` handles (`VK_KHR_sampler_ycbcr_conversion` / core 1.1). The
    /// conversion is a pure host object referenced by a sampler's pNext; no IR is emitted for it.
    pub ycbcr_conversions: std::collections::HashSet<u64>,

    /// `(objectType, objectHandle)` → the debug name set by `vkSetDebugUtilsObjectNameEXT` /
    /// `vkDebugMarkerSetObjectNameEXT`. Debug-only bookkeeping (the name is stored so a later query or a
    /// validation trace can surface it); never affects behaviour. `VK_EXT_debug_utils` is not advertised,
    /// but these entry points succeed benignly (they are safe no-ops that only record a name).
    pub debug_object_names: HashMap<(i32, u64), String>,
    /// Live `VkDebugUtilsMessengerEXT` handles (`vkCreateDebugUtilsMessengerEXT`). Pure host objects.
    pub debug_messengers: std::collections::HashSet<u64>,
    /// Live `VkDebugReportCallbackEXT` handles (`vkCreateDebugReportCallbackEXT`). Pure host objects.
    pub debug_report_callbacks: std::collections::HashSet<u64>,
    /// Live `VkBufferView` handles (`vkCreateBufferView`). A buffer view is a pure host object in this
    /// model (the color/compute lowering binds buffers directly), tracked so create/destroy balance.
    pub buffer_views: std::collections::HashSet<u64>,
    /// Monotonic counter for the auxiliary non-dispatchable handles above (debug messengers/callbacks,
    /// buffer views), kept on a distinct high base so they never alias device object or surface handles.
    next_aux: u64,

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
            sink: RemoteCommandSink::new(
                std::env::var("HL_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_owned()),
            ),
            image_views: HashMap::new(),
            render_passes: HashMap::new(),
            framebuffers: HashMap::new(),
            surfaces: std::collections::HashSet::new(),
            next_surface: 0,
            wayland_surfaces: HashMap::new(),
            swapchain_windows: HashMap::new(),
            presenters: HashMap::new(),
            private_data_slots: std::collections::HashSet::new(),
            private_data: HashMap::new(),
            ycbcr_conversions: std::collections::HashSet::new(),
            debug_object_names: HashMap::new(),
            debug_messengers: std::collections::HashSet::new(),
            debug_report_callbacks: std::collections::HashSet::new(),
            buffer_views: std::collections::HashSet::new(),
            next_aux: 0,
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

    /// Mint a fresh auxiliary non-dispatchable handle (debug messenger/callback, buffer view). Monotonic,
    /// never `VK_NULL_HANDLE`, on a high base distinct from device object + surface handles.
    pub fn mint_aux(&mut self) -> u64 {
        self.next_aux += 1;
        0x6000_0000_0000_0000 + self.next_aux
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

pub struct StateStore;

impl StateStore {
    /// Run `f` with exclusive access to the global shim state. Non-reentrant — never call this method
    /// from inside `f` (the `Mutex` is not recursive); each entry point takes the state exactly once.
    pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(State::new()));
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        f(&mut state)
    }
}
