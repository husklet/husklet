//! Shader-module + pipeline + render-pass + framebuffer entry points (real bodies), producing IR.
//!
//! Ported from MoltenVK:
//!   * `MVKShaderModule.mm` — a `VkShaderModule` IS SPIR-V (`pCode`/`codeSize`). The dd-gpu IR shader
//!     ABI is ALSO SPIR-V (`Cmd::CreateShader{ spirv }`, lowered host-side to MSL by naga), so the
//!     module forwards with **zero translation** — the keystone of the Vulkan seam. Entry point is
//!     selected by (name, stage) exactly as `SPIRVToMSLConverter.cpp` `set_entry_point` does.
//!   * `MVKPipeline.mm` — `MVKComputePipeline` wraps one MTLComputePipelineState from the compute
//!     stage's function; `MVKGraphicsPipeline::newMTLRenderPipelineDescriptor` (l.1170) builds the
//!     MTLRenderPipelineDescriptor from the vertex-input state (`addVertexInputToPipeline`, l.1193),
//!     vertex+fragment functions, color attachments and `inputPrimitiveTopology`. We build the IR
//!     `ComputePipelineDesc` / `RenderPipelineDesc` with the same pieces.
//!   * `MVKRenderPass.mm` — attachment loadAction Clear/Load (l.808), storeAction Store/DontCare
//!     (l.891). We fold the single color attachment's load/store into the render-pass record, applied
//!     at `vkCmdBeginRenderPass`.

use crate::reg::{self, FramebufferRec, PipeKind, PipelineRec, RenderPassRec, ShaderRec};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_shim_common::ir::*;

/// VkFormat → dd-gpu IR vertex-format code (float-component count in the low byte; see
/// dd-gpu-wgpu `vertex_format`). Covers the `R32*_SFLOAT` attribute formats.
fn vertex_format_code(f: vk::Format) -> u32 {
    match f {
        vk::Format::R32_SFLOAT => 1,
        vk::Format::R32G32_SFLOAT => 2,
        vk::Format::R32G32B32_SFLOAT => 3,
        vk::Format::R32G32B32A32_SFLOAT => 4,
        _ => 4,
    }
}

fn topology(t: vk::PrimitiveTopology) -> Topology {
    match t {
        vk::PrimitiveTopology::POINT_LIST => Topology::PointList,
        vk::PrimitiveTopology::LINE_LIST => Topology::LineList,
        vk::PrimitiveTopology::LINE_STRIP => Topology::LineStrip,
        vk::PrimitiveTopology::TRIANGLE_STRIP => Topology::TriangleStrip,
        _ => Topology::TriangleList,
    }
}

// ---- shader modules ------------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateShaderModule(
    _device: VkDevice,
    p_create_info: *const vk::ShaderModuleCreateInfo,
    _p_allocator: *const c_void,
    p_shader_module: *mut VkShaderModule,
) -> VkResult {
    let (Some(ci), Some(out)) =
        (unsafe { p_create_info.as_ref() }, unsafe { p_shader_module.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // codeSize is in BYTES; pCode is u32 words. Forward the SPIR-V verbatim (zero translation).
    let words = (ci.code_size / 4).max(0);
    let spirv: Vec<u32> = if ci.p_code.is_null() {
        Vec::new()
    } else {
        unsafe { core::slice::from_raw_parts(ci.p_code, words) }.to_vec()
    };
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    s.record(Cmd::CreateShader {
        id: ir_id,
        spirv: spirv.clone(),
    });
    s.shaders.insert(handle, ShaderRec { ir_id, spirv });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyShaderModule(
    _device: VkDevice,
    shader_module: VkShaderModule,
    _p_allocator: *const c_void,
) {
    let mut s = reg::lock();
    if let Some(sh) = s.shaders.remove(&shader_module) {
        s.record(Cmd::DestroyShader(sh.ir_id));
    }
}

// ---- pipeline layout (opaque; no IR needed — bindings come from descriptor sets) -----------------

#[no_mangle]
pub extern "C" fn vkCreatePipelineLayout(
    _device: VkDevice,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_pipeline_layout: *mut VkPipelineLayout,
) -> VkResult {
    if let Some(out) = unsafe { p_pipeline_layout.as_mut() } {
        *out = reg::lock().alloc_handle();
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyPipelineLayout(
    _device: VkDevice,
    _pipeline_layout: VkPipelineLayout,
    _p_allocator: *const c_void,
) {
}

// ---- compute pipelines ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateComputePipelines(
    _device: VkDevice,
    _pipeline_cache: u64,
    create_info_count: u32,
    p_create_infos: *const vk::ComputePipelineCreateInfo,
    _p_allocator: *const c_void,
    p_pipelines: *mut VkPipeline,
) -> VkResult {
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe { core::slice::from_raw_parts(p_create_infos, create_info_count as usize) };
    let mut s = reg::lock();
    for (i, info) in infos.iter().enumerate() {
        let shader_ir = s
            .shaders
            .get(&info.stage.module.as_raw())
            .map(|sh| sh.ir_id)
            .unwrap_or(0);
        let entry = cstr_or(info.stage.p_name, "main");
        let ir_id = s.alloc_ir();
        let handle = s.alloc_handle();
        s.record(Cmd::CreateComputePipeline(
            ir_id,
            ComputePipelineDesc {
                compute: ShaderRef {
                    module: shader_ir,
                    entry,
                },
                label: format!("vkcpipe{ir_id}"),
            },
        ));
        s.pipelines.insert(
            handle,
            PipelineRec {
                ir_id,
                kind: PipeKind::Compute,
            },
        );
        unsafe { *p_pipelines.add(i) = handle };
    }
    VK_SUCCESS
}

// ---- graphics pipelines --------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateGraphicsPipelines(
    _device: VkDevice,
    _pipeline_cache: u64,
    create_info_count: u32,
    p_create_infos: *const vk::GraphicsPipelineCreateInfo,
    _p_allocator: *const c_void,
    p_pipelines: *mut VkPipeline,
) -> VkResult {
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe { core::slice::from_raw_parts(p_create_infos, create_info_count as usize) };
    let mut s = reg::lock();
    for (i, info) in infos.iter().enumerate() {
        // Stages: find vertex + fragment SPIR-V modules by stage flag.
        let stages = if info.p_stages.is_null() {
            &[][..]
        } else {
            unsafe { core::slice::from_raw_parts(info.p_stages, info.stage_count as usize) }
        };
        let mut vertex = ShaderRef {
            module: 0,
            entry: "main".into(),
        };
        let mut fragment: Option<ShaderRef> = None;
        for st in stages {
            let ir = s.shaders.get(&st.module.as_raw()).map(|sh| sh.ir_id).unwrap_or(0);
            let entry = cstr_or(st.p_name, "main");
            if st.stage.contains(vk::ShaderStageFlags::VERTEX) {
                vertex = ShaderRef { module: ir, entry };
            } else if st.stage.contains(vk::ShaderStageFlags::FRAGMENT) {
                fragment = Some(ShaderRef { module: ir, entry });
            }
        }

        // Vertex input: binding stride + attributes (location/format/offset).
        let mut vertex_buffers = Vec::new();
        if let Some(vi) = unsafe { info.p_vertex_input_state.as_ref() } {
            let bindings = if vi.p_vertex_binding_descriptions.is_null() {
                &[][..]
            } else {
                unsafe {
                    core::slice::from_raw_parts(
                        vi.p_vertex_binding_descriptions,
                        vi.vertex_binding_description_count as usize,
                    )
                }
            };
            let attrs_in = if vi.p_vertex_attribute_descriptions.is_null() {
                &[][..]
            } else {
                unsafe {
                    core::slice::from_raw_parts(
                        vi.p_vertex_attribute_descriptions,
                        vi.vertex_attribute_description_count as usize,
                    )
                }
            };
            let stride = bindings.first().map(|b| b.stride).unwrap_or(0);
            let attrs = attrs_in
                .iter()
                .map(|a| VertexAttr {
                    location: a.location,
                    format: vertex_format_code(a.format),
                    offset: a.offset,
                })
                .collect();
            vertex_buffers.push(VertexLayout {
                stride,
                step_mode: 0,
                attrs,
            });
        }

        // Topology from input-assembly state.
        let topo = unsafe { info.p_input_assembly_state.as_ref() }
            .map(|ia| topology(ia.topology))
            .unwrap_or(Topology::TriangleList);

        let ir_id = s.alloc_ir();
        let handle = s.alloc_handle();
        s.record(Cmd::CreateRenderPipeline(
            ir_id,
            RenderPipelineDesc {
                vertex,
                fragment,
                vertex_buffers,
                color_targets: vec![ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: 0xf,
                }],
                depth: None,
                topology: topo,
                cull: 0,
                front_face: 0,
                label: format!("vkgpipe{ir_id}"),
            },
        ));
        s.pipelines.insert(
            handle,
            PipelineRec {
                ir_id,
                kind: PipeKind::Graphics,
            },
        );
        unsafe { *p_pipelines.add(i) = handle };
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyPipeline(_device: VkDevice, pipeline: VkPipeline, _p_allocator: *const c_void) {
    let mut s = reg::lock();
    if let Some(p) = s.pipelines.remove(&pipeline) {
        s.record(Cmd::DestroyPipeline(p.ir_id));
    }
}

// ---- render pass + framebuffer -------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateRenderPass(
    _device: VkDevice,
    p_create_info: *const vk::RenderPassCreateInfo,
    _p_allocator: *const c_void,
    p_render_pass: *mut VkRenderPass,
) -> VkResult {
    let (Some(ci), Some(out)) =
        (unsafe { p_create_info.as_ref() }, unsafe { p_render_pass.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // Fold attachment 0 (the color attachment) load/store into the record.
    let (load_clear, store) = if ci.attachment_count > 0 && !ci.p_attachments.is_null() {
        let a0 = unsafe { &*ci.p_attachments };
        (
            a0.load_op == vk::AttachmentLoadOp::CLEAR,
            a0.store_op == vk::AttachmentStoreOp::STORE,
        )
    } else {
        (true, true)
    };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.render_passes.insert(
        handle,
        RenderPassRec {
            color_load_clear: load_clear,
            clear: [0.0; 4],
            color_store: store,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyRenderPass(_device: VkDevice, render_pass: VkRenderPass, _p_allocator: *const c_void) {
    reg::lock().render_passes.remove(&render_pass);
}

#[no_mangle]
pub extern "C" fn vkCreateFramebuffer(
    _device: VkDevice,
    p_create_info: *const vk::FramebufferCreateInfo,
    _p_allocator: *const c_void,
    p_framebuffer: *mut VkFramebuffer,
) -> VkResult {
    let (Some(ci), Some(out)) =
        (unsafe { p_create_info.as_ref() }, unsafe { p_framebuffer.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut s = reg::lock();
    let color_view = if ci.attachment_count > 0 && !ci.p_attachments.is_null() {
        Some(unsafe { *ci.p_attachments }.as_raw())
    } else {
        None
    };
    let handle = s.alloc_handle();
    s.framebuffers.insert(
        handle,
        FramebufferRec {
            width: ci.width,
            height: ci.height,
            color_view,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyFramebuffer(_device: VkDevice, framebuffer: VkFramebuffer, _p_allocator: *const c_void) {
    reg::lock().framebuffers.remove(&framebuffer);
}

/// Resolve a `*const c_char` entry-point name to a `String`, defaulting when null/empty.
fn cstr_or(p: *const core::ffi::c_char, dflt: &str) -> String {
    if p.is_null() {
        return dflt.to_string();
    }
    unsafe { core::ffi::CStr::from_ptr(p) }
        .to_str()
        .map(|s| if s.is_empty() { dflt.to_string() } else { s.to_string() })
        .unwrap_or_else(|_| dflt.to_string())
}
