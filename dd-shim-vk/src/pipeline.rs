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

use crate::reg::{
    self, FramebufferRec, PipeKind, PipelineLayoutRec, PipelineRec, RenderPassRec, ShaderEntry,
    ShaderRec, ShaderType, SpecConstantRec,
};
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

#[derive(Default)]
struct Decorations {
    location: Option<u32>,
    binding: Option<u32>,
    set: Option<u32>,
    spec_id: Option<u32>,
    builtin: bool,
    /// `OpDecorate %struct Block` (2) — a uniform-buffer interface block.
    block: bool,
    /// `OpDecorate %struct BufferBlock` (3) — a (legacy) storage-buffer interface block.
    buffer_block: bool,
}

enum TypeNode {
    Bool,
    Int(u32, bool),
    Float(u32),
    Vector(u32, u32),
    Matrix(u32, u32),
    Pointer(u32),
    /// `OpTypeImage`: `(dim, sampled)` — Dim (2=2D, 5=Buffer, 6=SubpassData, …) and the Sampled operand
    /// (1 = sampled image, 2 = storage image). Drives the SAMPLED/STORAGE_IMAGE / *_TEXEL_BUFFER /
    /// INPUT_ATTACHMENT descriptor classification.
    Image(u32, u32),
    Sampler,
    /// `OpTypeSampledImage` (→ COMBINED_IMAGE_SAMPLER).
    SampledImage,
    Struct,
    /// `OpTypeArray` / `OpTypeRuntimeArray`: the element type id (a descriptor array).
    Array(u32),
    Other,
}

/// Infer the `VkDescriptorType` (raw) a resource `(type_id, storage class)` requires, walking descriptor
/// arrays to their element type. `None` when the type is not a classifiable descriptor (then no type
/// check is enforced). Mirrors `SPIRVReflection`/`MVKShaderStageResourceBinding` resource classification.
fn infer_descriptor_type(
    type_id: u32,
    storage: u32,
    nodes: &std::collections::HashMap<u32, TypeNode>,
    decorations: &std::collections::HashMap<u32, Decorations>,
    depth: u32,
) -> Option<i32> {
    if depth > 8 {
        return None;
    }
    match nodes.get(&type_id)? {
        // A descriptor array (e.g. `sampler2D tex[4]`) has the element's descriptor type.
        TypeNode::Array(elem) => infer_descriptor_type(*elem, storage, nodes, decorations, depth + 1),
        TypeNode::Sampler => Some(0), // VK_DESCRIPTOR_TYPE_SAMPLER
        TypeNode::SampledImage => Some(1), // COMBINED_IMAGE_SAMPLER
        TypeNode::Image(dim, sampled) => Some(match (*dim, *sampled) {
            (5, 2) => 5,  // Buffer + storage  → STORAGE_TEXEL_BUFFER
            (5, _) => 4,  // Buffer + sampled  → UNIFORM_TEXEL_BUFFER
            (6, _) => 10, // SubpassData       → INPUT_ATTACHMENT
            (_, 2) => 3,  // storage image     → STORAGE_IMAGE
            _ => 2,       // sampled image     → SAMPLED_IMAGE
        }),
        TypeNode::Struct => match storage {
            // Uniform(2): Block → UNIFORM_BUFFER, BufferBlock → STORAGE_BUFFER (legacy).
            2 => Some(if decorations.get(&type_id).is_some_and(|d| d.buffer_block) { 7 } else { 6 }),
            // StorageBuffer(12) storage class → STORAGE_BUFFER.
            12 => Some(7),
            _ => None,
        },
        _ => None,
    }
}

/// Whether a descriptor-set-layout binding of `layout_type` (raw `VkDescriptorType`) satisfies a shader
/// resource inferred as `shader_type`. Exact match, with the dynamic buffer types (8/9) accepted for a
/// plain uniform/storage buffer (a shader cannot distinguish dynamic from non-dynamic).
fn descriptor_type_compatible(layout_type: i32, shader_type: i32) -> bool {
    let norm = |t: i32| match t {
        8 => 6, // UNIFORM_BUFFER_DYNAMIC → UNIFORM_BUFFER
        9 => 7, // STORAGE_BUFFER_DYNAMIC → STORAGE_BUFFER
        other => other,
    };
    norm(layout_type) == norm(shader_type)
}

fn spirv_string(words: &[u32]) -> Option<String> {
    let mut bytes = Vec::new();
    for word in words {
        for byte in word.to_le_bytes() {
            if byte == 0 {
                return String::from_utf8(bytes).ok();
            }
            bytes.push(byte);
        }
    }
    None
}

fn resolve_type(id: u32, nodes: &std::collections::HashMap<u32, TypeNode>, depth: u32) -> ShaderType {
    if depth > 16 {
        return ShaderType::Other;
    }
    match nodes.get(&id) {
        Some(TypeNode::Bool) => ShaderType::Bool,
        Some(TypeNode::Int(width, signed)) => ShaderType::Int { width: *width, signed: *signed },
        Some(TypeNode::Float(width)) => ShaderType::Float { width: *width },
        Some(TypeNode::Vector(component, count)) => ShaderType::Vector {
            component: Box::new(resolve_type(*component, nodes, depth + 1)),
            count: *count,
        },
        Some(TypeNode::Matrix(column, count)) => ShaderType::Matrix {
            column: Box::new(resolve_type(*column, nodes, depth + 1)),
            count: *count,
        },
        Some(TypeNode::Pointer(pointee)) => resolve_type(*pointee, nodes, depth + 1),
        _ => ShaderType::Other,
    }
}

type ParsedSpirv = (
    std::collections::HashMap<String, ShaderEntry>,
    Vec<(u32, u32)>,
    std::collections::HashMap<(u32, u32), i32>,
    bool,
    std::collections::HashMap<u32, SpecConstantRec>,
);

fn parse_spirv(words: &[u32]) -> Option<ParsedSpirv> {
    if words.len() < 5
        || words[0] != 0x0723_0203
        || !(0x0001_0000..=0x0001_0600).contains(&words[1])
        || words[3] == 0
        || words[4] != 0
    {
        return None;
    }
    let bound = words[3];
    let mut nodes = std::collections::HashMap::new();
    let mut decorations: std::collections::HashMap<u32, Decorations> = std::collections::HashMap::new();
    let mut variables: std::collections::HashMap<u32, (u32, u32)> = std::collections::HashMap::new();
    let mut raw_entries = Vec::new();
    let mut spec_types = std::collections::HashMap::new();
    let mut cursor = 5usize;
    while cursor < words.len() {
        let first = words[cursor];
        let count = (first >> 16) as usize;
        let opcode = (first & 0xffff) as u16;
        if count == 0 || cursor.checked_add(count).is_none_or(|end| end > words.len()) {
            return None;
        }
        let inst = &words[cursor + 1..cursor + count];
        let valid_id = |id: u32| id != 0 && id < bound;
        match opcode {
            17 => {
                let capability = *inst.first()?;
                if !matches!(capability, 0 | 1) {
                    return None;
                }
            }
            14 => {
                if inst.len() != 2 || inst[0] != 0 || !matches!(inst[1], 1 | 3) {
                    return None;
                }
            }
            15 => {
                if inst.len() < 3 || !valid_id(inst[1]) || !matches!(inst[0], 0 | 4 | 5) {
                    return None;
                }
                let name = spirv_string(&inst[2..])?;
                let name_words = (name.len() + 1 + 3) / 4;
                if 2 + name_words > inst.len() {
                    return None;
                }
                raw_entries.push((inst[0], name, inst[2 + name_words..].to_vec()));
            }
            19 if inst.len() == 1 && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Other);
            }
            20 if inst.len() == 1 && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Bool);
            }
            21 if inst.len() == 3 && valid_id(inst[0]) && matches!(inst[1], 8 | 16 | 32 | 64) => {
                nodes.insert(inst[0], TypeNode::Int(inst[1], inst[2] != 0));
            }
            22 if inst.len() == 2 && valid_id(inst[0]) && matches!(inst[1], 16 | 32 | 64) => {
                nodes.insert(inst[0], TypeNode::Float(inst[1]));
            }
            23 if inst.len() == 3 && valid_id(inst[0]) && valid_id(inst[1]) && (2..=4).contains(&inst[2]) => {
                nodes.insert(inst[0], TypeNode::Vector(inst[1], inst[2]));
            }
            24 if inst.len() == 3 && valid_id(inst[0]) && valid_id(inst[1]) && (2..=4).contains(&inst[2]) => {
                nodes.insert(inst[0], TypeNode::Matrix(inst[1], inst[2]));
            }
            32 if inst.len() == 3 && valid_id(inst[0]) && valid_id(inst[2]) => {
                nodes.insert(inst[0], TypeNode::Pointer(inst[2]));
            }
            // OpTypeImage %r sampledType Dim Depth Arrayed MS Sampled Format [Access]
            25 if inst.len() >= 8 && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Image(inst[2], inst[6]));
            }
            26 if inst.len() == 1 && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Sampler);
            }
            // OpTypeSampledImage %r imageType
            27 if inst.len() == 2 && valid_id(inst[0]) && valid_id(inst[1]) => {
                nodes.insert(inst[0], TypeNode::SampledImage);
            }
            // OpTypeArray %r elementType lengthId
            28 if inst.len() == 3 && valid_id(inst[0]) && valid_id(inst[1]) => {
                nodes.insert(inst[0], TypeNode::Array(inst[1]));
            }
            // OpTypeRuntimeArray %r elementType
            29 if inst.len() == 2 && valid_id(inst[0]) && valid_id(inst[1]) => {
                nodes.insert(inst[0], TypeNode::Array(inst[1]));
            }
            // OpTypeStruct %r members...
            30 if !inst.is_empty() && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Struct);
            }
            31 if !inst.is_empty() && valid_id(inst[0]) => {
                nodes.insert(inst[0], TypeNode::Other);
            }
            59 if inst.len() >= 3 && valid_id(inst[0]) && valid_id(inst[1]) => {
                variables.insert(inst[1], (inst[0], inst[2]));
            }
            48..=50 if inst.len() >= 2 && valid_id(inst[0]) && valid_id(inst[1]) => {
                spec_types.insert(inst[1], inst[0]);
            }
            71 if inst.len() >= 2 && valid_id(inst[0]) => {
                let d = decorations.entry(inst[0]).or_default();
                match inst[1] {
                    1 if inst.len() == 3 => d.spec_id = Some(inst[2]),
                    2 => d.block = true,        // Block (uniform interface block)
                    3 => d.buffer_block = true, // BufferBlock (legacy storage interface block)
                    11 => d.builtin = true,
                    30 if inst.len() == 3 => d.location = Some(inst[2]),
                    33 if inst.len() == 3 => d.binding = Some(inst[2]),
                    34 if inst.len() == 3 => d.set = Some(inst[2]),
                    _ => {}
                }
            }
            _ => {}
        }
        cursor += count;
    }
    if raw_entries.is_empty() {
        return None;
    }
    let mut entries = std::collections::HashMap::new();
    for (model, name, interfaces) in raw_entries {
        let stage = match model {
            0 => vk::ShaderStageFlags::VERTEX.as_raw(),
            4 => vk::ShaderStageFlags::FRAGMENT.as_raw(),
            5 => vk::ShaderStageFlags::COMPUTE.as_raw(),
            _ => return None,
        };
        let mut inputs = std::collections::HashMap::new();
        let mut outputs = std::collections::HashMap::new();
        for id in interfaces {
            let Some((ty, storage)) = variables.get(&id) else {
                return None;
            };
            let deco = decorations.get(&id);
            if deco.is_some_and(|d| d.builtin) {
                continue;
            }
            let location = deco.and_then(|d| d.location)?;
            let ty = resolve_type(*ty, &nodes, 0);
            if ty == ShaderType::Other {
                return None;
            }
            let map = match *storage {
                1 => &mut inputs,
                3 => &mut outputs,
                _ => continue,
            };
            if map.insert(location, ty).is_some() {
                return None;
            }
        }
        if entries.insert(name, ShaderEntry { stage, inputs, outputs }).is_some() {
            return None;
        }
    }
    let mut descriptors = Vec::new();
    let mut descriptor_types: std::collections::HashMap<(u32, u32), i32> = std::collections::HashMap::new();
    let mut push_constant = false;
    for (id, (ptr_ty, storage)) in &variables {
        if *storage == 9 {
            push_constant = true;
        }
        if matches!(*storage, 0 | 2 | 12) {
            let deco = decorations.get(id)?;
            let key = (deco.set?, deco.binding?);
            descriptors.push(key);
            // Infer the descriptor type from the variable's pointee type (the pointer's pointee) and
            // its storage class. A binding we cannot classify simply carries no inferred type.
            if let Some(TypeNode::Pointer(pointee)) = nodes.get(ptr_ty) {
                if let Some(dt) = infer_descriptor_type(*pointee, *storage, &nodes, &decorations, 0) {
                    // Conflicting inferences for the same (set,binding) → reject the module.
                    if descriptor_types.insert(key, dt).is_some_and(|prev| prev != dt) {
                        return None;
                    }
                }
            }
        }
    }
    descriptors.sort_unstable();
    descriptors.dedup();
    let mut spec_constants = std::collections::HashMap::new();
    for (id, ty) in spec_types {
        let spec_id = decorations.get(&id)?.spec_id?;
        let ty = resolve_type(ty, &nodes, 0);
        if ty == ShaderType::Other || spec_constants.insert(spec_id, SpecConstantRec { ty }).is_some() {
            return None;
        }
    }
    Some((entries, descriptors, descriptor_types, push_constant, spec_constants))
}

fn specialization_size(ty: &ShaderType) -> Option<usize> {
    match ty {
        ShaderType::Bool => Some(4),
        ShaderType::Int { width, .. } | ShaderType::Float { width } => Some((*width / 8) as usize),
        _ => None,
    }
}

fn validate_stage<'a>(shader: &'a ShaderRec, stage: &vk::PipelineShaderStageCreateInfo) -> Option<(&'a ShaderEntry, String)> {
    if stage.p_name.is_null() {
        return None;
    }
    let name = unsafe { core::ffi::CStr::from_ptr(stage.p_name) }.to_str().ok()?.to_string();
    let entry = shader.entries.get(&name)?;
    if entry.stage != stage.stage.as_raw() || stage.stage.as_raw().count_ones() != 1 {
        return None;
    }
    if let Some(spec) = unsafe { stage.p_specialization_info.as_ref() } {
        if (spec.map_entry_count != 0 && spec.p_map_entries.is_null())
            || (spec.data_size != 0 && spec.p_data.is_null())
        {
            return None;
        }
        let entries = if spec.map_entry_count == 0 {
            &[][..]
        } else {
            unsafe { core::slice::from_raw_parts(spec.p_map_entries, spec.map_entry_count as usize) }
        };
        let mut ids = std::collections::HashSet::new();
        for map in entries {
            let constant = shader.spec_constants.get(&map.constant_id)?;
            if !ids.insert(map.constant_id)
                || specialization_size(&constant.ty)? != map.size
                || (map.offset as usize).checked_add(map.size).is_none_or(|end| end > spec.data_size)
            {
                return None;
            }
        }
    }
    Some((entry, name))
}

fn layout_supports_shader(state: &reg::VkState, layout: u64, shader: &ShaderRec, stage: u32) -> bool {
    let Some(layout) = state.pipeline_layouts.get(&layout) else {
        return false;
    };
    for &(set, binding) in &shader.descriptors {
        let Some(set_layout_handle) = layout.set_layouts.get(set as usize) else {
            return false;
        };
        let Some(set_layout) = state.descriptor_set_layouts.get(set_layout_handle) else {
            return false;
        };
        // The declared binding must exist, be non-empty, be visible to this stage, AND — when the SPIR-V
        // resource type was classifiable — declare a compatible descriptor type. A sampled image bound
        // where the layout declares a storage buffer (etc.) is a real interface mismatch, not a warning.
        let want_type = shader.descriptor_types.get(&(set, binding)).copied();
        if !set_layout.bindings.iter().any(|decl| {
            decl.binding == binding
                && decl.descriptor_count != 0
                && decl.stage_flags & stage != 0
                && want_type.is_none_or(|t| descriptor_type_compatible(decl.descriptor_type, t))
        }) {
            return false;
        }
    }
    !shader.push_constant
        || layout.push_ranges.iter().any(|(stages, _, size)| stages & stage != 0 && *size != 0)
}

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
    if ci.p_code.is_null() || ci.code_size < 20 || ci.code_size % 4 != 0 {
        return VK_ERROR_UNKNOWN;
    }
    let spirv = unsafe { core::slice::from_raw_parts(ci.p_code, ci.code_size / 4) }.to_vec();
    let Some((entries, descriptors, descriptor_types, push_constant, spec_constants)) = parse_spirv(&spirv)
    else {
        return VK_ERROR_UNKNOWN;
    };
    let mut s = reg::lock();
    let ir_id = s.alloc_ir();
    let handle = s.alloc_handle();
    s.record(Cmd::CreateShader {
        id: ir_id,
        kind: dd_gpu::ir::ShaderPayloadKind::SpirV,
        spirv: spirv.clone(),
    });
    s.shaders.insert(
        handle,
        ShaderRec { ir_id, spirv, entries, descriptors, descriptor_types, push_constant, spec_constants },
    );
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
    p_create_info: *const vk::PipelineLayoutCreateInfo,
    _p_allocator: *const c_void,
    p_pipeline_layout: *mut VkPipelineLayout,
) -> VkResult {
    let (Some(ci), Some(out)) = (unsafe { p_create_info.as_ref() }, unsafe { p_pipeline_layout.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if (ci.set_layout_count != 0 && ci.p_set_layouts.is_null())
        || (ci.push_constant_range_count != 0 && ci.p_push_constant_ranges.is_null())
    {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let set_layouts = if ci.set_layout_count == 0 {
        Vec::new()
    } else {
        unsafe { core::slice::from_raw_parts(ci.p_set_layouts, ci.set_layout_count as usize) }
            .iter()
            .map(|layout| layout.as_raw())
            .collect::<Vec<_>>()
    };
    let ranges = if ci.push_constant_range_count == 0 {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(ci.p_push_constant_ranges, ci.push_constant_range_count as usize) }
    };
    let mut s = reg::lock();
    if set_layouts.iter().any(|layout| !s.descriptor_set_layouts.contains_key(layout)) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let handle = s.alloc_handle();
    s.pipeline_layouts.insert(
        handle,
        PipelineLayoutRec {
            set_layouts,
            push_ranges: ranges.iter().map(|r| (r.stage_flags.as_raw(), r.offset, r.size)).collect(),
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyPipelineLayout(
    _device: VkDevice,
    pipeline_layout: VkPipelineLayout,
    _p_allocator: *const c_void,
) {
    reg::lock().pipeline_layouts.remove(&pipeline_layout);
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
    let mut result = VK_SUCCESS;
    for (i, info) in infos.iter().enumerate() {
        // The compute stage's module must resolve — a missing one fails the pipeline (no id-zero default).
        let Some((shader_ir, entry)) = s.shaders.get(&info.stage.module.as_raw()).and_then(|shader| {
            validate_stage(shader, &info.stage).and_then(|(_, name)| {
                (info.stage.stage == vk::ShaderStageFlags::COMPUTE
                    && layout_supports_shader(
                        &s,
                        info.layout.as_raw(),
                        shader,
                        vk::ShaderStageFlags::COMPUTE.as_raw(),
                    ))
                .then_some((shader.ir_id, name))
            })
        }) else {
            unsafe { *p_pipelines.add(i) = 0 }; // VK_NULL_HANDLE
            result = VK_ERROR_UNKNOWN;
            continue;
        };
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
    result
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
    crate::reg::trace("vkCreateGraphicsPipelines");
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe { core::slice::from_raw_parts(p_create_infos, create_info_count as usize) };
    let mut s = reg::lock();
    let mut result = VK_SUCCESS;
    for (i, info) in infos.iter().enumerate() {
        // Stages: resolve every stage's SPIR-V module by handle. A missing/invalid module must FAIL
        // the pipeline — never fall back to IR shader id zero, which the executor would then try to
        // run. Ported from MVKGraphicsPipeline stage handling (getOrCreateShaderModule).
        let stages = if info.p_stages.is_null() {
            &[][..]
        } else {
            unsafe { core::slice::from_raw_parts(info.p_stages, info.stage_count as usize) }
        };
        let mut vertex: Option<ShaderRef> = None;
        let mut fragment: Option<ShaderRef> = None;
        let mut vertex_entry: Option<ShaderEntry> = None;
        let mut fragment_entry: Option<ShaderEntry> = None;
        let mut bad_stage = false;
        for st in stages {
            let Some(sh) = s.shaders.get(&st.module.as_raw()) else {
                bad_stage = true;
                break;
            };
            let ir = sh.ir_id;
            let Some((reflected, entry)) = validate_stage(sh, st) else {
                bad_stage = true;
                break;
            };
            if !layout_supports_shader(&s, info.layout.as_raw(), sh, st.stage.as_raw()) {
                bad_stage = true;
                break;
            }
            if st.stage == vk::ShaderStageFlags::VERTEX && vertex.is_none() {
                vertex = Some(ShaderRef { module: ir, entry });
                vertex_entry = Some(reflected.clone());
            } else if st.stage == vk::ShaderStageFlags::FRAGMENT && fragment.is_none() {
                fragment = Some(ShaderRef { module: ir, entry });
                fragment_entry = Some(reflected.clone());
            } else {
                bad_stage = true;
                break;
            }
        }
        // A graphics pipeline requires a valid vertex stage and a known render pass + subpass; an
        // invalid combination is rejected (VK_ERROR_UNKNOWN) with a VK_NULL_HANDLE output, not defaulted.
        let subpass = info.subpass;
        let render_pass_known = s.render_passes.contains_key(&info.render_pass.as_raw());
        let interfaces_match = fragment_entry.as_ref().is_none_or(|fragment| {
            let Some(vertex) = vertex_entry.as_ref() else {
                return false;
            };
            fragment.inputs.iter().all(|(location, ty)| vertex.outputs.get(location) == Some(ty))
        });
        if bad_stage || vertex.is_none() || !interfaces_match || !render_pass_known || subpass != 0 {
            unsafe { *p_pipelines.add(i) = 0 }; // VK_NULL_HANDLE
            result = VK_ERROR_UNKNOWN;
            continue;
        }
        let vertex = vertex.expect("validated above");

        // Fixed-function state. The bring-up render pipeline lowers vertex input + topology + color
        // target; the remaining state structs are read here and either honoured elsewhere or explicitly
        // deferred (single-sample, opaque, dynamic viewport) — they are NOT silently defaulted away.
        let _p_rasterization_state = info.p_rasterization_state; // no-cull opaque bring-up; culling not yet lowered
        let _p_multisample_state = info.p_multisample_state; // single-sample only (MSAA is a later increment)
        let _p_depth_stencil_state = info.p_depth_stencil_state; // no depth target in the color-only path
        let _p_color_blend_state = info.p_color_blend_state; // opaque write-all; blend factors not yet lowered
        let _p_viewport_state = info.p_viewport_state; // viewport/scissor set dynamically at begin-render-pass
        let _p_dynamic_state = info.p_dynamic_state; // dynamic state honoured via vkCmdSetViewport/Scissor
        let _pipeline_layout = info.layout; // resource-binding layout (compatibility threaded via descriptor sets)

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
            let stride = bindings.first().map_or(0, |b| b.stride);
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

        // Color target format must match the render pass's attachment (Metal/wgpu validate this).
        let color_format = s
            .render_passes
            .get(&info.render_pass.as_raw())
            .map(|r| r.color_format)
            .unwrap_or(TextureFormat::Rgba8Unorm);

        let ir_id = s.alloc_ir();
        let handle = s.alloc_handle();
        s.record(Cmd::CreateRenderPipeline(
            ir_id,
            RenderPipelineDesc {
                vertex,
                fragment,
                vertex_buffers,
                color_targets: vec![ColorTargetState {
                    format: color_format,
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
    result
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
    crate::reg::trace("vkCreateRenderPass");
    let (Some(ci), Some(out)) =
        (unsafe { p_create_info.as_ref() }, unsafe { p_render_pass.as_mut() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // Fold attachment 0 (the color attachment) format + load/store into the record.
    let (fmt, load_clear, store, initial_layout, final_layout) =
        if ci.attachment_count > 0 && !ci.p_attachments.is_null() {
        let a0 = unsafe { &*ci.p_attachments };
        (
            crate::memory::tex_format(a0.format),
            a0.load_op == vk::AttachmentLoadOp::CLEAR,
            a0.store_op == vk::AttachmentStoreOp::STORE,
            a0.initial_layout.as_raw(),
            a0.final_layout.as_raw(),
        )
    } else {
        (
            TextureFormat::Rgba8Unorm,
            true,
            true,
            vk::ImageLayout::UNDEFINED.as_raw(),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw(),
        )
    };
    let subpass_layout = if ci.subpass_count > 0 && !ci.p_subpasses.is_null() {
        let subpass = unsafe { &*ci.p_subpasses };
        if subpass.color_attachment_count > 0 && !subpass.p_color_attachments.is_null() {
            unsafe { (*subpass.p_color_attachments).layout.as_raw() }
        } else {
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw()
        }
    } else {
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL.as_raw()
    };
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.render_passes.insert(
        handle,
        RenderPassRec {
            color_format: fmt,
            color_load_clear: load_clear,
            clear: [0.0; 4],
            color_store: store,
            initial_layout,
            subpass_layout,
            final_layout,
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
    // A framebuffer is created against a specific render pass; its attachment count/order/views must be
    // compatible (MVKFramebuffer validates against MVKRenderPass). Reject an unknown render pass or an
    // attachment that is not a known image view, rather than accepting arbitrary handles.
    if s.render_passes.get(&ci.render_pass.as_raw()).is_none() {
        return VK_ERROR_UNKNOWN;
    }
    let attachments = if ci.attachment_count > 0 && !ci.p_attachments.is_null() {
        unsafe { core::slice::from_raw_parts(ci.p_attachments, ci.attachment_count as usize) }
    } else {
        &[][..]
    };
    for v in attachments {
        if s.image_views.get(&v.as_raw()).is_none() {
            return VK_ERROR_UNKNOWN;
        }
    }
    let color_view = attachments.first().map(|v| v.as_raw());
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

// ---- render area granularity ---------------------------------------------------------------------

/// `vkGetRenderAreaGranularity` — the optimal render-area alignment for a render pass. Our render
/// targets impose no tile alignment (a texel-granular color target), so the granularity is `(1, 1)`,
/// which is always spec-valid (any render area is a multiple of 1). Ported from
/// `MVKRenderPass::getRenderAreaGranularity` (MoltenVK reports `{1,1}` for the non-tiled path).
#[no_mangle]
pub extern "C" fn vkGetRenderAreaGranularity(
    _device: VkDevice,
    _render_pass: VkRenderPass,
    p_granularity: *mut vk::Extent2D,
) {
    if let Some(out) = unsafe { p_granularity.as_mut() } {
        *out = vk::Extent2D { width: 1, height: 1 };
    }
}

// ---- pipeline cache ------------------------------------------------------------------------------

/// The 32-byte `VkPipelineCacheHeaderVersionOne` header (spec §10.3). A valid, round-trippable header
/// so `vkGetPipelineCacheData` returns something a loader/app accepts and can re-feed to
/// `vkCreatePipelineCache`. Ported from `MVKPipelineCache::writeData` header layout.
fn pipeline_cache_header() -> Vec<u8> {
    let mut h = Vec::with_capacity(32);
    h.extend_from_slice(&32u32.to_le_bytes()); // headerSize
    h.extend_from_slice(&1u32.to_le_bytes()); // headerVersion = ONE
    h.extend_from_slice(&crate::state::APPLE_VENDOR_ID.to_le_bytes()); // vendorID
    h.extend_from_slice(&0xdd_00_0001u32.to_le_bytes()); // deviceID (matches physical_device_properties)
    h.extend_from_slice(b"ddMetalVulkan\0\0\0"); // pipelineCacheUUID[16]
    h.truncate(32);
    while h.len() < 32 {
        h.push(0);
    }
    h
}

/// `vkCreatePipelineCache` — an opaque serializable cache. We seed it with the spec header plus any
/// `pInitialData` the app supplies (round-tripped from a previous `vkGetPipelineCacheData`). Ported
/// from `MVKPipelineCache` (`MVKPipeline.mm`).
#[no_mangle]
pub extern "C" fn vkCreatePipelineCache(
    _device: VkDevice,
    p_create_info: *const vk::PipelineCacheCreateInfo,
    _p_allocator: *const c_void,
    p_pipeline_cache: *mut VkPipelineCache,
) -> VkResult {
    let Some(out) = (unsafe { p_pipeline_cache.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let mut data = pipeline_cache_header();
    if let Some(ci) = unsafe { p_create_info.as_ref() } {
        if ci.initial_data_size > 0 && !ci.p_initial_data.is_null() {
            let init = unsafe {
                core::slice::from_raw_parts(ci.p_initial_data as *const u8, ci.initial_data_size)
            };
            // Retain the app's serialized payload beyond our header (opaque to us; observable on read-back).
            if init.len() > 32 {
                data.extend_from_slice(&init[32..]);
            }
        }
    }
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.pipeline_caches.insert(handle, reg::PipelineCacheRec { data });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyPipelineCache(
    _device: VkDevice,
    pipeline_cache: VkPipelineCache,
    _p_allocator: *const c_void,
) {
    reg::lock().pipeline_caches.remove(&pipeline_cache);
}

/// `vkGetPipelineCacheData` — the two-call `(pDataSize, pData)` idiom (spec §10.3): report the size, or
/// copy up to `*pDataSize` bytes and return `VK_INCOMPLETE` if the buffer was too small.
#[no_mangle]
pub extern "C" fn vkGetPipelineCacheData(
    _device: VkDevice,
    pipeline_cache: VkPipelineCache,
    p_data_size: *mut usize,
    p_data: *mut c_void,
) -> VkResult {
    let Some(size_out) = (unsafe { p_data_size.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let s = reg::lock();
    let Some(cache) = s.pipeline_caches.get(&pipeline_cache) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_data.is_null() {
        *size_out = cache.data.len();
        return VK_SUCCESS;
    }
    let n = (*size_out).min(cache.data.len());
    unsafe { core::ptr::copy_nonoverlapping(cache.data.as_ptr(), p_data as *mut u8, n) };
    *size_out = n;
    if n < cache.data.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

/// `vkMergePipelineCaches` — merge the source caches into `dstCache`. We append each source's payload
/// (beyond the shared header) so the merge is observable through a subsequent `vkGetPipelineCacheData`.
/// Ported from `MVKPipelineCache::mergePipelineCaches`.
#[no_mangle]
pub extern "C" fn vkMergePipelineCaches(
    _device: VkDevice,
    dst_cache: VkPipelineCache,
    src_cache_count: u32,
    p_src_caches: *const VkPipelineCache,
) -> VkResult {
    if p_src_caches.is_null() {
        return VK_SUCCESS;
    }
    let srcs = unsafe { core::slice::from_raw_parts(p_src_caches, src_cache_count as usize) };
    let mut s = reg::lock();
    if !s.pipeline_caches.contains_key(&dst_cache) {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let mut appended: Vec<u8> = Vec::new();
    for &src in srcs {
        if src == dst_cache {
            continue;
        }
        if let Some(c) = s.pipeline_caches.get(&src) {
            if c.data.len() > 32 {
                appended.extend_from_slice(&c.data[32..]);
            }
        }
    }
    if let Some(dst) = s.pipeline_caches.get_mut(&dst_cache) {
        dst.data.extend_from_slice(&appended);
    }
    VK_SUCCESS
}

#[cfg(test)]
mod shader_validation_tests {
    use super::*;
    use std::ffi::CString;

    fn inst(opcode: u16, operands: &[u32], out: &mut Vec<u32>) {
        out.push((((operands.len() + 1) as u32) << 16) | opcode as u32);
        out.extend_from_slice(operands);
    }

    fn string_words(value: &str) -> Vec<u32> {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        bytes
            .chunks_exact(4)
            .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
            .collect()
    }

    fn module(model: u32, name: &str, interface_storage: Option<(u32, bool)>, spec: bool) -> Vec<u32> {
        let mut words = vec![0x0723_0203, 0x0001_0000, 0, 32, 0];
        inst(17, &[1], &mut words); // OpCapability Shader
        inst(14, &[0, 1], &mut words); // Logical + GLSL450
        inst(22, &[1, 32], &mut words); // %1 = float32
        inst(21, &[2, 32, 1], &mut words); // %2 = int32
        let mut interfaces = Vec::new();
        if let Some((storage, integer)) = interface_storage {
            let pointee = if integer { 2 } else { 1 };
            inst(32, &[3, storage, pointee], &mut words); // pointer
            inst(59, &[3, 4, storage], &mut words); // variable
            inst(71, &[4, 30, 0], &mut words); // Location 0
            interfaces.push(4);
        }
        if spec {
            inst(50, &[2, 6, 1], &mut words); // i32 spec constant
            inst(71, &[6, 1, 7], &mut words); // SpecId 7
        }
        let mut entry = vec![model, 10];
        entry.extend(string_words(name));
        entry.extend(interfaces);
        inst(15, &entry, &mut words);
        words
    }

    fn module_with_resources() -> Vec<u32> {
        let mut words = module(5, "resources", None, false);
        let mut cursor = 5;
        while (words[cursor] & 0xffff) != 15 {
            cursor += (words[cursor] >> 16) as usize;
        }
        let mut extra = Vec::new();
        inst(32, &[8, 12, 1], &mut extra); // pointer StorageBuffer -> f32
        inst(59, &[8, 9, 12], &mut extra);
        inst(71, &[9, 34, 0], &mut extra); // DescriptorSet 0
        inst(71, &[9, 33, 2], &mut extra); // Binding 2
        inst(32, &[11, 9, 1], &mut extra); // pointer PushConstant -> f32
        inst(59, &[11, 12, 9], &mut extra);
        words.splice(cursor..cursor, extra);
        words
    }

    fn create_module(words: &[u32], out: &mut VkShaderModule) -> VkResult {
        let ci = vk::ShaderModuleCreateInfo::default().code(words);
        vkCreateShaderModule(core::ptr::null_mut(), &ci, core::ptr::null(), out)
    }

    fn render_pass() -> VkRenderPass {
        let ci = vk::RenderPassCreateInfo::default();
        let mut render_pass = 0;
        assert_eq!(
            vkCreateRenderPass(core::ptr::null_mut(), &ci, core::ptr::null(), &mut render_pass),
            VK_SUCCESS
        );
        render_pass
    }

    fn pipeline_layout() -> VkPipelineLayout {
        let ci = vk::PipelineLayoutCreateInfo::default();
        let mut layout = 0;
        assert_eq!(
            vkCreatePipelineLayout(core::ptr::null_mut(), &ci, core::ptr::null(), &mut layout),
            VK_SUCCESS
        );
        layout
    }

    #[test]
    fn spirv_modules_entries_specialization_and_interfaces_validate_before_ir_mutation() {
        let _guard = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let malformed = [0x0723_0203, 0x0001_0000, 0, 8, 0, (4 << 16) | 17, 1];
        let mut output = 0xfeed_u64;
        assert_eq!(create_module(&malformed, &mut output), VK_ERROR_UNKNOWN);
        assert_eq!(output, 0xfeed, "module failure must preserve output");

        let mut unsupported = module(5, "kernel", None, false);
        unsupported.splice(5..7, [((2u32) << 16) | 17, 64]); // unsupported capability
        assert_eq!(create_module(&unsupported, &mut output), VK_ERROR_UNKNOWN);

        let compute_words = module(5, "kernel", None, true);
        let mut compute_module = 0;
        assert_eq!(create_module(&compute_words, &mut compute_module), VK_SUCCESS);
        let compute_layout = pipeline_layout();
        let wrong_name = CString::new("main").unwrap();
        let bad_stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(vk::ShaderModule::from_raw(compute_module))
            .name(&wrong_name);
        let bad_ci = vk::ComputePipelineCreateInfo::default()
            .stage(bad_stage)
            .layout(vk::PipelineLayout::from_raw(compute_layout));
        let mut pipeline = 0xface_u64;
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &bad_ci, core::ptr::null(), &mut pipeline),
            VK_ERROR_UNKNOWN
        );
        assert_eq!(pipeline, 0);

        let kernel = CString::new("kernel").unwrap();
        let bad_map = vk::SpecializationMapEntry { constant_id: 7, offset: 2, size: 4 };
        let data = [9u8; 4];
        let bad_spec = vk::SpecializationInfo::default()
            .map_entries(core::slice::from_ref(&bad_map))
            .data(&data);
        let bad_stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(vk::ShaderModule::from_raw(compute_module))
            .name(&kernel)
            .specialization_info(&bad_spec);
        let bad_ci = vk::ComputePipelineCreateInfo::default()
            .stage(bad_stage)
            .layout(vk::PipelineLayout::from_raw(compute_layout));
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &bad_ci, core::ptr::null(), &mut pipeline),
            VK_ERROR_UNKNOWN
        );

        let good_map = vk::SpecializationMapEntry { constant_id: 7, offset: 0, size: 4 };
        let good_spec = vk::SpecializationInfo::default()
            .map_entries(core::slice::from_ref(&good_map))
            .data(&data);
        let good_stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(vk::ShaderModule::from_raw(compute_module))
            .name(&kernel)
            .specialization_info(&good_spec);
        let good_ci = vk::ComputePipelineCreateInfo::default()
            .stage(good_stage)
            .layout(vk::PipelineLayout::from_raw(compute_layout));
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &good_ci, core::ptr::null(), &mut pipeline),
            VK_SUCCESS
        );
        assert_ne!(pipeline, 0);

        let mut resource_module = 0;
        assert_eq!(create_module(&module_with_resources(), &mut resource_module), VK_SUCCESS);
        let (descriptors, push_constant) = {
            let state = reg::lock();
            let reflected = &state.shaders[&resource_module];
            (reflected.descriptors.clone(), reflected.push_constant)
        };
        assert_eq!(descriptors, [(0, 2)]);
        assert!(push_constant);
        let resources = CString::new("resources").unwrap();
        let resource_stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(vk::ShaderModule::from_raw(resource_module))
            .name(&resources);
        let resource_ci = vk::ComputePipelineCreateInfo::default()
            .stage(resource_stage)
            .layout(vk::PipelineLayout::from_raw(compute_layout));
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &resource_ci, core::ptr::null(), &mut pipeline),
            VK_ERROR_UNKNOWN,
            "reflected resources must not fit an empty pipeline layout"
        );
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(2)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE);
        let set_ci = vk::DescriptorSetLayoutCreateInfo::default().bindings(core::slice::from_ref(&binding));
        let mut set_layout = 0;
        assert_eq!(
            crate::descriptor::vkCreateDescriptorSetLayout(
                core::ptr::null_mut(),
                &set_ci,
                core::ptr::null(),
                &mut set_layout,
            ),
            VK_SUCCESS
        );
        let set_handle = vk::DescriptorSetLayout::from_raw(set_layout);
        let push = vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::COMPUTE,
            offset: 0,
            size: 4,
        };
        let layout_ci = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(core::slice::from_ref(&set_handle))
            .push_constant_ranges(core::slice::from_ref(&push));
        let mut resource_layout = 0;
        assert_eq!(
            vkCreatePipelineLayout(
                core::ptr::null_mut(),
                &layout_ci,
                core::ptr::null(),
                &mut resource_layout,
            ),
            VK_SUCCESS
        );
        let resource_ci = vk::ComputePipelineCreateInfo::default()
            .stage(resource_stage)
            .layout(vk::PipelineLayout::from_raw(resource_layout));
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &resource_ci, core::ptr::null(), &mut pipeline),
            VK_SUCCESS
        );

        let mut vertex = 0;
        let mut fragment_bad = 0;
        let mut fragment_good = 0;
        assert_eq!(create_module(&module(0, "vs", Some((3, false)), false), &mut vertex), VK_SUCCESS);
        assert_eq!(create_module(&module(4, "fs", Some((1, true)), false), &mut fragment_bad), VK_SUCCESS);
        assert_eq!(create_module(&module(4, "fs", Some((1, false)), false), &mut fragment_good), VK_SUCCESS);
        let vs = CString::new("vs").unwrap();
        let fs = CString::new("fs").unwrap();
        let make_stages = |fragment| {
            [
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::VERTEX)
                    .module(vk::ShaderModule::from_raw(vertex))
                    .name(&vs),
                vk::PipelineShaderStageCreateInfo::default()
                    .stage(vk::ShaderStageFlags::FRAGMENT)
                    .module(vk::ShaderModule::from_raw(fragment))
                    .name(&fs),
            ]
        };
        let render_pass = render_pass();
        let graphics_layout = pipeline_layout();
        let bad_stages = make_stages(fragment_bad);
        let bad_graphics = vk::GraphicsPipelineCreateInfo::default()
            .stages(&bad_stages)
            .render_pass(vk::RenderPass::from_raw(render_pass))
            .layout(vk::PipelineLayout::from_raw(graphics_layout));
        pipeline = 0xaaaa;
        assert_eq!(
            vkCreateGraphicsPipelines(
                core::ptr::null_mut(),
                0,
                1,
                &bad_graphics,
                core::ptr::null(),
                &mut pipeline,
            ),
            VK_ERROR_UNKNOWN
        );
        assert_eq!(pipeline, 0);

        let good_stages = make_stages(fragment_good);
        let good_graphics = vk::GraphicsPipelineCreateInfo::default()
            .stages(&good_stages)
            .render_pass(vk::RenderPass::from_raw(render_pass))
            .layout(vk::PipelineLayout::from_raw(graphics_layout));
        assert_eq!(
            vkCreateGraphicsPipelines(
                core::ptr::null_mut(),
                0,
                1,
                &good_graphics,
                core::ptr::null(),
                &mut pipeline,
            ),
            VK_SUCCESS
        );
        assert_ne!(pipeline, 0);
    }

    /// A `UniformConstant` variable typed `OpTypeImage(2D, Sampled=1)` at (set 0, binding 0) is a
    /// SAMPLED_IMAGE descriptor: a pipeline layout that declares a STORAGE_BUFFER there is a real
    /// interface mismatch and must be rejected; declaring SAMPLED_IMAGE is accepted.
    #[test]
    fn spirv_descriptor_type_must_match_the_set_layout() {
        let _guard = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        fn image_module() -> Vec<u32> {
            let mut w = vec![0x0723_0203u32, 0x0001_0000, 0, 32, 0];
            inst(17, &[1], &mut w); // OpCapability Shader
            inst(14, &[0, 1], &mut w); // OpMemoryModel Logical GLSL450
            inst(22, &[1, 32], &mut w); // %1 = OpTypeFloat 32 (image sampled type)
            inst(25, &[5, 1, 1, 0, 0, 0, 1, 0], &mut w); // %5 = OpTypeImage 2D Sampled=1
            inst(32, &[6, 0, 5], &mut w); // %6 = OpTypePointer UniformConstant %5
            inst(59, &[6, 7, 0], &mut w); // %7 = OpVariable %6 UniformConstant
            inst(71, &[7, 34, 0], &mut w); // DescriptorSet 0
            inst(71, &[7, 33, 0], &mut w); // Binding 0
            let mut entry = vec![5u32, 10]; // GLCompute, %main = 10
            entry.extend(string_words("img"));
            inst(15, &entry, &mut w);
            w
        }

        fn layout_with(binding0: vk::DescriptorType) -> VkPipelineLayout {
            let binding = vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(binding0)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE);
            let set_ci =
                vk::DescriptorSetLayoutCreateInfo::default().bindings(core::slice::from_ref(&binding));
            let mut set_layout = 0;
            assert_eq!(
                crate::descriptor::vkCreateDescriptorSetLayout(core::ptr::null_mut(), &set_ci, core::ptr::null(), &mut set_layout),
                VK_SUCCESS
            );
            let set_handle = vk::DescriptorSetLayout::from_raw(set_layout);
            let layout_ci =
                vk::PipelineLayoutCreateInfo::default().set_layouts(core::slice::from_ref(&set_handle));
            let mut layout = 0;
            assert_eq!(
                vkCreatePipelineLayout(core::ptr::null_mut(), &layout_ci, core::ptr::null(), &mut layout),
                VK_SUCCESS
            );
            layout
        }

        // The module must reflect a SAMPLED_IMAGE at (0,0).
        let mut module_handle = 0;
        assert_eq!(create_module(&image_module(), &mut module_handle), VK_SUCCESS);
        {
            let state = reg::lock();
            assert_eq!(state.shaders[&module_handle].descriptor_types.get(&(0, 0)).copied(), Some(2));
        }
        let name = CString::new("img").unwrap();
        let stage = |layout| {
            vk::ComputePipelineCreateInfo::default()
                .stage(
                    vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::COMPUTE)
                        .module(vk::ShaderModule::from_raw(module_handle))
                        .name(&name),
                )
                .layout(vk::PipelineLayout::from_raw(layout))
        };

        // Wrong descriptor type at the binding → interface mismatch → rejected.
        let wrong = layout_with(vk::DescriptorType::STORAGE_BUFFER);
        let wrong_ci = stage(wrong);
        let mut pipeline = 0xbeef_u64;
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &wrong_ci, core::ptr::null(), &mut pipeline),
            VK_ERROR_UNKNOWN,
            "a SAMPLED_IMAGE shader resource must not bind to a STORAGE_BUFFER layout slot"
        );
        assert_eq!(pipeline, 0);

        // Matching descriptor type → accepted.
        let right = layout_with(vk::DescriptorType::SAMPLED_IMAGE);
        let right_ci = stage(right);
        assert_eq!(
            vkCreateComputePipelines(core::ptr::null_mut(), 0, 1, &right_ci, core::ptr::null(), &mut pipeline),
            VK_SUCCESS
        );
        assert_ne!(pipeline, 0);
    }
}
