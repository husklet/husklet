//! Shader modules, compute/graphics pipelines, layouts, and pipeline caches.

use crate::adapter::spirv;
use crate::model::pipeline::{
    PipelineCacheRec, PipelineKind, PipelineLayoutRec, PipelineRec, ShaderRec,
};
use crate::*;
use hl_gpu::protocol::model::descriptor::{
    BlendState, ComputePipelineDesc, DepthState, PipelineBinding, PipelineBindingKind,
    PipelineLayout, RenderPipelineDesc, ShaderRef, VertexLayout,
};
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::{Cmd, CommandSink, GpuError, Result};

// ---- shader modules / pipelines ------------------------------------------------------------------

/// `vkCreateShaderModule` from a `pCode` byte image — validate the SPIR-V header, parse its
/// `OpEntryPoint` names, and submit [`Cmd::CreateShader`] with the words forwarded VERBATIM.
pub fn create_shader_module(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    code: &[u8],
) -> Result<VkShaderModule> {
    let module = spirv::Module::from_bytes(code)?;
    create_shader_module_words(dev, sink, module.into_words())
}

/// `vkCreateShaderModule` from SPIR-V words directly (the `pCode` already reinterpreted as `u32`s).
pub fn create_shader_module_words(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    words: Vec<u32>,
) -> Result<VkShaderModule> {
    let module = spirv::Module::from_words(words)?;
    let entries = module.entry_points();
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let words = module.words().to_vec();
    sink.submit(&[module.create_shader(ir_id)])?;
    hl_log::hl_debug!(
        hl_log::tag::VULKAN,
        "shader ir={} words={} entries={}",
        ir_id,
        words.len(),
        entries.len()
    );
    dev.shaders.insert(
        handle,
        ShaderRec {
            ir_id,
            spirv: words,
            entries,
        },
    );
    Ok(handle)
}

/// `vkCreateComputePipelines` (one pipeline) — resolve the compute stage's module + entry and submit
/// [`Cmd::CreateComputePipeline`]. Errors (`VK_ERROR_UNKNOWN` analogue) if the module or entry is
/// missing — no id-zero default. Ported from `pipeline.rs::vkCreateComputePipelines`.
pub fn create_compute_pipeline(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    shader: VkShaderModule,
    entry: &str,
) -> Result<VkPipeline> {
    create_compute_pipeline_with_layout(dev, sink, shader, entry, None)
}

pub fn create_compute_pipeline_with_layout(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    shader: VkShaderModule,
    entry: &str,
    layout: Option<VkPipelineLayout>,
) -> Result<VkPipeline> {
    let shader_ir = {
        let sh = dev.shaders.get(&shader).ok_or(GpuError::Invalid(
            "vkCreateComputePipelines: unknown VkShaderModule",
        ))?;
        if !sh.has_entry(entry) {
            return Err(GpuError::Invalid(
                "vkCreateComputePipelines: entry point not in module",
            ));
        }
        sh.ir_id
    };
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let desc = ComputePipelineDesc {
        compute: ShaderRef {
            module: shader_ir,
            entry: entry.to_string(),
        },
        label: format!("vkcpipe{ir_id}"),
    };
    let command = match layout {
        Some(layout) => {
            Cmd::CreateComputePipelineLayout(ir_id, desc, pipeline_bindings(dev, layout)?)
        }
        None => Cmd::CreateComputePipeline(ir_id, desc),
    };
    sink.submit(&[command])?;
    hl_log::hl_debug!(
        hl_log::tag::VULKAN,
        "pipeline kind=compute ir={} shader={} entry={}",
        ir_id,
        shader_ir,
        entry
    );
    dev.pipelines.insert(
        handle,
        PipelineRec {
            ir_id,
            kind: PipelineKind::Compute,
        },
    );
    Ok(handle)
}

/// `vkCreateGraphicsPipelines` (one pipeline) — resolve the vertex (+ optional fragment) stage(s), carry
/// the `VkPipelineVertexInputState` vertex-buffer layout(s), and submit [`Cmd::CreateRenderPipeline`] with
/// one color target per entry in `color_formats`. Ported from `pipeline.rs::vkCreateGraphicsPipelines`
/// (the bring-up subset: no blend/depth). `vertex_layouts` are the translated
/// `VkVertexInputBindingDescription`s (slot-0 layout is what the host rasterizer fetches positions from).
///
/// `color_formats` carries one format per color attachment — sourced from the bound `VkRenderPass`'s
/// attachments in the classic path, or from the pipeline's `VkPipelineRenderingCreateInfo::pColorAttachmentFormats`
/// pNext in the dynamic-rendering path (a null `renderPass`). An empty slice is valid (a depth-only /
/// no-color pipeline), yielding a pipeline with no color targets.
///
/// `depth` is the pipeline's depth-test state (format + write-enable + compare op) when
/// `VkPipelineDepthStencilStateCreateInfo::depthTestEnable` is set, else `None`. A `Some(..)` pipeline
/// MUST be drawn in a pass carrying a matching depth attachment (wgpu enforces this) — the shim threads
/// that attachment through the dynamic-rendering `vkCmdBeginRendering` depth target. Without this the
/// depth-stencil state was dropped (`depth: None` hardcoded) and every depth-tested draw ran with the
/// test disabled, so a far primitive could never be occluded by a nearer one.
///
/// `blend` is the pipeline's fixed-function color-blend state (src/dst factors + ops) when the color
/// attachment's `VkPipelineColorBlendAttachmentState::blendEnable` is set, else `None`. It is applied to
/// every color target. Without this the blend state was dropped (`blend: None` hardcoded) and a
/// translucent (alpha-over) draw OVERWROTE the destination instead of compositing over it.
///
/// `sample_count` is the pipeline's multisample count (`VkPipelineMultisampleStateCreateInfo::rasterizationSamples`
/// as a raw `VkSampleCountFlagBits` count value). It threads to [`RenderPipelineDesc::sample_count`] so an
/// MSAA pipeline rasterizes into a matching multisampled color attachment (the executor honors it, #179).
/// `0`/`1` collapse to single-sample so an existing single-sample pipeline is byte-identical.
///
/// `topology` is the pipeline's `VkPipelineInputAssemblyStateCreateInfo::topology` mapped onto the wire
/// [`Topology`] (the shim parses it; a null/unsupported topology folds to `TriangleList`). Without this the
/// topology was DROPPED (`Topology::TriangleList` hardcoded) and a pipeline drawing 4-vertex TRIANGLE_STRIP
/// quads — GPUI/wgpu's entire UI: every window/panel/glyph quad — rasterized only the FIRST triangle of each
/// quad, so each rectangle collapsed to a half-rectangle triangle (Zed rendered as scattered triangles).
///
/// `cull` (0 none / 1 front / 2 back) + `front_face` (0 CCW / 1 CW) are the rasterization cull state from
/// `VkPipelineRasterizationStateCreateInfo`; `color_write_mask` (RGBA, low 4 bits) is the first color
/// attachment's `colorWriteMask`, applied to every target. All three were previously hardcoded (`cull: 0`,
/// `front_face: 0`, `write_mask: 0xf`), silently dropping a guest's real cull/winding/channel-mask — a
/// back-face-culled mesh drew its interior and a masked channel was overwritten.
#[allow(clippy::too_many_arguments)]
pub fn create_graphics_pipeline(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vertex: (VkShaderModule, &str),
    fragment: Option<(VkShaderModule, &str)>,
    vertex_layouts: Vec<VertexLayout>,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
    blend: Option<BlendState>,
    sample_count: u32,
    topology: Topology,
    cull: u32,
    front_face: u32,
    color_write_mask: u32,
) -> Result<VkPipeline> {
    let color_targets = color_formats
        .into_iter()
        .map(
            |format| hl_gpu::protocol::model::descriptor::ColorTargetState {
                format,
                blend: blend.clone(),
                write_mask: color_write_mask & 0xf,
            },
        )
        .collect();
    create_graphics_pipeline_with_layout(
        dev,
        sink,
        vertex,
        fragment,
        vertex_layouts,
        color_targets,
        depth,
        sample_count,
        Default::default(),
        topology,
        cull,
        front_face,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn create_graphics_pipeline_with_layout(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vertex: (VkShaderModule, &str),
    fragment: Option<(VkShaderModule, &str)>,
    vertex_layouts: Vec<VertexLayout>,
    color_targets: Vec<hl_gpu::protocol::model::descriptor::ColorTargetState>,
    depth: Option<DepthState>,
    sample_count: u32,
    multisample: hl_gpu::protocol::model::descriptor::RenderMultisample,
    topology: Topology,
    cull: u32,
    front_face: u32,
    layout: Option<VkPipelineLayout>,
) -> Result<VkPipeline> {
    let resolve = |dev: &Device, (module, entry): (VkShaderModule, &str)| -> Result<ShaderRef> {
        let sh = dev.shaders.get(&module).ok_or(GpuError::Invalid(
            "vkCreateGraphicsPipelines: unknown VkShaderModule",
        ))?;
        if !sh.has_entry(entry) {
            return Err(GpuError::Invalid(
                "vkCreateGraphicsPipelines: entry point not in module",
            ));
        }
        Ok(ShaderRef {
            module: sh.ir_id,
            entry: entry.to_string(),
        })
    };
    let vertex_ref = resolve(dev, vertex)?;
    let fragment_ref = fragment.map(|f| resolve(dev, f)).transpose()?;
    let _color_targets_len = color_targets.len();
    let _has_fragment = fragment_ref.is_some();
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let desc = RenderPipelineDesc {
        vertex: vertex_ref,
        fragment: fragment_ref,
        vertex_buffers: vertex_layouts,
        color_targets,
        depth,
        topology,
        cull,
        front_face,
        sample_count: sample_count.max(1),
        label: format!("vkgpipe{ir_id}"),
    };
    let command = match layout {
        Some(layout) => Cmd::CreateRenderPipelineLayout(
            ir_id,
            desc,
            pipeline_bindings(dev, layout)?,
            multisample,
        ),
        None => Cmd::CreateRenderPipeline(ir_id, desc),
    };
    sink.submit(&[command])?;
    hl_log::hl_debug!(
        hl_log::tag::VULKAN,
        "pipeline kind=graphics ir={} frag={} targets={}",
        ir_id,
        _has_fragment,
        _color_targets_len
    );
    dev.pipelines.insert(
        handle,
        PipelineRec {
            ir_id,
            kind: PipelineKind::Graphics,
        },
    );
    Ok(handle)
}

fn pipeline_bindings(dev: &Device, layout: VkPipelineLayout) -> Result<PipelineLayout> {
    let layout = dev
        .pipeline_layouts
        .get(&layout)
        .ok_or(GpuError::Invalid("unknown VkPipelineLayout"))?;
    let mut bindings = Vec::new();
    for (group, set_layout) in layout.set_layouts.iter().enumerate() {
        let set_layout = dev.set_layouts.get(set_layout).ok_or(GpuError::Invalid(
            "VkPipelineLayout contains unknown descriptor set layout",
        ))?;
        for binding in set_layout
            .bindings
            .iter()
            .filter(|binding| binding.descriptor_count != 0)
        {
            let kind = match binding.descriptor_type {
                crate::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER
                | crate::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER_DYNAMIC => {
                    PipelineBindingKind::UniformBuffer
                }
                crate::model::descriptor::vk_descriptor_type::STORAGE_BUFFER
                | crate::model::descriptor::vk_descriptor_type::STORAGE_BUFFER_DYNAMIC => {
                    PipelineBindingKind::StorageBuffer
                }
                crate::model::descriptor::vk_descriptor_type::SAMPLED_IMAGE => {
                    PipelineBindingKind::SampledTexture
                }
                crate::model::descriptor::vk_descriptor_type::STORAGE_IMAGE => {
                    PipelineBindingKind::StorageTexture
                }
                crate::model::descriptor::vk_descriptor_type::SAMPLER => {
                    PipelineBindingKind::Sampler
                }
                crate::model::descriptor::vk_descriptor_type::COMBINED_IMAGE_SAMPLER => {
                    PipelineBindingKind::CombinedImageSampler
                }
                _ => {
                    return Err(GpuError::Unsupported(
                        "pipeline layout descriptor kind is unsupported",
                    ))
                }
            };
            bindings.push(PipelineBinding {
                group: group as u32,
                binding: binding.binding,
                count: binding.descriptor_count,
                kind,
            });
        }
    }
    Ok(PipelineLayout { bindings })
}

/// `vkCreatePipelineLayout` — record the composed set-layouts. No IR (bindings arrive with the sets).
impl Device {
    pub fn create_pipeline_layout(
        &mut self,
        set_layouts: Vec<VkDescriptorSetLayout>,
    ) -> VkPipelineLayout {
        let handle = self.alloc_handle();
        self.pipeline_layouts
            .insert(handle, PipelineLayoutRec { set_layouts });
        handle
    }
}

// ---- pipeline cache (modeled: a valid, versioned header; no host binary to cache) ----------------

/// The 32-byte `VkPipelineCacheHeaderVersionOne` prefix a valid pipeline-cache blob begins with:
/// `{ u32 length=32; u32 version=1; u32 vendorID; u32 deviceID; u8 uuid[16] }` (all little-endian, from
/// vk.xml). The hl-GPU pipelines forward SPIR-V verbatim (no compiled host artifact), so a cache carries
/// only this header — enough that a loader/app re-reading it via `vkGetPipelineCacheData` accepts it.
pub struct PipelineCache;
impl PipelineCache {
    pub fn header(dev: &Device) -> Vec<u8> {
        const HEADER_LEN: u32 = 32;
        const VK_PIPELINE_CACHE_HEADER_VERSION_ONE: u32 = 1;
        let pd = &dev.physical_device;
        let mut hdr = Vec::with_capacity(HEADER_LEN as usize);
        hdr.extend_from_slice(&HEADER_LEN.to_le_bytes());
        hdr.extend_from_slice(&VK_PIPELINE_CACHE_HEADER_VERSION_ONE.to_le_bytes());
        hdr.extend_from_slice(&pd.vendor_id.to_le_bytes());
        hdr.extend_from_slice(&pd.device_id.to_le_bytes());
        hdr.extend_from_slice(&pd.pipeline_cache_uuid);
        hdr
    }

    /// `vkCreatePipelineCache` — mint a cache holding a valid header (plus any app-provided `initial_data`,
    /// retained verbatim for round-trip). No IR.
    pub fn create(dev: &mut Device, initial_data: &[u8]) -> VkPipelineCache {
        // A well-formed `initialDataSize` blob is retained as-is; anything else falls back to a fresh header.
        let data = if initial_data.len() >= 32 {
            initial_data.to_vec()
        } else {
            Self::header(dev)
        };
        let handle = dev.alloc_handle();
        dev.pipeline_caches
            .insert(handle, PipelineCacheRec { data });
        handle
    }

    /// `vkDestroyPipelineCache` — drop the cache. No-op on `VK_NULL_HANDLE`/unknown.
    pub fn destroy(dev: &mut Device, cache: VkPipelineCache) {
        dev.pipeline_caches.remove(&cache);
    }

    /// `vkMergePipelineCaches` — merge `src` caches into `dst`. There is no compiled artifact to combine, so
    /// this is a truthful no-op that validates the handles. Errors on an unknown `dst`/`src` cache.
    pub fn merge(dev: &Device, dst: VkPipelineCache, srcs: &[VkPipelineCache]) -> Result<()> {
        if !dev.pipeline_caches.contains_key(&dst) {
            return Err(GpuError::Invalid(
                "vkMergePipelineCaches: unknown dst VkPipelineCache",
            ));
        }
        if !srcs.iter().all(|s| dev.pipeline_caches.contains_key(s)) {
            return Err(GpuError::Invalid(
                "vkMergePipelineCaches: unknown src VkPipelineCache",
            ));
        }
        Ok(())
    }

    /// `vkGetPipelineCacheData` — the serialized cache blob (a spec-valid header). Errors on an unknown cache.
    pub fn data(dev: &Device, cache: VkPipelineCache) -> Result<Vec<u8>> {
        Ok(dev
            .pipeline_caches
            .get(&cache)
            .ok_or(GpuError::Invalid(
                "vkGetPipelineCacheData: unknown VkPipelineCache",
            ))?
            .data
            .clone())
    }
}
