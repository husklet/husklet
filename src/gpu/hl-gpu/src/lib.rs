//! hl-gpu — the v2-layered staging rewrite of hl's GPU core.
//!
//! This crate currently implements the **`protocol`** module: the versioned, platform-neutral command
//! language every driver submits through, plus the `CommandSink` port that carries it. It is ported from
//! the shipping `hl-gpu` crate and is **wire byte-identical** to it — same [`protocol::WIRE_VERSION`],
//! same tag numbers, same field order — so a stream encoded by old `hl-gpu` decodes here and vice-versa.
//!
//! Layering (v2 doctrine): `model/` owns the values + invariants, `codec/` owns serialization, `port/`
//! owns the boundary trait. `protocol/` is a **leaf**: no cuda/vulkan/gl/wgpu/Metal/fd/IOSurface/DRM
//! type appears anywhere in it, so it compiles for a guest-Linux target and the host alike. Shader
//! payloads are classified by a neutral magic ([`protocol::model::kernel::KERNEL_MAGIC`]) — the decoder
//! never reaches into a CUDA/PTX constant.

pub mod cpu;
pub mod protocol;
pub mod runtime;
pub mod transport;

// Ergonomic re-exports so downstream reads `hl_gpu::{GpuError, Result, Cmd, …}`.
pub use protocol::codec::{Decoder, Encoder};
pub use protocol::model::capability::{Capabilities, FeatureRequest, PresentKind};
pub use protocol::model::command::{Cmd, CommandBuffer, Enc, ShaderPayloadKind, WIRE_VERSION};
pub use protocol::model::descriptor::{FrameSerial, SurfaceToken};
pub use protocol::model::error::{GpuError, Result};
pub use protocol::model::id::{
    BindGroupId, BufferId, FenceId, PipelineId, ResourceTable, SamplerId, ShaderId, SurfaceId,
    TextureId,
};
pub use protocol::port::sink::{CommandSink, FenceWait, RecordingSink};

// Runtime layer: the per-connection Session that validates + accounts a decoded batch and dispatches it
// to an injected GpuExecutor (built on top of `protocol`).
pub use runtime::{
    Clock, ExportId, Exports, FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink, Ledger,
    Limits, Presentation, Session, SessionResources, SharedSync, SyncExportId, SyncExports,
    SystemClock,
};
pub use transport::{
    serve, serve_connection, serve_connection_with_handler, ConnectionHandler, GpuAlloc,
    ReadbackRequest, RemoteCommandSink, Surface, TransportConfig, TransportConfigError,
    TransportError, TransportPhase,
};

// The reference CPU executor (the semantic oracle): a pure, platform-free `GpuExecutor` a composition
// root injects for socket-free CPU execution and against which every real executor is conformance-checked.
pub use cpu::CpuExecutor;
