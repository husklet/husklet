//! A runtime-neutral **device-integration seam**: how an external backend (a GPU, an accelerator, a
//! display bridge, …) tells a container launch what host resources it needs — WITHOUT the runtime ever
//! learning what that backend *is*. hl-jit knows only "some mounts, some env, maybe a synthetic device
//! node"; the concrete meaning (CUDA shims, compositor sockets, an IOSurface render node) lives entirely
//! in the provider's own crate (e.g. `hl-gpu`), which implements [`DeviceProvider`] and hands the
//! resulting [`DeviceRequest`] to [`ContainerBuilder::apply_device`](super::ContainerBuilder::apply_device).
//!
//! This keeps GPU/CUDA/display specifics OUT of the runtime: hl-jit gains no dependency on, and no
//! vocabulary from, any particular device backend.

/// One host→guest bind mount a device backend needs (a shim library, a helper binary, a socket — the
/// runtime treats them all identically).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMount {
    /// Absolute host path to expose.
    pub host: String,
    /// Absolute guest path it appears at.
    pub container: String,
    /// Mount read-only (`true`) or read-write (`false`).
    pub read_only: bool,
}

impl DeviceMount {
    /// A read-only bind (the common case for injected libraries/binaries).
    pub fn ro(host: impl Into<String>, container: impl Into<String>) -> Self {
        DeviceMount { host: host.into(), container: container.into(), read_only: true }
    }
    /// A read-write bind (e.g. a socket the guest connects to).
    pub fn rw(host: impl Into<String>, container: impl Into<String>) -> Self {
        DeviceMount { host: host.into(), container: container.into(), read_only: false }
    }
}

/// Everything a device backend asks a container launch to add, in runtime-neutral terms. The runtime
/// applies each part generically (see [`ContainerBuilder::apply_device`](super::ContainerBuilder::apply_device)):
/// binds the [`mounts`](Self::mounts), folds the extra [`env`](Self::env) into the guest environment, and —
/// if [`render_node`](Self::render_node) — asks the backend to synthesize a host-backed device/render node.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceRequest {
    /// Host→guest bind mounts to add (libraries, tools, sockets — the runtime does not distinguish them).
    pub mounts: Vec<DeviceMount>,
    /// Extra guest environment as `K=V` lines, appended verbatim to the guest env (they go through the
    /// normal docker env dedup, so a later assignment of a key wins).
    pub env: Vec<String>,
    /// Request the backend synthesize a host-backed synthetic device node (the accelerated "render node"
    /// rung). Off = the whole device path stays inert.
    pub render_node: bool,
}

/// Something that can describe its device-integration needs to a container launch in runtime-neutral
/// terms. The implementor lives in the backend's own crate and holds all backend-specific knowledge; the
/// runtime only ever sees the [`DeviceRequest`] it returns.
pub trait DeviceProvider {
    /// Produce the mounts / env / device-node this backend needs for a launch. `guest_env` is the
    /// container's current merged guest environment (`K=V` lines), so a provider can compose against it —
    /// e.g. prepend its library dir to an existing `LD_LIBRARY_PATH`. The runtime folds the returned
    /// [`DeviceRequest::env`] in afterwards.
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest;
}
