//! dd-shim-common — the guest-side foundation for every dd GPU shim (GL, Vulkan, CUDA).
//!
//! This is the ONLY-place-the-wire-lives layer, lifted out of the hand-rolled C in
//! `dd-tests/guests/gl_shim.c` so that `dd-shim-gl`, `dd-shim-vk`, and `dd-shim-cuda` can all be
//! sibling crates on top of it. It owns three things:
//!
//! 1. **The shared IR contract** ([`ir`], [`wire`], [`id`]) — *re-exported* from the host's `dd-gpu`
//!    crate, never redefined. The guest producer encodes with the very same [`ir::Cmd`] /
//!    [`ir::encode_stream`] the host executor decodes with [`ir::Cmd::decode`], so the two sides
//!    share one Rust type and one encode/decode implementation and *cannot drift*. (This is what
//!    gl_shim.c's hand-rolled `iu8/iu32/istr` and its magic tag numbers `iu8(8)` used to duplicate.)
//!
//! 2. **The host-exec transport** ([`transport`]) — the persistent Unix-socket channel to the host
//!    GPU-exec service (`$DD_GPU_EXEC`), the `[hdr][ir]`+ack frame protocol, lazy reconnect with the
//!    residency-reset signal, and the completion-wake seam (eventfd/futex doorbell for the future
//!    shared-memory ring).
//!
//! 3. **Guest GPU-memory registration** ([`transport::renderd`]) — the `renderD128`
//!    `DD_IOCTL_GPU_ALLOC` that mints the rung-2 IOSurface-backed dma-buf a frame renders into.
//!
//! Cross-compiles to the guest ELF targets (aarch64 native on this host; x86_64 needs the
//! `x86_64-unknown-linux-gnu` rust-std — see `docs/rendering/SHIM_RUST_ARCHITECTURE.md`).

// ---- The shared IR contract, re-exported verbatim from the host crate. -----------------------------
// A guest shim `use dd_shim_common::ir::Cmd;` gets the exact type the host replays. There is no second
// definition to keep in sync — the "one source of truth" the architecture demands.
pub use dd_gpu::id;
pub use dd_gpu::ir;
pub use dd_gpu::wire;
pub use dd_gpu::{GpuError, Result};

pub mod transport;

/// The dd-gpu wire/protocol version the guest and host must agree on. Re-exported for shims that want
/// to stamp it into a handshake; it tracks `dd_gpu::ir` because that is literally the code both sides
/// run.
pub const IR_WIRE_VERSION: u32 = 1;
