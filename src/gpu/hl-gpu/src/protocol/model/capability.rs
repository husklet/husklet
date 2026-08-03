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

/// Cross-API shader execution guarantees proved by an executor.
pub mod gpu_feature {
    /// Buffer accesses are confined to the bound resource. Out-of-bounds reads cannot expose another
    /// resource and out-of-bounds writes cannot modify one.
    pub const ROBUST_BUFFER_ACCESS: u32 = 1 << 0;
    /// Fragment shaders may write and perform atomics on storage resources.
    pub const FRAGMENT_STORES_ATOMICS: u32 = 1 << 1;
    pub const DEPTH_BIAS_CLAMP: u32 = 1 << 2;
    pub const IMAGE_CUBE_ARRAY: u32 = 1 << 3;
    pub const INDEPENDENT_BLEND: u32 = 1 << 4;
    pub const SAMPLE_RATE_SHADING: u32 = 1 << 5;
}

/// Present-kind bitset used by the serialized handshake (a `Vec<PresentKind>` is not wire-friendly).
pub(crate) mod present_bit {
    pub const SHM: u32 = 1 << 0;
    pub const IOSURFACE: u32 = 1 << 1;
    pub const DMABUF: u32 = 1 << 2;
}

/// Build a supported-command bitset from a slice of [`etag`] encoder-op tag numbers.
impl Capabilities {
    pub fn command_bits(etags: &[u8]) -> u64 {
        let mut b = 0u64;
        for &t in etags {
            if t < 64 {
                b |= 1u64 << t;
            }
        }
        b
    }
}

/// Build a supported-format bitset (bit = `TextureFormat::to_u32()`).
///
/// 128 slots wide. A discriminant at or beyond that is dropped rather than aliasing onto another format's
/// bit — it would be a silent capability lie, so
/// `every_declared_texture_format_is_representable_in_the_bitset` guards the invariant that no declared
/// format is ever out of range.
impl TextureFormat {
    pub fn bits(formats: &[TextureFormat]) -> u128 {
        let mut b = 0u128;
        for f in formats {
            let n = f.to_u32();
            if n < 128 {
                b |= 1u128 << n;
            }
        }
        b
    }
}

/// The colour formats EVERY host backend can materialize in full — allocate, clear, draw into, blend and
/// sample.
///
/// This was described as "the non-depth `TextureFormat`s", which is not what it is and is the dangerous
/// direction to be wrong in. Two whole families of non-depth format are deliberately absent:
/// [`INTEGER_FORMATS`], whose texels have no normalized reading for the software oracle's blend and
/// sample paths, and [`BC_FORMATS`], which are block-compressed. A reader who trusted the old rule and
/// added a new non-depth format here would be advertising, on behalf of every backend, a capability the
/// oracle does not have — and a format offered by one part of a driver and refused by another is the
/// defect that has cost this project the most.
///
/// Membership is an obligation, not a description: `clear_texel` must pack every format in this list,
/// which `protocol::model::enums::clear_texel_tests` asserts. Adding one without that is how the float
/// formats came to be promised here while both clear paths refused them.
pub const COLOR_FORMATS: &[TextureFormat] = &[
    TextureFormat::Rgba8Unorm,
    TextureFormat::Bgra8Unorm,
    TextureFormat::Rgba8Srgb,
    TextureFormat::Bgra8Srgb,
    TextureFormat::R8Unorm,
    TextureFormat::Rg8Unorm,
    TextureFormat::Rgba16Float,
    TextureFormat::Rgba16Unorm,
    TextureFormat::Rg16Unorm,
    TextureFormat::Rgba32Float,
    TextureFormat::R32Float,
    TextureFormat::Rg32Float,
];

/// The INTEGER color formats — the storage a `usampler2D`/`isampler2D` reads (`GL_RGBA_INTEGER`,
/// `GL_RED_INTEGER`, `GL_RG_INTEGER`).
///
/// Deliberately NOT part of [`COLOR_FORMATS`]: that set is the formats *every* host backend can
/// materialize, and the software oracle cannot. Its BLEND and SAMPLE paths are defined on normalized float
/// channels, and an integer format has no normalized reading. A backend advertises this set only if it can
/// really carry raw integer texels, which today means the wgpu executor.
///
/// CLEARING them is no longer part of the gap. `TextureFormat::clear_texel` packs an integer target from
/// raw values, because the hardware load-op clear already does — it passes the same `[f32; 4]` to
/// `wgpu::Color`, which a Uint/Sint target reads as the integer. Leaving the emulated rectangle clear
/// refusing what the load op accepts would have made one driver's two clear routes disagree. Being
/// clearable is not being materializable in full, so this set is unchanged.
pub const INTEGER_FORMATS: &[TextureFormat] = &[
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba32Uint,
    TextureFormat::Rgba32Sint,
    TextureFormat::R32Uint,
    TextureFormat::R32Sint,
];

/// Additional formats with exact native wgpu/Metal representations. They are executor-specific rather
/// than part of `COLOR_FORMATS`: the CPU oracle does not implement their normalized/half-width storage.
pub const NATIVE_FORMATS: &[TextureFormat] = &[
    TextureFormat::R8Snorm,
    TextureFormat::Rg8Snorm,
    TextureFormat::Rgba8Snorm,
    TextureFormat::Rg16Float,
    TextureFormat::R16Float,
    TextureFormat::R16Uint,
    TextureFormat::R16Sint,
    TextureFormat::Rg16Uint,
    TextureFormat::Rg16Sint,
    TextureFormat::Rgba16Uint,
    TextureFormat::Rgba16Sint,
    TextureFormat::Rg32Uint,
    TextureFormat::Rg32Sint,
    TextureFormat::Rgb9e5Ufloat,
    TextureFormat::Rgb10a2Unorm,
    TextureFormat::Rgb10a2Uint,
    TextureFormat::Rg11b10Ufloat,
    TextureFormat::R5g6b5Unorm,
    TextureFormat::A1r5g5b5Unorm,
    TextureFormat::B4g4r4a4Unorm,
];

/// The depth/stencil formats a backend can materialize as a real depth target (the software oracle
/// runs the per-fragment depth test against a `Depth32Float` plane).
pub const DEPTH_FORMATS: &[TextureFormat] = &[TextureFormat::Depth32Float];

pub const BC_FORMATS: &[TextureFormat] = &[
    TextureFormat::Bc1RgbaUnorm,
    TextureFormat::Bc1RgbaSrgb,
    TextureFormat::Bc2RgbaUnorm,
    TextureFormat::Bc2RgbaSrgb,
    TextureFormat::Bc3RgbaUnorm,
    TextureFormat::Bc3RgbaSrgb,
    TextureFormat::Bc4RUnorm,
    TextureFormat::Bc4RSnorm,
    TextureFormat::Bc5RgUnorm,
    TextureFormat::Bc5RgSnorm,
    TextureFormat::Bc6hRgbUfloat,
    TextureFormat::Bc6hRgbFloat,
    TextureFormat::Bc7RgbaUnorm,
    TextureFormat::Bc7RgbaSrgb,
];

pub const ETC2_FORMATS: &[TextureFormat] = &[
    TextureFormat::Etc2Rgb8Unorm,
    TextureFormat::Etc2Rgb8Srgb,
    TextureFormat::Etc2Rgb8A1Unorm,
    TextureFormat::Etc2Rgb8A1Srgb,
    TextureFormat::Etc2Rgba8Unorm,
    TextureFormat::Etc2Rgba8Srgb,
    TextureFormat::EacR11Unorm,
    TextureFormat::EacR11Snorm,
    TextureFormat::EacRg11Unorm,
    TextureFormat::EacRg11Snorm,
];

/// Every encoder-op tag in the current IR (a backend that replays the whole set advertises this).
pub const ALL_COMMANDS: &[u8] = &[
    etag::BEGIN_RENDER_PASS,
    etag::END_RENDER_PASS,
    etag::SET_PIPELINE,
    etag::SET_BIND_GROUP,
    etag::SET_VERTEX_BUFFER,
    etag::SET_INDEX_BUFFER,
    etag::SET_VIEWPORT,
    etag::SET_SCISSOR,
    etag::CLEAR_RECT,
    etag::DRAW,
    etag::DRAW_INDEXED,
    etag::BEGIN_COMPUTE_PASS,
    etag::END_COMPUTE_PASS,
    etag::DISPATCH,
    etag::COPY_B2B,
    etag::COPY_B2T,
    etag::COPY_T2B,
    etag::COPY_B2T_REGION,
    etag::COPY_T2B_REGION,
    etag::COPY_T2T,
    etag::BLIT_TEXTURE,
    etag::RESOLVE_TEXTURE,
    etag::FILL_BUFFER,
    etag::SET_STENCIL_REFERENCE,
    etag::SET_BLEND_CONSTANT,
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
    /// Bitset of supported texture formats (bit = `TextureFormat::to_u32()`). 128 bits wide so every
    /// declared neutral format remains representable. Only occupied 32-bit words reach the wire — see
    /// [`Capabilities::encode`](crate::protocol::codec::encode).
    pub texture_formats: u128,
    /// Largest single submitted frame (encoded IR byte-stream) the backend will accept.
    pub max_frame_bytes: u64,
    /// Largest single buffer/texture allocation the backend will accept.
    pub max_buffer_bytes: u64,
    /// Maximum bind groups per pipeline layout.
    pub max_bind_groups: u32,
    /// Whether the backend implements a real external timeline-fence primitive (vs. emulating a fence
    /// with submission completion). Advertised truthfully so a guest cannot promise cross-process sync.
    pub supports_timeline_fences: bool,
    /// Supported descriptor-array resource kinds ([`binding_array`]).
    pub binding_arrays: u32,
    /// Array resource kinds that permit dynamically non-uniform shader indices.
    pub non_uniform_binding_arrays: u32,
    /// Independently negotiated shader execution guarantees ([`gpu_feature`]).
    pub gpu_features: u32,
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
    /// Required texture-format bits (use [`TextureFormat::bits`]).
    pub texture_formats: u128,
    pub binding_arrays: u32,
    pub non_uniform_binding_arrays: u32,
    pub gpu_features: u32,
}

pub mod binding_array {
    pub const UNIFORM_BUFFER: u32 = 1 << 0;
    pub const STORAGE_BUFFER: u32 = 1 << 1;
    pub const SAMPLED_TEXTURE: u32 = 1 << 2;
    pub const STORAGE_TEXTURE: u32 = 1 << 3;
    pub const SAMPLER: u32 = 1 << 4;
    pub const BUFFER: u32 = UNIFORM_BUFFER | STORAGE_BUFFER;
    pub const TEXTURE: u32 = SAMPLED_TEXTURE | STORAGE_TEXTURE;
    pub const ALL: u32 = BUFFER | TEXTURE | SAMPLER;
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
        n < 128 && self.texture_formats & (1u128 << n) != 0
    }

    /// Negotiate a guest's [`FeatureRequest`] against this descriptor. Returns a typed, clean error the
    /// guest can act on — NOT a runtime `BadTag` after the app already committed to the path. Every bit
    /// the guest requires must be present in the advertisement.
    pub fn negotiate(&self, req: &FeatureRequest) -> Result<()> {
        if req.wire_version != self.wire_version {
            return Err(GpuError::Unsupported("capability: wire version mismatch"));
        }
        if req.shader_payloads & !self.shader_payloads != 0 {
            return Err(GpuError::Unsupported(
                "capability: shader payload not supported",
            ));
        }
        if req.command_bits & !self.command_bits != 0 {
            return Err(GpuError::Unsupported(
                "capability: command tag not supported",
            ));
        }
        if req.texture_formats & !self.texture_formats != 0 {
            return Err(GpuError::Unsupported(
                "capability: texture format not supported",
            ));
        }
        if req.binding_arrays & !self.binding_arrays != 0 {
            return Err(GpuError::Unsupported(
                "capability: binding array kind not supported",
            ));
        }
        if req.non_uniform_binding_arrays & !self.non_uniform_binding_arrays != 0 {
            return Err(GpuError::Unsupported(
                "capability: non-uniform binding array indexing not supported",
            ));
        }
        if req.gpu_features & !self.gpu_features != 0 {
            return Err(GpuError::Unsupported(
                "capability: GPU feature not supported",
            ));
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

    /// A TEST FIXTURE for sinks that accept anything the guest can encode: every encoder command, all
    /// shader payloads, all present kinds, every binding-array kind and every [`gpu_feature`], at
    /// [`super::command::WIRE_VERSION`].
    ///
    /// Its texture formats are exactly [`COLOR_FORMATS`] — which is NOT "every colour format". Three
    /// families are absent, and this listed only two of them: depth/stencil ([`DEPTH_FORMATS`]),
    /// block-compressed ([`BC_FORMATS`]), and the INTEGER colour formats ([`INTEGER_FORMATS`]), which are
    /// colour formats by any ordinary reading of the words and are the one a reader would not expect. A
    /// test that needs an integer texture must widen its own capabilities deliberately rather than
    /// assuming this fixture already covers it — and widening THIS one to make such a test pass would
    /// claim integer support on behalf of every backend, including the oracle that cannot blend or sample
    /// them.
    ///
    /// It must never be advertised on behalf of a real executor: the standing rule is that a capability
    /// claim equals the intersection of what shim, IR, executor, compositor and presenter actually honour,
    /// and no executor in this workspace honours this whole set (the CPU oracle, for one, runs no graphics
    /// shader payload and emulates fences). A real executor builds its own descriptor from what it
    /// implements — see [`crate::cpu::CpuExecutor::capabilities`].
    ///
    /// For a fixture that also carries the block-compressed formats a Metal-class host reports, use
    /// [`Capabilities::metal_class_fixture`]; do not widen this one inline.
    pub fn permissive_fixture(name: impl Into<String>) -> Capabilities {
        Capabilities {
            name: name.into(),
            unified_memory: true,
            supports_compute: true,
            supports_graphics: true,
            max_texture_2d: 16384,
            present_kinds: vec![
                PresentKind::Shm,
                PresentKind::IoSurface,
                PresentKind::DmaBuf,
            ],
            wire_version: super::command::WIRE_VERSION,
            command_bits: Capabilities::command_bits(ALL_COMMANDS),
            shader_payloads: shader_payload::SPIRV
                | shader_payload::GLSL
                | shader_payload::MSL
                | shader_payload::WGSL
                | shader_payload::KERNEL,
            texture_formats: TextureFormat::bits(COLOR_FORMATS),
            // Browser-class per-frame wire-byte ceiling (hostile-DoS guard, not a correctness bound; the
            // `GlobalLedger` is the true host-OOM guard). Raised from 64 MiB — mis-sized for a browser, whose
            // real frames run to 89–168 MB — to a finite 256 MiB. See the wgpu executor for the full rationale.
            max_frame_bytes: 256 << 20,
            max_buffer_bytes: 1 << 30,
            max_bind_groups: 8,
            supports_timeline_fences: true,
            binding_arrays: binding_array::ALL,
            non_uniform_binding_arrays: binding_array::ALL,
            gpu_features: gpu_feature::ROBUST_BUFFER_ACCESS
                | gpu_feature::FRAGMENT_STORES_ATOMICS
                | gpu_feature::DEPTH_BIAS_CLAMP
                | gpu_feature::IMAGE_CUBE_ARRAY
                | gpu_feature::INDEPENDENT_BLEND
                | gpu_feature::SAMPLE_RATE_SHADING,
        }
    }

    /// A TEST FIXTURE for a Metal-class host: [`Capabilities::permissive_fixture`] plus the
    /// block-compressed formats ([`BC_FORMATS`]) such a host reports. This is what a shim asserting
    /// `textureCompressionBC` against a host advertisement should be checked against.
    pub fn metal_class_fixture(name: impl Into<String>) -> Capabilities {
        let mut capabilities = Self::permissive_fixture(name);
        capabilities.texture_formats |= TextureFormat::bits(BC_FORMATS);
        capabilities
    }

    /// A TEST FIXTURE for a CPU-oracle session: `base` (the oracle's own honest descriptor) widened to
    /// admit the SPIR-V/GLSL payloads it treats as opaque handles and the combined depth+stencil format it
    /// models but does not advertise. It changes nothing the oracle computes; it only lets an identical
    /// program past the runtime `validate` gate on both sides of a differential run.
    pub fn oracle_session_fixture(base: &Capabilities) -> Capabilities {
        let mut capabilities = base.clone();
        capabilities.shader_payloads |= shader_payload::SPIRV | shader_payload::GLSL;
        capabilities.texture_formats |= TextureFormat::bits(&[TextureFormat::Depth24PlusStencil8]);
        capabilities
    }
}
