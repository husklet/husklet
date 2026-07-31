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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PresenterId {
    Wayland(usize),
    Surface(u64),
}

impl PresenterId {
    /// Whether this identity names a real application window that a present MUST reach.
    ///
    /// `Wayland(w)` with a non-zero `w` is an application-owned `wl_surface`: if a present does not
    /// commit to it, nothing is displayed and the present has failed. `Surface(_)` (a headless surface)
    /// and `Wayland(0)` (the application supplied no `wl_surface`) are deliberately offscreen targets
    /// where a readback-only present is the honest outcome, so those stay `VK_SUCCESS`.
    pub fn expects_window(&self) -> bool {
        matches!(self, PresenterId::Wayland(surface) if *surface != 0)
    }
}

/// Surface-owned Wayland presenters with swapchain leases.
///
/// Vulkan permits an old and replacement swapchain to overlap on one `VkSurfaceKHR`. The protocol
/// identity therefore belongs to the surface, while swapchains only hold leases to it.
pub struct Presenters {
    surfaces: HashMap<PresenterId, Option<WaylandAppPresenter>>,
    swapchains: HashMap<u64, PresenterId>,
}

impl Presenters {
    pub(crate) fn new() -> Self {
        Self {
            surfaces: HashMap::new(),
            swapchains: HashMap::new(),
        }
    }

    pub fn ensure(
        &mut self,
        surface: PresenterId,
        create: impl FnOnce() -> Option<WaylandAppPresenter>,
    ) -> &mut Option<WaylandAppPresenter> {
        self.surfaces.entry(surface).or_insert_with(create)
    }

    pub fn bind(&mut self, swapchain: u64, surface: PresenterId) {
        self.swapchains.insert(swapchain, surface);
    }

    pub fn discard_unbound(&mut self, surface: PresenterId) {
        if !self.swapchains.values().any(|owner| *owner == surface) {
            self.surfaces.remove(&surface);
        }
    }

    pub fn get_mut(&mut self, swapchain: u64) -> Option<&mut Option<WaylandAppPresenter>> {
        let surface = *self.swapchains.get(&swapchain)?;
        self.surfaces.get_mut(&surface)
    }

    pub fn surface(&self, swapchain: u64) -> Option<PresenterId> {
        self.swapchains.get(&swapchain).copied()
    }

    pub fn swapchains(&self, surface: PresenterId) -> Vec<u64> {
        self.swapchains
            .iter()
            .filter_map(|(swapchain, owner)| (*owner == surface).then_some(*swapchain))
            .collect()
    }

    pub fn unbind(&mut self, swapchain: u64) {
        let Some(surface) = self.swapchains.remove(&swapchain) else {
            return;
        };
        if !self.swapchains.values().any(|owner| *owner == surface) {
            self.surfaces.remove(&surface);
        }
    }
}

/// Everything the shim tracks between `vk*` calls.
pub struct State {
    /// The current `VkInstance` (created by `vkCreateInstance`), holding the physical-device descriptor.
    pub instance: Option<Instance>,
    /// The logical device (created by `vkCreateDevice`) — the `hl_vulkan` object model + lowering target.
    pub device: Option<Device>,
    /// The guest→host boundary: encodes each lowered batch and ships it framed over `$HL_GPU_EXEC`.
    pub sink: RemoteCommandSink,
    /// Whether the negotiated host can import native IOSurface-backed presentation textures.
    pub native_present: bool,

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
    /// Surface-owned presenters plus swapchain leases. Overlapping recreation shares one identity.
    pub presenters: Presenters,

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
    /// `(VkCommandBuffer, set number)` → the descriptor set that carries the descriptors pushed at that
    /// set index (`VK_KHR_push_descriptor`, core Vulkan 1.4). Push descriptors are command-buffer state,
    /// not an app object, so the set is minted by the shim; consecutive pushes accumulate into it exactly
    /// as the spec requires, and re-recording the command buffer forgets it (a push-descriptor set does
    /// not survive `vkBeginCommandBuffer`).
    pub push_descriptor_sets: HashMap<(u64, u32), u64>,
    /// The unbounded pool those pushed sets are allocated from, minted on first push.
    pub push_descriptor_pool: u64,
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
        // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
        let mut sink = RemoteCommandSink::new(
            std::env::var("HL_GPU_EXEC").unwrap_or_else(|_| DEFAULT_EXEC_SOCK.to_owned()),
        );
        // Honour the same trace switches the GL shim honours. Without this the Vulkan ICD was silent
        // under both, so a total presentation failure produced no diagnostic at all.
        sink.set_trace(
            std::env::var_os("HL_GPU_TRACE").is_some()
                || std::env::var_os("HL_SHIM_DEBUG").is_some(),
        );
        State {
            instance: None,
            device: None,
            // Connect target from $HL_GPU_EXEC; the connection itself is opened lazily on first submit.
            sink,
            native_present: false,
            image_views: HashMap::new(),
            render_passes: HashMap::new(),
            framebuffers: HashMap::new(),
            surfaces: std::collections::HashSet::new(),
            next_surface: 0,
            wayland_surfaces: HashMap::new(),
            presenters: Presenters::new(),
            private_data_slots: std::collections::HashSet::new(),
            private_data: HashMap::new(),
            ycbcr_conversions: std::collections::HashSet::new(),
            debug_object_names: HashMap::new(),
            debug_messengers: std::collections::HashSet::new(),
            debug_report_callbacks: std::collections::HashSet::new(),
            buffer_views: std::collections::HashSet::new(),
            push_descriptor_sets: HashMap::new(),
            push_descriptor_pool: 0,
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
        // The driver's logging composition root: opens `hl-log`'s runtime tag mask from the environment
        // on first use, so an `hl_error!` in an entry point can actually reach stderr. One relaxed
        // atomic after the first call. Every `vk*` entry point funnels through here, so the gate opens
        // before anything can report. See [`crate::logging::GuestLogging`].
        crate::logging::GuestLogging::install();
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        let state = STATE.get_or_init(|| Mutex::new(State::new()));
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        f(&mut state)
    }
}

#[cfg(test)]
mod presenter_tests {
    use super::{PresenterId, Presenters};

    #[test]
    fn overlapping_swapchains_keep_one_surface_owner_until_the_last_destroy() {
        let surface = PresenterId::Wayland(7);
        let old = 11;
        let new = 12;
        let mut presenters = Presenters::new();
        presenters.ensure(surface, || None);
        presenters.bind(old, surface);
        presenters.bind(new, surface);

        presenters.unbind(old);
        assert_eq!(presenters.surface(new), Some(surface));
        assert!(matches!(presenters.get_mut(new), Some(None)));

        presenters.unbind(new);
        assert!(presenters.get_mut(new).is_none());
        assert!(presenters.swapchains(surface).is_empty());
    }
}
