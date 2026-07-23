use super::*;

/// `VK_SHADER_STAGE_VERTEX_BIT` / `..._FRAGMENT_BIT` (from vk.xml) — classify a pipeline stage.
pub const VK_SHADER_STAGE_VERTEX_BIT: u32 = 0x0000_0001;
pub const VK_SHADER_STAGE_FRAGMENT_BIT: u32 = 0x0000_0010;
/// `VK_ATTACHMENT_LOAD_OP_CLEAR` (a render pass's first color attachment clears when its loadOp is this).
pub const VK_ATTACHMENT_LOAD_OP_CLEAR: i32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkExtent2D {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkOffset2D {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkRect2D {
    pub offset: VkOffset2D,
    pub extent: VkExtent2D,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkViewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[repr(C)]
pub struct VkImageCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub image_type: i32,
    pub format: i32,
    pub extent: VkExtent3D,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub samples: i32,
    pub tiling: i32,
    pub usage: VkFlags,
    pub sharing_mode: i32,
    pub queue_family_index_count: u32,
    pub p_queue_family_indices: *const u32,
    pub initial_layout: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkComponentMapping {
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub a: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkImageSubresourceRange {
    pub aspect_mask: VkFlags,
    pub base_mip_level: u32,
    pub level_count: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

#[repr(C)]
pub struct VkImageViewCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub image: u64,
    pub view_type: i32,
    pub format: i32,
    pub components: VkComponentMapping,
    pub subresource_range: VkImageSubresourceRange,
}

#[repr(C)]
pub struct VkSamplerCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub mag_filter: i32,
    pub min_filter: i32,
    pub mipmap_mode: i32,
    pub address_mode_u: i32,
    pub address_mode_v: i32,
    pub address_mode_w: i32,
    pub mip_lod_bias: f32,
    pub anisotropy_enable: VkBool32,
    pub max_anisotropy: f32,
    pub compare_enable: VkBool32,
    pub compare_op: i32,
    pub min_lod: f32,
    pub max_lod: f32,
    pub border_color: i32,
    pub unnormalized_coordinates: VkBool32,
}

#[repr(C)]
pub struct VkVertexInputBindingDescription {
    pub binding: u32,
    pub stride: u32,
    pub input_rate: i32,
}

#[repr(C)]
pub struct VkVertexInputAttributeDescription {
    pub location: u32,
    pub binding: u32,
    pub format: i32,
    pub offset: u32,
}

#[repr(C)]
pub struct VkPipelineVertexInputStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub vertex_binding_description_count: u32,
    pub p_vertex_binding_descriptions: *const VkVertexInputBindingDescription,
    pub vertex_attribute_description_count: u32,
    pub p_vertex_attribute_descriptions: *const VkVertexInputAttributeDescription,
}

#[repr(C)]
pub struct VkGraphicsPipelineCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub stage_count: u32,
    pub p_stages: *const VkPipelineShaderStageCreateInfo,
    pub p_vertex_input_state: *const VkPipelineVertexInputStateCreateInfo,
    pub p_input_assembly_state: *const c_void,
    pub p_tessellation_state: *const c_void,
    pub p_viewport_state: *const c_void,
    pub p_rasterization_state: *const c_void,
    pub p_multisample_state: *const c_void,
    pub p_depth_stencil_state: *const c_void,
    pub p_color_blend_state: *const c_void,
    pub p_dynamic_state: *const c_void,
    pub layout: u64,
    pub render_pass: u64,
    pub subpass: u32,
    pub base_pipeline_handle: u64,
    pub base_pipeline_index: i32,
}

#[repr(C)]
pub struct VkPipelineInputAssemblyStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    /// `VkPrimitiveTopology` — 0 POINT_LIST, 1 LINE_LIST, 2 LINE_STRIP, 3 TRIANGLE_LIST, 4 TRIANGLE_STRIP.
    pub topology: i32,
    pub primitive_restart_enable: u32,
}

#[repr(C)]
pub struct VkAttachmentDescription {
    pub flags: VkFlags,
    pub format: i32,
    pub samples: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub stencil_load_op: i32,
    pub stencil_store_op: i32,
    pub initial_layout: i32,
    pub final_layout: i32,
}

#[repr(C)]
pub struct VkRenderPassCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub attachment_count: u32,
    pub p_attachments: *const VkAttachmentDescription,
    pub subpass_count: u32,
    pub p_subpasses: *const c_void,
    pub dependency_count: u32,
    pub p_dependencies: *const c_void,
}

#[repr(C)]
pub struct VkFramebufferCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub render_pass: u64,
    pub attachment_count: u32,
    pub p_attachments: *const u64,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
}

/// `VkClearValue` is a 16-byte union; the color clear path reads it as `float32[4]`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkClearValue {
    pub float32: [f32; 4],
}

#[repr(C)]
pub struct VkRenderPassBeginInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub render_pass: u64,
    pub framebuffer: u64,
    pub render_area: VkRect2D,
    pub clear_value_count: u32,
    pub p_clear_values: *const VkClearValue,
}

// ---- dynamic rendering (VK_KHR_dynamic_rendering / core 1.3) --------------------------------------
// A dynamic-rendering pass carries its attachments inline (no VkRenderPass/VkFramebuffer object). Layout
// from vk.xml; the same sType values for the core and `KHR` aliases.

/// `VK_STRUCTURE_TYPE_RENDERING_INFO`.
pub const VK_STRUCTURE_TYPE_RENDERING_INFO: i32 = 1_000_044_000;
/// `VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO` (a graphics-pipeline pNext carrying color formats).
pub const VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO: i32 = 1_000_044_002;
/// `VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES` (the feature pNext in `...Features2`).
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DYNAMIC_RENDERING_FEATURES: i32 = 1_000_044_003;
/// `VK_ATTACHMENT_STORE_OP_STORE` (a dynamic-rendering attachment stores its result when its storeOp is this).
pub const VK_ATTACHMENT_STORE_OP_STORE: i32 = 0;

/// `VkRenderingAttachmentInfo` — one inline color/depth attachment of a dynamic-rendering pass.
#[repr(C)]
pub struct VkRenderingAttachmentInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub image_view: u64,
    pub image_layout: i32,
    pub resolve_mode: i32,
    pub resolve_image_view: u64,
    pub resolve_image_layout: i32,
    pub load_op: i32,
    pub store_op: i32,
    pub clear_value: VkClearValue,
}

/// `VkRenderingInfo` — the `vkCmdBeginRendering` argument (render area + inline color/depth attachments).
#[repr(C)]
pub struct VkRenderingInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub render_area: VkRect2D,
    pub layer_count: u32,
    pub view_mask: u32,
    pub color_attachment_count: u32,
    pub p_color_attachments: *const VkRenderingAttachmentInfo,
    pub p_depth_attachment: *const VkRenderingAttachmentInfo,
    pub p_stencil_attachment: *const VkRenderingAttachmentInfo,
}

/// `VkPipelineRenderingCreateInfo` — a graphics-pipeline pNext giving the color/depth formats a
/// dynamic-rendering pipeline (null `renderPass`) targets.
#[repr(C)]
pub struct VkPipelineRenderingCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub view_mask: u32,
    pub color_attachment_count: u32,
    pub p_color_attachment_formats: *const i32,
    pub depth_attachment_format: i32,
    pub stencil_attachment_format: i32,
}

/// `VkStencilOpState` — one face's stencil test + operation set. `failOp`/`passOp`/`depthFailOp` are
/// `VkStencilOp` values whose numbering (KEEP=0, ZERO=1, REPLACE=2, INCREMENT_AND_CLAMP=3,
/// DECREMENT_AND_CLAMP=4, INVERT=5, INCREMENT_AND_WRAP=6, DECREMENT_AND_WRAP=7) matches the neutral
/// `hl_gpu` `stencil_op::*` constants verbatim; `compareOp` is a `VkCompareOp` (NEVER=0 … ALWAYS=7) that
/// matches `compare::*` verbatim.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkStencilOpState {
    pub fail_op: i32,
    pub pass_op: i32,
    pub depth_fail_op: i32,
    pub compare_op: i32,
    pub compare_mask: u32,
    pub write_mask: u32,
    pub reference: u32,
}

/// `VkPipelineDepthStencilStateCreateInfo` — the depth/stencil fixed-function state of a graphics
/// pipeline. The full struct is declared: `depthTestEnable`/`depthWriteEnable`/`depthCompareOp` thread to
/// the neutral [`DepthState`] depth test, and `stencilTestEnable` + `front`/`back` (`VkStencilOpState`)
/// thread to its per-face stencil state (the executor honors both — wgpu `StencilState` + the CPU oracle's
/// `Depth24PlusStencil8` stencil plane). `depthBoundsTestEnable` + `min/maxDepthBounds` are NOT modeled (no
/// neutral field expresses depth bounds), but the struct is declared in full so `front`/`back` are read at
/// their correct offsets.
#[repr(C)]
pub struct VkPipelineDepthStencilStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub depth_test_enable: VkBool32,
    pub depth_write_enable: VkBool32,
    pub depth_compare_op: i32,
    pub depth_bounds_test_enable: VkBool32,
    pub stencil_test_enable: VkBool32,
    pub front: VkStencilOpState,
    pub back: VkStencilOpState,
    pub min_depth_bounds: f32,
    pub max_depth_bounds: f32,
}

/// `VkPipelineColorBlendAttachmentState` — the per-color-target fixed-function blend state. All fields are
/// read: `blendEnable` gates whether the target composites (vs. overwrites), and the src/dst factors + ops
/// (each a `VkBlendFactor` / `VkBlendOp`) are translated onto the neutral `hl_gpu` blend wire numbering by
/// `parse_color_blend_state`. `colorWriteMask` is the last field, so this is the full struct.
#[repr(C)]
pub struct VkPipelineColorBlendAttachmentState {
    pub blend_enable: VkBool32,
    pub src_color_blend_factor: i32,
    pub dst_color_blend_factor: i32,
    pub color_blend_op: i32,
    pub src_alpha_blend_factor: i32,
    pub dst_alpha_blend_factor: i32,
    pub alpha_blend_op: i32,
    pub color_write_mask: VkFlags,
}

/// `VkPipelineColorBlendStateCreateInfo` — the color-blend fixed-function state of a graphics pipeline.
/// The first attachment's blend AND `colorWriteMask` are threaded (the software rasterizer applies one
/// blend + one write mask to all targets), so the struct is truncated after `pAttachments`:
/// `logicOp`/`blendConstants` are NOT modeled and no field past this prefix is ever accessed. A null
/// pointer / `blendEnable = VK_FALSE` => no blend (an opaque overwrite); an absent state => `colorWriteMask`
/// defaults to `0xf` (write all channels).
#[repr(C)]
pub struct VkPipelineColorBlendStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub logic_op_enable: VkBool32,
    pub logic_op: i32,
    pub attachment_count: u32,
    pub p_attachments: *const VkPipelineColorBlendAttachmentState,
    // Remaining field (blendConstants[4]) is NOT modeled and is never read through this pointer.
}

/// `VkPipelineMultisampleStateCreateInfo` — the multisample fixed-function state of a graphics pipeline.
/// Only `rasterizationSamples` is read (a `VkSampleCountFlagBits` whose bit VALUE is the sample count:
/// `_1_BIT`=1, `_2_BIT`=2, `_4_BIT`=4, …), threaded to [`RenderPipelineDesc::sample_count`] so an MSAA
/// pipeline rasterizes multisampled. The struct is truncated after `rasterizationSamples`: the remaining
/// fields (sampleShadingEnable, minSampleShading, pSampleMask, alphaToCoverage/OneEnable) are NOT modeled and
/// are never read through this pointer. A null pointer / `_1_BIT` => single-sample.
#[repr(C)]
pub struct VkPipelineMultisampleStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub rasterization_samples: i32,
    // Remaining fields (sampleShadingEnable, minSampleShading, pSampleMask, alphaToCoverageEnable,
    // alphaToOneEnable) are NOT modeled and are never read through this pointer.
}

/// `VkPipelineRasterizationStateCreateInfo` — the rasterization fixed-function state of a graphics
/// pipeline. Only `cullMode` + `frontFace` are read (threaded to the neutral `RenderPipelineDesc::cull` /
/// `front_face`, honored by wgpu's `PrimitiveState` and the CPU oracle's face-cull). The struct is
/// truncated after `frontFace`: `polygonMode` (line/point fill), `depthClampEnable`,
/// `rasterizerDiscardEnable`, and the depthBias / lineWidth tail are NOT expressible in the neutral
/// pipeline and are never read through this pointer.
#[repr(C)]
pub struct VkPipelineRasterizationStateCreateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub flags: VkFlags,
    pub depth_clamp_enable: VkBool32,
    pub rasterizer_discard_enable: VkBool32,
    pub polygon_mode: i32,
    /// `VkCullModeFlags` — 0 NONE, 1 FRONT_BIT, 2 BACK_BIT, 3 FRONT_AND_BACK.
    pub cull_mode: VkFlags,
    /// `VkFrontFace` — 0 COUNTER_CLOCKWISE, 1 CLOCKWISE.
    pub front_face: i32,
    // Remaining fields (depthBiasEnable, depthBiasConstantFactor, depthBiasClamp, depthBiasSlopeFactor,
    // lineWidth) are NOT modeled and are never read through this pointer.
}

/// `VkPhysicalDeviceDynamicRenderingFeatures` — the feature pNext `vkGetPhysicalDeviceFeatures2` fills to
/// advertise `dynamicRendering = VK_TRUE` (really backed by the `cmd_begin_rendering` lowering).
#[repr(C)]
pub struct VkPhysicalDeviceDynamicRenderingFeatures {
    pub s_type: i32,
    pub p_next: *mut c_void,
    pub dynamic_rendering: VkBool32,
}

// ---- bind-memory-2 / memory-requirements-2 (core 1.1 / VK_KHR_bind_memory2 + get_memory_requirements2)
// Each aggregate wraps the v1 arguments behind a `{ sType, pNext }` header; the `...2` entry points read
// these and delegate to the identical v1 body. Layout from vk.xml.
