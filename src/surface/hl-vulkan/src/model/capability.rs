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

use super::memory::{vk_format, Format as MemoryFormat, VertexFormat};

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

/// Whether `vk_format` is one of the color formats the render/transfer path materializes (matches the
/// translated set in [`crate::model::memory::tex_format_from_vk`] /
/// [`crate::service::create::create_image`]).
impl Format {
    pub fn is_color(&self) -> bool {
        let vk_format = self.0;
        matches!(
            vk_format,
            vk_format::R8G8B8A8_UNORM
                | vk_format::R8G8B8A8_SRGB
                | vk_format::B8G8R8A8_UNORM
                | vk_format::B8G8R8A8_SRGB
        )
    }

    /// Whether `vk_format` is a depth/stencil format the render path materializes.
    pub fn is_depth(&self) -> bool {
        let vk_format = self.0;
        matches!(
            vk_format,
            vk_format::D32_SFLOAT | vk_format::D24_UNORM_S8_UINT
        )
    }

    /// Whether a `(format, 2D, optimal-tiling)` image is creatable — the subset
    /// `vkGetPhysicalDeviceImageFormatProperties` reports as supported (color formats only; anything else
    /// is truthfully `VK_ERROR_FORMAT_NOT_SUPPORTED`).
    pub fn is_image_supported(&self) -> bool {
        self.is_color()
            || Format(self.0)
                .wire()
                .is_some_and(|format| format.block_geometry().is_some())
    }

    /// The truthful per-format `VkFormatProperties` feature masks. A color format advertises
    /// color-attachment + blend + sampled + storage + blit + transfer; a depth/stencil format advertises
    /// depth-stencil-attachment + sampled + transfer (never color-attachment, and vice-versa); vertex float
    /// formats advertise `VERTEX_BUFFER`. Reporting the same flags for every format (the old all-broad stub)
    /// made a client build wrong per-format capabilities. Ported from
    /// `MVKPixelFormats::getVkFormatProperties`.
    pub fn features(&self) -> FormatFeatures {
        let vk_format = self.0;
        use format_feature as f;
        let color = self.is_color();
        let depth = self.is_depth();
        let compressed = Format(vk_format)
            .wire()
            .is_some_and(|format| format.block_geometry().is_some());
        let optimal = if color {
            f::SAMPLED_IMAGE
                | f::STORAGE_IMAGE
                | f::COLOR_ATTACHMENT
                | f::COLOR_ATTACHMENT_BLEND
                | f::BLIT_SRC
                | f::BLIT_DST
                | f::SAMPLED_IMAGE_FILTER_LINEAR
                | f::TRANSFER_SRC
                | f::TRANSFER_DST
        } else if compressed {
            f::SAMPLED_IMAGE | f::SAMPLED_IMAGE_FILTER_LINEAR | f::TRANSFER_SRC | f::TRANSFER_DST
        } else if depth {
            f::SAMPLED_IMAGE | f::DEPTH_STENCIL_ATTACHMENT | f::TRANSFER_SRC | f::TRANSFER_DST
        } else {
            0
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
        if color {
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
        assert!(lowerable >= 30, "expected the lowerable set to be the 30 from VertexFormat, got {lowerable}");
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
        assert!(features.buffer & format_feature::VERTEX_BUFFER != 0, "vertex buffer");
        assert!(
            features.buffer & format_feature::UNIFORM_TEXEL_BUFFER != 0,
            "a colour format is also a uniform texel buffer"
        );
    }

    /// KNOWN GAP, recorded so it is not rediscovered: `is_color` is a hand-written list of four formats
    /// while `Format::wire` lowers roughly thirty as textures, and `is_image_supported` is just
    /// `is_color`. So `vkGetPhysicalDeviceFormatProperties` reports NO optimal-tiling features, and
    /// `vkGetPhysicalDeviceImageFormatProperties` reports VK_ERROR_FORMAT_NOT_SUPPORTED, for formats this
    /// driver genuinely materializes — R32G32B32A32_SFLOAT, R16G16B16A16_SFLOAT, R32_SFLOAT, R8_UNORM,
    /// R8G8_UNORM and every BC block format among them.
    ///
    /// That under-advertising is what gates most of the Vulkan CTS: 94.9% of enumerated cases return
    /// NotSupported, and the largest single reason by an order of magnitude is a format query
    /// (69,606 "Source format not supported" in blitting alone). This test pins the CURRENT extent of
    /// the drift so closing it is a deliberate, measured change rather than a silent one.
    #[test]
    fn color_advertisement_drifts_from_the_texture_lowering() {
        let mut lowered_not_advertised = Vec::new();
        for format in 0..=200u32 {
            let lowers = MemoryFormat(format).wire().is_some();
            if lowers && !Format(format).is_color() && !Format(format).is_depth() {
                lowered_not_advertised.push(format);
            }
        }
        assert!(
            !lowered_not_advertised.is_empty(),
            "if this list is empty the drift is closed — delete this test and its comment"
        );
        assert!(
            lowered_not_advertised.len() >= 20,
            "the drift shrank to {} formats; update this test deliberately: {:?}",
            lowered_not_advertised.len(),
            lowered_not_advertised
        );
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
