//! Advertised capability truth: the instance/device extensions the driver really backs + the
//! per-`VkFormat` feature report. Pure data + pure functions (no `Cmd`, no sink), so the shim's
//! `vkEnumerate{Instance,Device}ExtensionProperties` / `vkGetPhysicalDeviceFormatProperties` entry
//! points read one authoritative, unit-tested source.
//!
//! Ported from `hl-shim-vk/src/{capability.rs,instance.rs}`: an extension is advertised ONLY when it is
//! really implemented here (the WSI swapchain present path in [`crate::service::present`], and the
//! `...2` physical-device property queries carried by `VK_KHR_get_physical_device_properties2`), NEVER
//! the whole `vk.xml` list — a claimed-but-unbacked extension is a lie a real app builds a broken path
//! on. The format masks mirror `MVKPixelFormats::getVkFormatProperties` for the color/depth subset the
//! render/transfer path materializes ([`crate::service::create::create_image`]).

#[cfg(test)]
use super::memory::vk_format;
use super::memory::VertexFormat;

/// One advertised extension: its `VK_KHR_*`/`VK_EXT_*` name + spec version (the two fields
/// `vkEnumerate{Instance,Device}ExtensionProperties` writes into each `VkExtensionProperties`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtensionProp {
    pub name: &'static str,
    pub spec_version: u32,
}

/// Instance-level extensions the ICD really backs: the WSI base (`VK_KHR_surface`), the Wayland WSI
/// surface platform (`VK_KHR_wayland_surface`, really backed by `vkCreateWaylandSurfaceKHR` +
/// `vkGetPhysicalDeviceWaylandPresentationSupportKHR` in [`crate::shim`]'s `surface` + the present path in
/// [`crate::adapter::wayland_app`]), and the `...2` physical-device property queries
/// (`VK_KHR_get_physical_device_properties2`). A real app (wgpu/ash/vkcube-wayland) gates its init on
/// these being enumerated and aborts otherwise — and the Vulkan loader only reports a platform WSI
/// surface extension when an ICD advertises it — so advertising them is the key unblock past instance
/// setup + Wayland-surface creation.
pub const INSTANCE_EXTENSIONS: &[ExtensionProp] = &[
    ExtensionProp {
        name: "VK_KHR_surface",
        spec_version: 25,
    },
    ExtensionProp {
        name: "VK_KHR_wayland_surface",
        spec_version: 6,
    },
    ExtensionProp {
        name: "VK_KHR_get_physical_device_properties2",
        spec_version: 2,
    },
    // The external-capability QUERY trio. All three were promoted to core in Vulkan 1.1 and this ICD
    // advertises 1.3, so their core entry points
    // (`vkGetPhysicalDeviceExternalBufferProperties`/`…SemaphoreProperties`/`…FenceProperties`, in
    // `devgroup.rs`) are already mandatory and already have real bodies: each reports all-zero
    // `externalMemoryFeatures` / `compatibleHandleTypes`, the truthful answer for a device that
    // supports no external handle types at all.
    //
    // Naming them costs nothing and changes no behaviour, and NOT naming them was the ladder's rule
    // running backwards — under-advertising something we do honour. A client targeting 1.0/1.1 asks for
    // the `KHR` spelling rather than the core one and was refused `VK_ERROR_EXTENSION_NOT_PRESENT` at
    // `vkCreateInstance` for a capability this driver genuinely has. Measured against the installed
    // bundle: these three were the only refused instance extensions whose entry points are really
    // implemented (`VK_KHR_get_surface_capabilities2`'s are in the refused list, and there is no X11 or
    // display path at all, so those stay unadvertised).
    ExtensionProp {
        name: "VK_KHR_external_memory_capabilities",
        spec_version: 1,
    },
    ExtensionProp {
        name: "VK_KHR_external_semaphore_capabilities",
        spec_version: 1,
    },
    ExtensionProp {
        name: "VK_KHR_external_fence_capabilities",
        spec_version: 1,
    },
];

/// Device-level extensions the ICD really backs: `VK_KHR_swapchain` (the present path in
/// [`crate::service::present`]) + `VK_KHR_dynamic_rendering` (the render-pass-object-free rendering path
/// in [`crate::service::record::cmd_begin_rendering`], really lowered to `Enc::BeginRenderPass`). Nothing
/// else is advertised — a `vk.xml` extension without a real body here (timeline semaphores, buffer device
/// address, …) would be a dishonest claim.
pub const DEVICE_EXTENSIONS: &[ExtensionProp] = &[
    ExtensionProp {
        name: "VK_KHR_swapchain",
        spec_version: 70,
    },
    ExtensionProp {
        name: "VK_KHR_dynamic_rendering",
        spec_version: 1,
    },
];

/// `VkFormatFeatureFlagBits` (stable bit values from vk.xml) — the per-format capability bits reported
/// in `VkFormatProperties`.
pub mod format_feature {
    pub const SAMPLED_IMAGE: u32 = 0x0000_0001;
    pub const STORAGE_IMAGE: u32 = 0x0000_0002;
    pub const UNIFORM_TEXEL_BUFFER: u32 = 0x0000_0008;
    pub const STORAGE_TEXEL_BUFFER: u32 = 0x0000_0010;
    pub const VERTEX_BUFFER: u32 = 0x0000_0040;
    pub const COLOR_ATTACHMENT: u32 = 0x0000_0080;
    pub const COLOR_ATTACHMENT_BLEND: u32 = 0x0000_0100;
    pub const DEPTH_STENCIL_ATTACHMENT: u32 = 0x0000_0200;
    pub const BLIT_SRC: u32 = 0x0000_0400;
    pub const BLIT_DST: u32 = 0x0000_0800;
    pub const SAMPLED_IMAGE_FILTER_LINEAR: u32 = 0x0000_1000;
    pub const TRANSFER_SRC: u32 = 0x0000_4000;
    pub const TRANSFER_DST: u32 = 0x0000_8000;
}

/// The three `VkFormatProperties` feature masks for a format: what it supports when linearly tiled,
/// optimally tiled, and as a buffer. Raw `VkFormatFeatureFlags` bitsets (the shim copies them straight
/// into the app's `VkFormatProperties`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FormatFeatures {
    pub linear_tiling: u32,
    pub optimal_tiling: u32,
    pub buffer: u32,
}

use crate::model::memory::Format;

/// The class a lowerable `VkFormat` belongs to. Every class is derived from the ONE wire format the
/// Vulkan format lowers to ([`crate::model::memory::Format::wire`]), so an advertisement can never again
/// name a format the driver cannot materialize, nor omit one it can.
///
/// The classes exist because their capabilities genuinely differ, not for tidiness: an integer color
/// format is unfilterable and unblendable by specification, a 32-bit float color format is renderable but
/// not filterable without a host feature this driver does not request, a block-compressed format is
/// sample-only, and a depth format is never a color attachment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FormatClass {
    /// 8-bit normalized/sRGB color: filterable, renderable, blendable.
    NormalizedColor,
    /// 8-bit integer color: renderable, but read only by `texelFetch` and never blended or filtered.
    IntegerColor,
    /// 16-bit float color: filterable, renderable, blendable.
    FloatColor,
    /// 32-bit float color: renderable, but unfilterable without the host `float32-filterable` feature,
    /// which this driver does not request.
    UnfilterableFloatColor,
    /// Block-compressed color: sampled and copied only — never a render target.
    Compressed,
    /// Depth/stencil: a depth-stencil attachment, never a color one.
    DepthStencil,
}

impl Format {
    /// Which class this `VkFormat` lowers into, or `None` when the driver has no encoding for it at all.
    pub fn class(&self) -> Option<FormatClass> {
        use hl_gpu::protocol::model::enums::TextureFormat as T;
        Some(match self.wire()? {
            T::Rgba8Unorm
            | T::Bgra8Unorm
            | T::Rgba8Srgb
            | T::Bgra8Srgb
            | T::R8Unorm
            | T::Rg8Unorm => FormatClass::NormalizedColor,
            T::Rgba8Uint
            | T::Rgba8Sint
            | T::R8Uint
            | T::R8Sint
            | T::Rg8Uint
            | T::Rg8Sint
            | T::R32Uint
            | T::R32Sint => FormatClass::IntegerColor,
            T::Rgba16Float => FormatClass::FloatColor,
            T::R32Float | T::Rgba32Float => FormatClass::UnfilterableFloatColor,
            T::Depth32Float | T::Depth24PlusStencil8 => FormatClass::DepthStencil,
            other if other.block_geometry().is_some() => FormatClass::Compressed,
            // Unreachable by construction: every variant above is enumerated. A new wire format reaches
            // this arm and is truthfully unadvertised until it is classified.
            _ => return None,
        })
    }

    /// Whether this format materializes as a color image (any of the three color classes).
    pub fn is_color(&self) -> bool {
        matches!(
            self.class(),
            Some(
                FormatClass::NormalizedColor
                    | FormatClass::IntegerColor
                    | FormatClass::FloatColor
                    | FormatClass::UnfilterableFloatColor
            )
        )
    }

    /// Whether `vk_format` is a depth/stencil format the render path materializes.
    pub fn is_depth(&self) -> bool {
        self.class() == Some(FormatClass::DepthStencil)
    }

    /// Whether an optimally-tiled image of this format is creatable — the subset
    /// `vkGetPhysicalDeviceImageFormatProperties` reports as supported. This is exactly the set the
    /// driver can lower: reporting anything narrower refuses images `vkCreateImage` would have created,
    /// and anything wider promises a format the executor would reject.
    pub fn is_image_supported(&self) -> bool {
        self.class().is_some()
    }

    /// `STORAGE_IMAGE` when the host really permits a storage binding of this format. The core WebGPU
    /// storage-texture set the executor targets is narrow: four-channel 8-bit unorm/integer, `rgba16float`
    /// and the 32-bit float formats. Notably it excludes every sRGB format and the one- and two-channel
    /// 8-bit formats, which the previous blanket "colour implies storage" claim asserted anyway.
    fn storage(&self) -> u32 {
        use hl_gpu::protocol::model::enums::TextureFormat as T;
        match self.wire() {
            Some(
                T::Rgba8Unorm
                | T::Rgba8Uint
                | T::Rgba8Sint
                | T::R32Uint
                | T::R32Sint
                | T::Rgba16Float
                | T::R32Float
                | T::Rgba32Float,
            ) => format_feature::STORAGE_IMAGE,
            _ => 0,
        }
    }

    /// The truthful per-format `VkFormatProperties` feature masks. A color format advertises
    /// color-attachment + blend + sampled + storage + blit + transfer; a depth/stencil format advertises
    /// depth-stencil-attachment + sampled + transfer (never color-attachment, and vice-versa); vertex float
    /// formats advertise `VERTEX_BUFFER`. Reporting the same flags for every format (the old all-broad stub)
    /// made a client build wrong per-format capabilities. Ported from
    /// `MVKPixelFormats::getVkFormatProperties`.
    /// # This table is currently wrong in BOTH directions, and they pull against each other
    ///
    /// Measured 2026-08-01 against `dEQP-VK.api.*` and `dEQP-VK.api.copy_and_blit.core.*`:
    ///
    /// * **Claimed and not honoured.** Bits advertised here that the recorder then refuses. A blit of an
    ///   integer format and a blit whose region spans depth are both refused at record time while the
    ///   format bits say yes — together 452 cases failing at `vkEndCommandBuffer` with
    ///   VK_ERROR_FEATURE_NOT_PRESENT. An application that queries correctly still fails, which is worse
    ///   than never advertising.
    /// * **Required and not claimed.** 30 formats — `r16_sfloat`, `r32_uint`, `a8b8g8r8_unorm_pack32`,
    ///   `r8g8b8a8_snorm` among them — lack a bit the specification mandates, which is what most of the
    ///   `dEQP-VK.api.info.*` failures are.
    ///
    /// **So a change that only widens, or only narrows, makes the other half worse.** Before editing:
    /// decide which of the two a format is in, and re-run BOTH `dEQP-VK.api.info.*` (10,481 cases, ~12s,
    /// catches required-and-missing) and the relevant `copy_and_blit` groups (catches
    /// claimed-and-refused). Neither suite alone can see the damage the other measures — that asymmetry
    /// is why the table drifted in two directions without either being noticed.
    ///
    /// A widening in particular must be paired with a path that serves it: an earlier format ungate moved
    /// 783 cases from honestly declined to running and failing, because three of four code paths learned
    /// the new capability and the fourth did not.
    pub fn features(&self) -> FormatFeatures {
        let vk_format = self.0;
        use format_feature as f;
        // Every color class carries the copy/blit/sample base; the differences between the classes are
        // filtering, blending and storage, each of which is a real host capability rather than a
        // presentation choice.
        const COLOR_BASE: u32 = f::SAMPLED_IMAGE
            | f::COLOR_ATTACHMENT
            | f::BLIT_SRC
            | f::BLIT_DST
            | f::TRANSFER_SRC
            | f::TRANSFER_DST;
        let optimal = match self.class() {
            Some(FormatClass::NormalizedColor) => {
                COLOR_BASE
                    | f::COLOR_ATTACHMENT_BLEND
                    | f::SAMPLED_IMAGE_FILTER_LINEAR
                    | self.storage()
            }
            // Integer color is unfilterable and unblendable BY SPECIFICATION — a shader reads it through
            // `texelFetch` only — so neither bit may be claimed however capable the host is.
            Some(FormatClass::IntegerColor) => COLOR_BASE | self.storage(),
            Some(FormatClass::FloatColor) => {
                COLOR_BASE
                    | f::COLOR_ATTACHMENT_BLEND
                    | f::SAMPLED_IMAGE_FILTER_LINEAR
                    | self.storage()
            }
            // 32-bit float sampling needs the host `float32-filterable` feature, which this driver does not
            // request, and 32-bit float blending is not a core host capability either.
            Some(FormatClass::UnfilterableFloatColor) => COLOR_BASE | self.storage(),
            // Block-compressed texels are decoded by the sampler and cannot be written by a render pass or
            // a blit destination, so only the read side is claimed.
            //
            // BLIT_SRC is not claimed either, and that is the correction rather than the obvious part.
            // It WAS claimed, and the blit recorder refuses every compressed format outright because a
            // block-compressed texel has no packed colour layout to resample — so the driver promised a
            // capability at query time and refused it at record time, which is worse than never
            // promising it: an application that checks `VkFormatProperties` correctly still failed.
            // That cost 636 cases in `dEQP-VK.api.copy_and_blit.core`, every one refused at
            // `vkEndCommandBuffer` with VK_ERROR_FEATURE_NOT_PRESENT.
            //
            // Dropping it is free here and was MEASURED to be, not assumed: BLIT_SRC becomes mandatory
            // for a compressed format only when that format's required set includes
            // SAMPLED_IMAGE_FILTER_LINEAR (CTS `getRequiredOptimalTilingFeatures`), which follows from
            // `textureCompressionBC`, which this driver does not advertise. Re-running
            // `dEQP-VK.api.info.*` with the bit removed left the failure count at exactly 184, and the
            // 624-case BC blit groups went from wholly failing to wholly NotSupported.
            //
            // Restore this bit only together with a blit path that can actually resample a compressed
            // source, and re-run both suites — the two move in opposite directions.
            Some(FormatClass::Compressed) => {
                f::SAMPLED_IMAGE
                    | f::SAMPLED_IMAGE_FILTER_LINEAR
                    | f::TRANSFER_SRC
                    | f::TRANSFER_DST
            }
            Some(FormatClass::DepthStencil) => {
                f::SAMPLED_IMAGE
                    | f::DEPTH_STENCIL_ATTACHMENT
                    | f::BLIT_SRC
                    | f::BLIT_DST
                    | f::TRANSFER_SRC
                    | f::TRANSFER_DST
            }
            None => 0,
        };
        // Buffer features are INDEPENDENT bits, not alternatives. The previous `match` was exclusive, so
        // a format that advertised VERTEX_BUFFER could not also advertise texel-buffer use and vice
        // versa, which is not how VkFormatProperties::bufferFeatures works.
        //
        // VERTEX_BUFFER is derived from the vertex lowering itself rather than from a second hand-written
        // list. It previously named three formats while `VertexFormat::wire` lowers thirty, so the driver
        // refused to admit to twenty-seven vertex formats it correctly supports — the same
        // under-advertising defect as the instance extensions, and the mirror of the bug where the driver
        // forwarded a VkFormat it had never lowered. Deriving it means the advertisement can never drift
        // from the lowering again: adding a format to `VertexFormat::wire` advertises it, and removing
        // one stops advertising it, in the same edit.
        //
        // KNOWN GAP, deliberate: Vulkan's mandatory-format table also requires VERTEX_BUFFER for the
        // single-component 8/16-bit formats and for BGRA orders. The neutral wire has no encoding for
        // those (no 1- or 3-component narrow formats, no component swizzle), so this driver genuinely
        // cannot lower them and does not claim them. Claiming them to satisfy the table would be the lie
        // the ladder forbids; closing the gap needs a wire change in hl-gpu.
        let mut buffer = 0;
        if VertexFormat(vk_format).wire().is_some() {
            buffer |= f::VERTEX_BUFFER;
        }
        if matches!(
            self.wire(),
            Some(
                hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm
                    | hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm
                    | hl_gpu::protocol::model::enums::TextureFormat::R8Unorm
                    | hl_gpu::protocol::model::enums::TextureFormat::Rg8Unorm
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba16Float
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba32Float
                    | hl_gpu::protocol::model::enums::TextureFormat::R32Float
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba8Uint
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba8Sint
                    | hl_gpu::protocol::model::enums::TextureFormat::R8Uint
                    | hl_gpu::protocol::model::enums::TextureFormat::R8Sint
                    | hl_gpu::protocol::model::enums::TextureFormat::Rg8Uint
                    | hl_gpu::protocol::model::enums::TextureFormat::Rg8Sint
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba32Uint
                    | hl_gpu::protocol::model::enums::TextureFormat::Rgba32Sint
                    | hl_gpu::protocol::model::enums::TextureFormat::R32Uint
                    | hl_gpu::protocol::model::enums::TextureFormat::R32Sint
            )
        ) {
            buffer |= f::UNIFORM_TEXEL_BUFFER | f::STORAGE_TEXEL_BUFFER;
        }
        FormatFeatures {
            // LINEAR tiling advertises NOTHING materializable. Vulkan's linear tiling exists so an app can
            // populate an image through host-mapped memory (`vkMapMemory` + memcpy) — but this backend stores
            // every image as a device-only wgpu texture and materializes texel content solely through device
            // commands (`vkCmdCopyBufferToImage` / render passes), never from host-mapped image memory. So
            // advertising e.g. SAMPLED for a linear image would be a capability lie: it steers an app (vkcube's
            // default texture path keys on `linearTilingFeatures & SAMPLED_IMAGE`) into a host-mapped upload
            // whose texels never reach the sampled texture — the cube then samples black. Reporting 0 forces the
            // staging-buffer + `vkCmdCopyBufferToImage` → optimal-image path this backend actually materializes.
            linear_tiling: 0,
            optimal_tiling: optimal,
            buffer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every format this driver can LOWER as a vertex attribute must also be ADVERTISED as usable in a
    /// vertex buffer. A well-behaved application (and the CTS) checks `bufferFeatures & VERTEX_BUFFER_BIT`
    /// before using a format, so a format we lower but do not advertise is one no careful caller will
    /// ever reach — the same under-advertising defect as an unnamed instance extension.
    #[test]
    fn every_lowerable_vertex_format_advertises_vertex_buffer() {
        let mut lowerable = 0;
        for format in 0..=200u32 {
            if VertexFormat(format).wire().is_none() {
                continue;
            }
            lowerable += 1;
            let features = Format(format).features();
            assert!(
                features.buffer & format_feature::VERTEX_BUFFER != 0,
                "VkFormat {format} lowers as a vertex attribute but bufferFeatures omits VERTEX_BUFFER"
            );
        }
        assert!(
            lowerable >= 30,
            "expected the lowerable set to be the 30 from VertexFormat, got {lowerable}"
        );
    }

    /// And the converse: nothing may claim VERTEX_BUFFER that the driver cannot lower, or an application
    /// that trusts the advertisement gets a pipeline refused at creation.
    #[test]
    fn nothing_advertises_a_vertex_format_it_cannot_lower() {
        for format in 0..=250u32 {
            let features = Format(format).features();
            if features.buffer & format_feature::VERTEX_BUFFER != 0 {
                assert!(
                    VertexFormat(format).wire().is_some(),
                    "VkFormat {format} advertises VERTEX_BUFFER but has no wire encoding"
                );
            }
        }
    }

    /// Buffer feature bits are independent, not alternatives: a colour format used as a vertex buffer is
    /// still a legal texel buffer. The exclusive `match` this replaced could report only one of them.
    #[test]
    fn buffer_features_are_not_mutually_exclusive() {
        // R8G8B8A8_UNORM is both a lowerable vertex format and a colour format, so it must carry BOTH
        // sets of buffer bits. The exclusive `match` this replaced could report only one.
        let features = Format(vk_format::R8G8B8A8_UNORM).features();
        assert!(
            features.buffer & format_feature::VERTEX_BUFFER != 0,
            "vertex buffer"
        );
        assert!(
            features.buffer & format_feature::UNIFORM_TEXEL_BUFFER != 0,
            "a colour format is also a uniform texel buffer"
        );
    }

    #[test]
    fn texel_buffer_claims_match_every_raw_lowerable_wire_format() {
        use hl_gpu::protocol::model::enums::TextureFormat as T;

        for format in 0..=250u32 {
            let wire = Format(format).wire();
            let supported = matches!(
                wire,
                Some(
                    T::Rgba8Unorm | T::Bgra8Unorm | T::R8Unorm | T::Rg8Unorm
                        | T::Rgba16Float | T::Rgba32Float | T::R32Float
                        | T::Rgba8Uint | T::Rgba8Sint | T::R8Uint | T::R8Sint
                        | T::Rg8Uint | T::Rg8Sint | T::Rgba32Uint | T::Rgba32Sint
                        | T::R32Uint | T::R32Sint
                )
            );
            let bits = Format(format).features().buffer
                & (format_feature::UNIFORM_TEXEL_BUFFER | format_feature::STORAGE_TEXEL_BUFFER);
            assert_eq!(bits != 0, supported, "VkFormat {format}, wire={wire:?}");
            assert_eq!(
                bits,
                if supported {
                    format_feature::UNIFORM_TEXEL_BUFFER | format_feature::STORAGE_TEXEL_BUFFER
                } else {
                    0
                },
                "VkFormat {format}, wire={wire:?}"
            );
        }
    }

    /// The advertisement and the lowering are now ONE source: every format the driver can lower is
    /// classified, and only those are. The previous form of this test recorded the opposite — a drift of
    /// twenty-odd formats that `Format::wire` lowered while `is_color`'s hand-written four-format list
    /// refused to advertise — which is what made `vkGetPhysicalDeviceImageFormatProperties` report
    /// VK_ERROR_FORMAT_NOT_SUPPORTED for formats this driver genuinely materializes.
    #[test]
    fn every_lowerable_format_is_classified_and_advertised() {
        let mut lowerable = 0;
        for format in 0..=250u32 {
            if Format(format).wire().is_none() {
                assert!(
                    Format(format).class().is_none(),
                    "VkFormat {format} is classified but has no wire encoding"
                );
                continue;
            }
            lowerable += 1;
            assert!(
                Format(format).class().is_some(),
                "VkFormat {format} lowers as a texture but has no format class"
            );
            assert!(
                Format(format).is_image_supported(),
                "VkFormat {format} lowers as a texture but is refused image creation"
            );
            assert_ne!(
                Format(format).features().optimal_tiling,
                0,
                "VkFormat {format} lowers as a texture but advertises no optimal-tiling feature"
            );
        }
        assert!(
            lowerable >= 30,
            "expected at least the 30 lowerable texture formats, got {lowerable}"
        );
    }

    /// Every class carries the transfer bits, because a copy is the one operation this driver performs on
    /// every image it can create. A class that advertised no transfer would be an image no caller could
    /// populate.
    #[test]
    fn every_class_is_transferable_both_ways() {
        for format in 0..=250u32 {
            let Some(class) = Format(format).class() else {
                continue;
            };
            let optimal = Format(format).features().optimal_tiling;
            assert_ne!(
                optimal & format_feature::TRANSFER_SRC,
                0,
                "{class:?} VkFormat {format} is not transfer-src"
            );
            assert_ne!(
                optimal & format_feature::TRANSFER_DST,
                0,
                "{class:?} VkFormat {format} is not transfer-dst"
            );
        }
    }

    /// Integer colour is unfilterable and unblendable by specification — a shader reads it only through
    /// `texelFetch`. Claiming either bit steers a caller into a sampler configuration the host refuses.
    #[test]
    fn integer_color_claims_no_filtering_or_blending() {
        for format in [
            vk_format::R8G8B8A8_UINT,
            vk_format::R8G8B8A8_SINT,
            vk_format::R8_UINT,
            vk_format::R8_SINT,
            vk_format::R8G8_UINT,
            vk_format::R8G8_SINT,
        ] {
            assert_eq!(Format(format).class(), Some(FormatClass::IntegerColor));
            let optimal = Format(format).features().optimal_tiling;
            assert_ne!(optimal & format_feature::COLOR_ATTACHMENT, 0);
            assert_eq!(optimal & format_feature::SAMPLED_IMAGE_FILTER_LINEAR, 0);
            assert_eq!(optimal & format_feature::COLOR_ATTACHMENT_BLEND, 0);
        }
    }

    /// 32-bit float sampling needs a host feature this driver does not request; 16-bit float filtering is
    /// core. The two float classes must therefore differ in exactly that bit.
    #[test]
    fn float_classes_differ_by_filtering() {
        assert_ne!(
            Format(vk_format::R16G16B16A16_SFLOAT)
                .features()
                .optimal_tiling
                & format_feature::SAMPLED_IMAGE_FILTER_LINEAR,
            0
        );
        for format in [vk_format::R32_SFLOAT, vk_format::R32G32B32A32_SFLOAT] {
            assert_eq!(
                Format(format).features().optimal_tiling
                    & format_feature::SAMPLED_IMAGE_FILTER_LINEAR,
                0,
                "VkFormat {format} may not claim linear filtering"
            );
        }
    }

    /// D16 lowers onto the 32-bit float depth target, so it is a depth format like any other. The
    /// hand-written `is_depth` list omitted it, which made a D16 depth image report itself uncreatable
    /// while `vkCreateImage` created it happily.
    #[test]
    fn every_lowerable_depth_format_is_depth() {
        for format in [
            vk_format::D16_UNORM,
            vk_format::D32_SFLOAT,
            vk_format::D24_UNORM_S8_UINT,
        ] {
            assert!(Format(format).is_depth(), "VkFormat {format} is not depth");
            assert!(!Format(format).is_color());
        }
    }

    /// sRGB and the narrow one/two-channel 8-bit formats are not core storage-texture formats. The old
    /// blanket "colour implies storage" claim asserted them anyway.
    #[test]
    fn storage_is_claimed_only_where_the_host_permits_it() {
        for format in [
            vk_format::R8G8B8A8_SRGB,
            vk_format::B8G8R8A8_SRGB,
            vk_format::B8G8R8A8_UNORM,
            vk_format::R8_UNORM,
            vk_format::R8G8_UNORM,
        ] {
            assert_eq!(
                Format(format).features().optimal_tiling & format_feature::STORAGE_IMAGE,
                0,
                "VkFormat {format} is not a core storage-texture format"
            );
        }
        assert_ne!(
            Format(vk_format::R8G8B8A8_UNORM).features().optimal_tiling
                & format_feature::STORAGE_IMAGE,
            0
        );
    }

    /// Block-compressed texels are produced by an offline encoder, not by a render pass or a blit
    /// destination. Advertising either would steer a caller into a write this driver cannot perform.
    #[test]
    fn compressed_is_read_only() {
        let optimal = Format(vk_format::BC7_UNORM_BLOCK).features().optimal_tiling;
        assert_eq!(
            Format(vk_format::BC7_UNORM_BLOCK).class(),
            Some(FormatClass::Compressed)
        );
        assert_ne!(optimal & format_feature::SAMPLED_IMAGE, 0);
        assert_eq!(optimal & format_feature::COLOR_ATTACHMENT, 0);
        assert_eq!(optimal & format_feature::BLIT_DST, 0);
        // Nor BLIT_SRC: the recorder refuses a compressed blit on either side, so claiming the source
        // half was a promise the driver could not keep. Measured free to drop — see `features`.
        assert_eq!(optimal & format_feature::BLIT_SRC, 0);
    }

    #[test]
    fn instance_extensions_advertise_surface_and_pdp2() {
        let names: Vec<&str> = INSTANCE_EXTENSIONS.iter().map(|e| e.name).collect();
        assert!(names.contains(&"VK_KHR_surface"));
        assert!(names.contains(&"VK_KHR_wayland_surface"));
        assert!(names.contains(&"VK_KHR_get_physical_device_properties2"));
        assert!(INSTANCE_EXTENSIONS.iter().all(|e| e.spec_version >= 1));
    }

    /// The census: the EXACT instance-extension set, so adding one is a deliberate act with a body
    /// behind it rather than a drift. A name here is a promise the driver keeps.
    #[test]
    fn instance_extension_census_is_exact() {
        let names: Vec<&str> = INSTANCE_EXTENSIONS.iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            vec![
                "VK_KHR_surface",
                "VK_KHR_wayland_surface",
                "VK_KHR_get_physical_device_properties2",
                "VK_KHR_external_memory_capabilities",
                "VK_KHR_external_semaphore_capabilities",
                "VK_KHR_external_fence_capabilities",
            ],
            "the advertised instance-extension set drifted"
        );
        // No duplicates: the loader dedupes, but a repeated name means the list was edited blind.
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "a name is listed twice");
    }

    /// The extensions this driver must NOT advertise, each because the entry points it would promise
    /// are refused stubs or do not exist. Measured against the installed bundle before this list was
    /// written; every one of these is a lie the ladder forbids, not an oversight to fix later.
    #[test]
    fn extensions_without_a_body_are_not_advertised() {
        let names: Vec<&str> = INSTANCE_EXTENSIONS.iter().map(|e| e.name).collect();
        for forbidden in [
            // Entry points are in the shim's refused list.
            "VK_KHR_get_surface_capabilities2",
            // No X11 path exists; only wayland surfaces are created.
            "VK_KHR_xcb_surface",
            "VK_KHR_xlib_surface",
            // No display/KMS path exists.
            "VK_KHR_display",
        ] {
            assert!(
                !names.contains(&forbidden),
                "{forbidden} has no body in this driver and must not be advertised"
            );
        }
    }

    #[test]
    fn device_extensions_advertise_swapchain() {
        let names: Vec<&str> = DEVICE_EXTENSIONS.iter().map(|e| e.name).collect();
        assert!(names.contains(&"VK_KHR_swapchain"));
    }

    #[test]
    fn device_extensions_advertise_dynamic_rendering() {
        // VK_KHR_dynamic_rendering is really backed (cmd_begin_rendering lowers to Enc::BeginRenderPass),
        // so it is honestly advertised — a modern app / wgpu-on-Vulkan gates its no-render-pass path on it.
        let names: Vec<&str> = DEVICE_EXTENSIONS.iter().map(|e| e.name).collect();
        assert!(names.contains(&"VK_KHR_dynamic_rendering"));
        assert!(DEVICE_EXTENSIONS.iter().all(|e| e.spec_version >= 1));
    }

    #[test]
    fn rgba8_reports_color_attachment_and_sampled() {
        let ff = Format(vk_format::R8G8B8A8_UNORM).features();
        assert_ne!(ff.optimal_tiling & format_feature::COLOR_ATTACHMENT, 0);
        assert_ne!(ff.optimal_tiling & format_feature::SAMPLED_IMAGE, 0);
        // A color format is never depth-stencil-capable.
        assert_eq!(
            ff.optimal_tiling & format_feature::DEPTH_STENCIL_ATTACHMENT,
            0
        );
        // LINEAR tiling advertises nothing: host-mapped linear image content is not materialized by this
        // backend, so a color format is sampleable only when OPTIMAL-tiled (uploaded via a device copy).
        assert_eq!(
            ff.linear_tiling, 0,
            "linear tiling must claim no materializable feature"
        );
    }

    #[test]
    fn depth_reports_depth_stencil_not_color() {
        let ff = Format(vk_format::D32_SFLOAT).features();
        assert_ne!(
            ff.optimal_tiling & format_feature::DEPTH_STENCIL_ATTACHMENT,
            0
        );
        assert_eq!(ff.optimal_tiling & format_feature::COLOR_ATTACHMENT, 0);
        // Depth is not linearly tileable.
        assert_eq!(ff.linear_tiling, 0);
    }

    #[test]
    fn vertex_float_reports_vertex_buffer() {
        let ff = Format(vk_format::R32G32B32A32_SFLOAT).features();
        assert_ne!(ff.buffer & format_feature::VERTEX_BUFFER, 0);
    }

    #[test]
    fn unknown_format_reports_nothing() {
        let ff = Format(0).features();
        assert_eq!(ff, FormatFeatures::default());
    }
}
