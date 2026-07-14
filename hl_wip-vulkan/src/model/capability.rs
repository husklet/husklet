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

use super::memory::vk_format;

/// One advertised extension: its `VK_KHR_*`/`VK_EXT_*` name + spec version (the two fields
/// `vkEnumerate{Instance,Device}ExtensionProperties` writes into each `VkExtensionProperties`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExtensionProp {
    pub name: &'static str,
    pub spec_version: u32,
}

/// Instance-level extensions the ICD really backs: the WSI base (`VK_KHR_surface`) + the `...2`
/// physical-device property queries (`VK_KHR_get_physical_device_properties2`). A real app (wgpu/ash/
/// vkcube) gates its init on these being enumerated and aborts otherwise, so advertising them is the
/// key unblock past instance setup.
pub const INSTANCE_EXTENSIONS: &[ExtensionProp] = &[
    ExtensionProp { name: "VK_KHR_surface", spec_version: 25 },
    ExtensionProp { name: "VK_KHR_get_physical_device_properties2", spec_version: 2 },
];

/// Device-level extensions the ICD really backs: `VK_KHR_swapchain` (the present path in
/// [`crate::service::present`]) + `VK_KHR_dynamic_rendering` (the render-pass-object-free rendering path
/// in [`crate::service::record::cmd_begin_rendering`], really lowered to `Enc::BeginRenderPass`). Nothing
/// else is advertised — a `vk.xml` extension without a real body here (timeline semaphores, buffer device
/// address, …) would be a dishonest claim.
pub const DEVICE_EXTENSIONS: &[ExtensionProp] = &[
    ExtensionProp { name: "VK_KHR_swapchain", spec_version: 70 },
    ExtensionProp { name: "VK_KHR_dynamic_rendering", spec_version: 1 },
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

/// Whether `vk_format` is one of the color formats the render/transfer path materializes (matches the
/// translated set in [`crate::model::memory::tex_format_from_vk`] /
/// [`crate::service::create::create_image`]).
pub fn is_color_format(vk_format: u32) -> bool {
    matches!(
        vk_format,
        vk_format::R8G8B8A8_UNORM
            | vk_format::R8G8B8A8_SRGB
            | vk_format::B8G8R8A8_UNORM
            | vk_format::B8G8R8A8_SRGB
    )
}

/// Whether `vk_format` is a depth/stencil format the render path materializes.
pub fn is_depth_format(vk_format: u32) -> bool {
    matches!(vk_format, vk_format::D32_SFLOAT | vk_format::D24_UNORM_S8_UINT)
}

/// Whether a `(format, 2D, optimal-tiling)` image is creatable — the subset
/// `vkGetPhysicalDeviceImageFormatProperties` reports as supported (color formats only; anything else
/// is truthfully `VK_ERROR_FORMAT_NOT_SUPPORTED`).
pub fn image_format_supported(vk_format: u32) -> bool {
    is_color_format(vk_format)
}

/// The truthful per-format `VkFormatProperties` feature masks. A color format advertises
/// color-attachment + blend + sampled + storage + blit + transfer; a depth/stencil format advertises
/// depth-stencil-attachment + sampled + transfer (never color-attachment, and vice-versa); vertex float
/// formats advertise `VERTEX_BUFFER`. Reporting the same flags for every format (the old all-broad stub)
/// made a client build wrong per-format capabilities. Ported from
/// `MVKPixelFormats::getVkFormatProperties`.
pub fn format_features(vk_format: u32) -> FormatFeatures {
    use format_feature as f;
    let color = is_color_format(vk_format);
    let depth = is_depth_format(vk_format);
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
    } else if depth {
        f::SAMPLED_IMAGE | f::DEPTH_STENCIL_ATTACHMENT | f::TRANSFER_SRC | f::TRANSFER_DST
    } else {
        0
    };
    // Vertex-attribute float formats (a client's vertex buffers); a color format also serves as a
    // uniform/storage texel buffer.
    let buffer = match vk_format {
        vk_format::R32_SFLOAT
        | vk_format::R16G16B16A16_SFLOAT
        | vk_format::R32G32B32A32_SFLOAT => f::VERTEX_BUFFER,
        _ if color => f::UNIFORM_TEXEL_BUFFER | f::STORAGE_TEXEL_BUFFER,
        _ => 0,
    };
    FormatFeatures {
        // Depth is never linear-tileable; a color format reports the same materializable set for both
        // tilings (the bring-up path treats tiling uniformly).
        linear_tiling: if depth { 0 } else { optimal },
        optimal_tiling: optimal,
        buffer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_extensions_advertise_surface_and_pdp2() {
        let names: Vec<&str> = INSTANCE_EXTENSIONS.iter().map(|e| e.name).collect();
        assert!(names.contains(&"VK_KHR_surface"));
        assert!(names.contains(&"VK_KHR_get_physical_device_properties2"));
        assert!(INSTANCE_EXTENSIONS.iter().all(|e| e.spec_version >= 1));
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
        let ff = format_features(vk_format::R8G8B8A8_UNORM);
        assert_ne!(ff.optimal_tiling & format_feature::COLOR_ATTACHMENT, 0);
        assert_ne!(ff.optimal_tiling & format_feature::SAMPLED_IMAGE, 0);
        // A color format is never depth-stencil-capable.
        assert_eq!(ff.optimal_tiling & format_feature::DEPTH_STENCIL_ATTACHMENT, 0);
    }

    #[test]
    fn depth_reports_depth_stencil_not_color() {
        let ff = format_features(vk_format::D32_SFLOAT);
        assert_ne!(ff.optimal_tiling & format_feature::DEPTH_STENCIL_ATTACHMENT, 0);
        assert_eq!(ff.optimal_tiling & format_feature::COLOR_ATTACHMENT, 0);
        // Depth is not linearly tileable.
        assert_eq!(ff.linear_tiling, 0);
    }

    #[test]
    fn vertex_float_reports_vertex_buffer() {
        let ff = format_features(vk_format::R32G32B32A32_SFLOAT);
        assert_ne!(ff.buffer & format_feature::VERTEX_BUFFER, 0);
    }

    #[test]
    fn unknown_format_reports_nothing() {
        let ff = format_features(0);
        assert_eq!(ff, FormatFeatures::default());
    }
}
