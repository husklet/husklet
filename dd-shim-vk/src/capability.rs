//! The generated Vulkan **capability inventory** — the machine-checkable census that tags every
//! exported `vk*` entry point as `full`, `partial`, or `stub`, records the exact `VkResult` (or
//! zero/`VK_FALSE`/NULL) each stub returns, and names the Vulkan core version or extension the command
//! originates from.
//!
//! This is Phase 0's "make completeness measurable" deliverable for Vulkan (see
//! `docs/codex-rendering.md` §6 Phase 0 and §2.2): a bare `IMPLEMENTED` name list proves only that a
//! symbol resolves, not that its semantics exist. The inventory is *generated* by `build.rs` from the
//! export manifest, the `IMPLEMENTED` set, a `partial` override table, and the `vk.xml`-derived origin
//! sidecar (`registry/vk_command_origins.manifest`). The crate asserts against it at test time
//! (`CAPABILITIES` covers every exported command; nothing is advertised without a full/partial/stub
//! record; every stub returns a truthful non-success value). Runtime debug output and
//! `docs/rendering/SHIM_RUST_ARCHITECTURE.md` draw from the same census, so the advertised surface and
//! the truthful surface cannot drift.
//!
//! Classification is conservative: an entry is `full` only when its observable Vulkan semantics are
//! implemented for the bring-up model; `partial` when it works within a bounded domain (the entry's
//! `note` names it — e.g. the FIFO/round-robin swapchain, the already-signaled fence model);
//! and `stub` when it has no real body and always returns `vk_error` (the feature/extension is not
//! implemented).

/// Capability level of an exported entry point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cap {
    /// Observable Vulkan semantics implemented for the bring-up model.
    Full,
    /// Works within a bounded supported domain; outside it, behaviour is as the entry's `note` states.
    Partial,
    /// No real body: always returns the truthful `vk_error` (or zero/`VK_FALSE`/NULL for a
    /// non-`VkResult` return). The feature/extension is not implemented.
    Stub,
}

/// One entry in the capability inventory.
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    /// The exported `vk*` symbol name.
    pub name: &'static str,
    /// full / partial / stub.
    pub cap: Cap,
    /// The value a `stub` returns for a `VkResult` command (`VK_ERROR_FEATURE_NOT_PRESENT` /
    /// `VK_ERROR_EXTENSION_NOT_PRESENT`); `0` for a `full`/`partial` entry or for a stub whose return
    /// is `void`/`VkBool32`(→`VK_FALSE`)/pointer(→NULL)/integer(→0). Never `VK_SUCCESS` for a stub that
    /// returns `VkResult`.
    pub vk_error: i32,
    /// The command's Vulkan origin: `"core:1.0"`..`"core:1.3"` or `"ext:VK_KHR_swapchain"` (or
    /// `"ext:(unlisted)"` for a platform/vulkansc command with no plain-`vulkan` origin).
    pub origin: &'static str,
    /// Human-readable supported-domain / reason note (empty for a plain `full`).
    pub note: &'static str,
}

impl Entry {
    /// Whether this entry has a real (full or partial) body — i.e. it is NOT a default stub.
    pub fn implemented(&self) -> bool {
        !matches!(self.cap, Cap::Stub)
    }
}

/// The Vulkan API version this ICD advertises, as `(major, minor)`. Truthfulness: we advertise **1.1**
/// now that the entire 1.1 mandatory core has real bodies (`vkEnumerateInstanceVersion` and the
/// physical-device `apiVersion` both report this; a 1.2+ app request is rejected with
/// `VK_ERROR_INCOMPATIBLE_DRIVER`). The inventory test cross-checks this against `state::DD_API_VERSION`.
pub const ADVERTISED_API_VERSION: (u32, u32) = (1, 1);

/// The instance extensions the ICD advertises (must equal what `vkEnumerateInstanceExtensionProperties`
/// returns) — the allow-list of what is actually implemented, not everything `vk.xml` lists.
pub const ADVERTISED_INSTANCE_EXTENSIONS: &[&str] =
    &["VK_KHR_surface", "VK_KHR_wayland_surface", "VK_KHR_get_physical_device_properties2"];

/// The device extensions the ICD advertises (must equal what `vkEnumerateDeviceExtensionProperties`
/// returns).
pub const ADVERTISED_DEVICE_EXTENSIONS: &[&str] = &[
    "VK_KHR_swapchain",
    // Modern extensions wgpu-on-Vulkan / Zed require — advertised only because really implemented
    // (see `crate::ext`): timeline semaphores, dynamic rendering, buffer device address.
    "VK_KHR_timeline_semaphore",
    "VK_KHR_dynamic_rendering",
    "VK_KHR_buffer_device_address",
];

// The generated inventory (`CAPABILITIES`, `CAP_FULL`, `CAP_PARTIAL`, `CAP_STUB`, and the
// Vulkan-1.0 mandatory-core census `CORE_1_0_*`) is emitted by build.rs and `include!`d at the crate
// root (see lib.rs) so it can name `crate::capability::{Entry, Cap}`.
