//! The negotiated capability descriptor — the value types a backend advertises and a guest checks with
//! [`Capabilities::negotiate`] BEFORE advertising the matching API feature to the app.
//!
//! Value types + the pure bit helpers only; the handshake serialization lives in
//! [`crate::protocol::codec`]. An incompatible guest/backend pair fails cleanly here at negotiation
//! instead of surfacing as a runtime `BadTag`/`Unsupported` after the app already committed to a path.

use super::command::etag;
use super::enums::TextureFormat;
use super::error::{GpuError, Result};

/// Where a presented frame's pixels live, so the HLP layer can attach the right buffer kind. The variant
/// names are neutral tags — no platform handle type appears in the protocol; the real fd/mach-port is
/// passed out-of-band and correlated by request id at the transport layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresentKind {
    /// POSIX-shm region (CPU / software path).
    Shm,
    /// IOSurface handed over a mach-port (Apple GPU path).
    IoSurface,
    /// dma-buf fd (Linux/discrete host GPU path).
    DmaBuf,
}

/// Shader payload kinds a backend can consume, as a `u32` bitset. A guest front end negotiates its shader
/// ABI against a backend's advertised set before advertising a feature to the app.
pub mod shader_payload {
    /// SPIR-V words (the Vulkan ABI).
    pub const SPIRV: u32 = 1 << 0;
    /// GLSL-ES / GLSL source (translated host-side).
    pub const GLSL: u32 = 1 << 1;
    /// Metal Shading Language source/bytes.
    pub const MSL: u32 = 1 << 2;
    /// WGSL source (wgpu-native).
    pub const WGSL: u32 = 1 << 3;
    /// Neutral kernel descriptor (compiled to hl-GPU kernel IR).
    pub const KERNEL: u32 = 1 << 4;
}

/// Present-kind bitset used by the serialized handshake (a `Vec<PresentKind>` is not wire-friendly).
pub(crate) mod present_bit {
    pub const SHM: u32 = 1 << 0;
    pub const IOSURFACE: u32 = 1 << 1;
    pub const DMABUF: u32 = 1 << 2;
}

/// Build a supported-command bitset from a slice of [`etag`] encoder-op tag numbers.
pub fn command_bits(etags: &[u8]) -> u64 {
    let mut b = 0u64;
    for &t in etags {
        if t < 64 {
            b |= 1u64 << t;
        }
    }
    b
}

/// Build a supported-format bitset (bit = `TextureFormat::to_u32()`).
pub fn format_bits(formats: &[TextureFormat]) -> u32 {
    let mut b = 0u32;
    for f in formats {
        let n = f.to_u32();
        if n < 32 {
            b |= 1u32 << n;
        }
    }
    b
}

/// The color formats every host backend can materialize (the non-depth `TextureFormat`s).
pub const COLOR_FORMATS: &[TextureFormat] = &[
    TextureFormat::Rgba8Unorm,
    TextureFormat::Bgra8Unorm,
    TextureFormat::Rgba8Srgb,
    TextureFormat::Bgra8Srgb,
    TextureFormat::R8Unorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::Rgba16Float,
    TextureFormat::Rgba32Float,
    TextureFormat::R32Float,
];

/// The depth/stencil formats a backend can materialize as a real depth target (the software oracle
/// runs the per-fragment depth test against a `Depth32Float` plane).
pub const DEPTH_FORMATS: &[TextureFormat] = &[TextureFormat::Depth32Float];

/// Every encoder-op tag in the current IR (a backend that replays the whole set advertises this).
pub const ALL_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS, etag::END_RENDER_PASS, etag::SET_PIPELINE, etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER, etag::SET_INDEX_BUFFER, etag::SET_VIEWPORT, etag::SET_SCISSOR,
    etag::CLEAR_RECT, etag::DRAW, etag::DRAW_INDEXED, etag::BEGIN_COMPUTE_PASS, etag::END_COMPUTE_PASS,
    etag::DISPATCH, etag::COPY_B2B, etag::COPY_B2T, etag::COPY_T2B, etag::COPY_T2T, etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE, etag::FILL_BUFFER, etag::SET_STENCIL_REFERENCE,
];

/// A versioned, serialized capability descriptor a backend advertises and a guest negotiates against
/// BEFORE advertising the corresponding API feature to the app. It names the exact wire version,
/// encoder-command set, shader payloads, texture formats, present kinds, and size/limit ceilings, so an
/// incompatible pair fails cleanly at negotiation instead of surfacing as a runtime `BadTag` later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub name: String,
    /// True on integrated GPUs: CPU and GPU share memory, so buffer uploads are zero-copy.
    pub unified_memory: bool,
    pub supports_compute: bool,
    pub supports_graphics: bool,
    pub max_texture_2d: u32,
    /// The present buffer kinds this backend can hand to HLP.
    pub present_kinds: Vec<PresentKind>,

    // --- versioned negotiation descriptor ---
    /// hl-gpu IR wire version this backend decodes; a guest with a different
    /// [`super::command::WIRE_VERSION`] must be rejected before any command flows.
    pub wire_version: u32,
    /// Bitset of encoder-op tags ([`etag`]) this backend replays. Bit N = etag N supported.
    pub command_bits: u64,
    /// Bitset of accepted shader payload kinds ([`shader_payload`]).
    pub shader_payloads: u32,
    /// Bitset of supported texture formats (bit = `TextureFormat::to_u32()`).
    pub texture_formats: u32,
    /// Largest single submitted frame (encoded IR byte-stream) the backend will accept.
    pub max_frame_bytes: u64,
    /// Largest single buffer/texture allocation the backend will accept.
    pub max_buffer_bytes: u64,
    /// Maximum bind groups per pipeline layout.
    pub max_bind_groups: u32,
    /// Whether the backend implements a real external timeline-fence primitive (vs. emulating a fence
    /// with submission completion). Advertised truthfully so a guest cannot promise cross-process sync.
    pub supports_timeline_fences: bool,
}

/// A guest's required feature set, checked against a backend's [`Capabilities`] via
/// [`Capabilities::negotiate`] before the guest advertises the matching API feature to the app.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureRequest {
    pub wire_version: u32,
    /// Required shader payload bits ([`shader_payload`]).
    pub shader_payloads: u32,
    /// Required encoder-command bits (use [`command_bits`]).
    pub command_bits: u64,
    /// Required texture-format bits (use [`format_bits`]).
    pub texture_formats: u32,
}

impl Capabilities {
    /// True if the backend replays encoder op `etag`.
    pub fn supports_command(&self, etag: u8) -> bool {
        etag < 64 && self.command_bits & (1u64 << etag) != 0
    }
    /// True if the backend accepts shader payload `bit` (a [`shader_payload`] constant).
    pub fn supports_shader_payload(&self, bit: u32) -> bool {
        self.shader_payloads & bit != 0
    }
    /// True if the backend can materialize texture `format`.
    pub fn supports_format(&self, format: TextureFormat) -> bool {
        let n = format.to_u32();
        n < 32 && self.texture_formats & (1u32 << n) != 0
    }

    /// Negotiate a guest's [`FeatureRequest`] against this descriptor. Returns a typed, clean error the
    /// guest can act on — NOT a runtime `BadTag` after the app already committed to the path. Every bit
    /// the guest requires must be present in the advertisement.
    pub fn negotiate(&self, req: &FeatureRequest) -> Result<()> {
        if req.wire_version != self.wire_version {
            return Err(GpuError::Unsupported("capability: wire version mismatch"));
        }
        if req.shader_payloads & !self.shader_payloads != 0 {
            return Err(GpuError::Unsupported("capability: shader payload not supported"));
        }
        if req.command_bits & !self.command_bits != 0 {
            return Err(GpuError::Unsupported("capability: command tag not supported"));
        }
        if req.texture_formats & !self.texture_formats != 0 {
            return Err(GpuError::Unsupported("capability: texture format not supported"));
        }
        Ok(())
    }

    /// The present-kind bitset for this descriptor (used by the handshake codec).
    pub(crate) fn present_bits(&self) -> u32 {
        let mut b = 0u32;
        for k in &self.present_kinds {
            b |= match k {
                PresentKind::Shm => present_bit::SHM,
                PresentKind::IoSurface => present_bit::IOSURFACE,
                PresentKind::DmaBuf => present_bit::DMABUF,
            };
        }
        b
    }

    /// Reconstruct the present-kind list from a handshake bitset (used by the handshake codec).
    pub(crate) fn present_kinds_from_bits(pbits: u32) -> Vec<PresentKind> {
        let mut present_kinds = Vec::new();
        if pbits & present_bit::SHM != 0 {
            present_kinds.push(PresentKind::Shm);
        }
        if pbits & present_bit::IOSURFACE != 0 {
            present_kinds.push(PresentKind::IoSurface);
        }
        if pbits & present_bit::DMABUF != 0 {
            present_kinds.push(PresentKind::DmaBuf);
        }
        present_kinds
    }

    /// A permissive descriptor advertising the full current IR surface — every encoder command, every
    /// color format, all shader payloads, all present kinds — at [`super::command::WIRE_VERSION`]. Used
    /// by the software oracle and test doubles that accept anything the guest can encode.
    pub fn full(name: impl Into<String>) -> Capabilities {
        Capabilities {
            name: name.into(),
            unified_memory: true,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 16384,
            present_kinds: vec![PresentKind::Shm, PresentKind::IoSurface, PresentKind::DmaBuf],
            wire_version: super::command::WIRE_VERSION,
            command_bits: command_bits(ALL_COMMANDS),
            shader_payloads: shader_payload::SPIRV
                | shader_payload::GLSL
                | shader_payload::MSL
                | shader_payload::WGSL
                | shader_payload::KERNEL,
            texture_formats: format_bits(COLOR_FORMATS),
            // Browser-class per-frame wire-byte ceiling (hostile-DoS guard, not a correctness bound; the
            // `GlobalLedger` is the true host-OOM guard). Raised from 64 MiB — mis-sized for a browser, whose
            // real frames run to 89–168 MB — to a finite 256 MiB. See the wgpu executor for the full rationale.
            max_frame_bytes: 256 << 20,
            max_buffer_bytes: 1 << 30,
            max_bind_groups: 8,
            supports_timeline_fences: true,
        }
    }
}
