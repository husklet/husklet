//! SPIR-V pre-pass: rewrite Vulkan-GLSL **combined** image-samplers into the **separate** image + sampler
//! model naga's SPIR-V front-end accepts.
//!
//! # Why
//!
//! glslang compiles a GLSL `layout(binding=N) uniform sampler2D tex; … texture(tex, uv)` into a *combined*
//! sampler: an `OpTypeSampledImage` global variable in `UniformConstant` storage, an `OpLoad` of it, and an
//! `OpImageSample*` that consumes the loaded value directly — with **no** `OpSampledImage`. naga-24's
//! `spv-in` implements only Vulkan's *separate* model (a `texture_2d` global + a `sampler` global recombined
//! at each use by `OpSampledImage`), so it rejects the combined form (`InvalidId`). Every textured real
//! Vulkan app (vkcube, Zed, …) hits this, so `spirv_to_wgsl` runs this pass first.
//!
//! This is the inverse of SPIRV-Cross's `build_combined_image_samplers`. For each combined
//! `OpTypeSampledImage` variable we emit a separate `OpTypeImage` variable and an `OpTypeSampler` variable
//! (two distinct bindings), then rewrite every `OpLoad` of the combined variable into: load the image, load
//! the sampler, and `OpSampledImage(image, sampler)` producing the *same* result id the original `OpLoad`
//! produced — so every downstream `OpImageSample*`/`OpImage`/… consumes it unchanged. naga then reflects a
//! `texture_2d` + a `sampler` and accepts the module.
//!
//! # Binding coordination (mirrors the GL #114 split)
//!
//! wgpu forbids two bind-group entries at one binding, so the split image and sampler must land on DISTINCT
//! bindings, and the Vulkan driver that supplies the descriptors must agree. A combined descriptor at Vulkan
//! binding `B` becomes: **image at binding `B`**, **sampler at binding `B + `[`SAMPLER_BINDING_OFFSET`]**.
//! The driver (`hl-vulkan/src/service/record.rs`) applies the identical offset when it lowers a
//! `COMBINED_IMAGE_SAMPLER` to its `Texture` + `Sampler` `BindEntry`s. Buffers, separate images, and
//! separate samplers keep their own Vulkan bindings (this pass only touches combined samplers), so the two
//! sides stay in lock-step and only collide if a set places another descriptor at `B + offset` alongside a
//! combined sampler — not a layout real apps in this bring-up use.
//!
//! # Scope
//!
//! Hand-rolled on the raw SPIR-V word stream (no `rspirv` in the offline index; the neutral `spirv` enum
//! crate carries only constants). A shader with no combined `OpTypeSampledImage` variable is returned
//! byte-for-byte unchanged, so the existing SPIR-V conformance (separate model, minted from WGSL) is
//! untouched. A combined variable used anywhere other than a plain `OpLoad` (e.g. through an array
//! `OpAccessChain`) makes the pass DECLINE (return the input unchanged) rather than emit a broken module —
//! naga then produces its normal honest error, never a wrong-shader substitution.

use std::collections::{HashMap, HashSet};

use hl_gpu::Result;

/// The bind-group binding offset the sampler half of a split `COMBINED_IMAGE_SAMPLER` is placed at (the
/// image half keeps the descriptor's Vulkan binding `B`; the sampler goes to `B + SAMPLER_BINDING_OFFSET`).
/// The Vulkan driver's `vkCmdBindDescriptorSets` lowering MUST use the same constant so the descriptors it
/// binds match the layout naga reflects from the rewritten SPIR-V. Kept larger than any binding real
/// bring-up shaders pack into a single set so the two halves never collide with another descriptor.
pub const SAMPLER_BINDING_OFFSET: u32 = 16;

// ---- SPIR-V core opcodes (values are stable across SPIR-V 1.x) ----------------------------------------
const OP_NAME: u16 = 5;
const OP_ENTRY_POINT: u16 = 15;
const OP_TYPE_IMAGE: u16 = 25;
const OP_TYPE_SAMPLER: u16 = 26;
const OP_TYPE_SAMPLED_IMAGE: u16 = 27;
const OP_TYPE_POINTER: u16 = 32;
const OP_VARIABLE: u16 = 59;
const OP_LOAD: u16 = 61;
const OP_SAMPLED_IMAGE: u16 = 86;
const OP_DECORATE: u16 = 71;
const OP_MEMBER_DECORATE: u16 = 72;

const SC_UNIFORM_CONSTANT: u32 = 0;
const DEC_BINDING: u32 = 33;
const DEC_DESCRIPTOR_SET: u32 = 34;

const HEADER_WORDS: usize = 5;

/// A combined-sampler variable to split and the ids its replacement image/sampler use. All ids are `u32`.
#[derive(Clone, Copy)]
struct CombinedSampler {
    image_type: u32,
    set: u32,
    binding: u32,
    image_var: u32,
    sampler_var: u32,
    ptr_image: u32,
}

#[derive(Clone, Copy)]
struct Instruction(u32);

impl Instruction {
    #[inline]
    fn opcode(self) -> u16 {
        (self.0 & 0xffff) as u16
    }

    #[inline]
    fn len(self) -> usize {
        (self.0 >> 16) as usize
    }

    /// Opcodes 19..=39 are the `OpType*` range — the start of the module's types/constants/global-variables
    /// section (SPIR-V logical layout §2.4), which follows the annotations (`OpDecorate`) section.
    #[inline]
    fn is_type(op: u16) -> bool {
        (19..=39).contains(&op)
    }
}

pub struct CombinedSamplers;

/// Append one instruction `op` with `operands` (word count is derived).
fn emit(out: &mut Vec<u32>, op: u16, operands: &[u32]) {
    let word_count = (operands.len() + 1) as u32;
    out.push((word_count << 16) | op as u32);
    out.extend_from_slice(operands);
}

/// Rewrite combined image-samplers in `words` to the separate image+sampler model. Returns the input
/// unchanged when there is nothing to split (or when a case outside this pass's scope is detected).
impl CombinedSamplers {
    pub fn split(words: &[u32]) -> Result<Vec<u32>> {
        if words.len() <= HEADER_WORDS {
            return Ok(words.to_vec());
        }

        // ---- 1. Index every instruction boundary (op, start-word, word-count). --------------------------
        let mut insts: Vec<(u16, usize, usize)> = Vec::new();
        let mut i = HEADER_WORDS;
        while i < words.len() {
            let instruction = Instruction(words[i]);
            let len = instruction.len();
            if len == 0 || i + len > words.len() {
                return Ok(words.to_vec()); // malformed — let naga surface the honest error
            }
            insts.push((instruction.opcode(), i, len));
            i += len;
        }

        // ---- 2. Scan types / pointers / variables / decorations. ----------------------------------------
        let mut image_types: HashSet<u32> = HashSet::new();
        let mut sampler_types: HashSet<u32> = HashSet::new();
        let mut existing_sampler_type: Option<u32> = None;
        let mut sampled_to_image: HashMap<u32, u32> = HashMap::new(); // OpTypeSampledImage id -> image type
        let mut ptr_to_sampled: HashMap<u32, u32> = HashMap::new(); // UC OpTypePointer id -> sampled type
        let mut ptr_to_image: HashMap<u32, u32> = HashMap::new(); // image type -> UC pointer id (reuse)
        let mut ptr_to_sampler: HashMap<u32, u32> = HashMap::new(); // sampler type -> UC pointer id (reuse)
        let mut var_candidates: Vec<(u32, u32)> = Vec::new(); // (var id, pointer type) in UniformConstant
        let mut dec_set: HashMap<u32, u32> = HashMap::new();
        let mut dec_binding: HashMap<u32, u32> = HashMap::new();

        for &(op, s, len) in &insts {
            match op {
                OP_TYPE_IMAGE if len >= 2 => {
                    image_types.insert(words[s + 1]);
                }
                OP_TYPE_SAMPLER if len >= 2 => {
                    sampler_types.insert(words[s + 1]);
                    existing_sampler_type.get_or_insert(words[s + 1]);
                }
                OP_TYPE_SAMPLED_IMAGE if len >= 3 => {
                    sampled_to_image.insert(words[s + 1], words[s + 2]);
                }
                OP_TYPE_POINTER if len >= 4 && words[s + 2] == SC_UNIFORM_CONSTANT => {
                    let (ptr, pointee) = (words[s + 1], words[s + 3]);
                    if sampled_to_image.contains_key(&pointee) {
                        ptr_to_sampled.insert(ptr, pointee);
                    } else if image_types.contains(&pointee) {
                        ptr_to_image.entry(pointee).or_insert(ptr);
                    } else if sampler_types.contains(&pointee) {
                        ptr_to_sampler.entry(pointee).or_insert(ptr);
                    }
                }
                OP_VARIABLE if len >= 4 && words[s + 3] == SC_UNIFORM_CONSTANT => {
                    var_candidates.push((words[s + 2], words[s + 1])); // (result id, result-pointer type)
                }
                OP_DECORATE if len >= 4 && words[s + 2] == DEC_DESCRIPTOR_SET => {
                    dec_set.insert(words[s + 1], words[s + 3]);
                }
                OP_DECORATE if len >= 4 && words[s + 2] == DEC_BINDING => {
                    dec_binding.insert(words[s + 1], words[s + 3]);
                }
                _ => {}
            }
        }

        // Which UniformConstant variables are combined samplers (their pointer points to an OpTypeSampledImage)?
        let combined_var_ids: Vec<(u32, u32, u32)> = var_candidates
            .iter()
            .filter_map(|&(var, ptr)| {
                ptr_to_sampled
                    .get(&ptr)
                    .map(|&sampled| (var, sampled, sampled_to_image[&sampled]))
            })
            .collect();
        if combined_var_ids.is_empty() {
            return Ok(words.to_vec()); // nothing to split — leave the module untouched
        }
        hl_log::hl_count!(hl_log::tag::WGPU, "spirv_split");
        let combined_ids: HashSet<u32> = combined_var_ids.iter().map(|&(v, _, _)| v).collect();

        // A combined var used outside a plain `OpLoad` (e.g. an array `OpAccessChain`) is out of this pass's
        // scope: its declaration is removed, so any remaining reference becomes a dangling id and naga reports
        // its normal `InvalidId` — an honest error, never a wrong-shader substitution. (We do NOT scan operands
        // to pre-decline, because SPIR-V literal operands — a vector's component count, a decoration value —
        // can numerically equal an id and would false-trip such a check.)

        // ---- 3. Allocate ids for the new types / pointers / variables. ----------------------------------
        let mut next_id = words[3]; // the id bound (max id + 1)
        let alloc = |n: &mut u32| {
            let id = *n;
            *n += 1;
            id
        };

        let sampler_type = existing_sampler_type.unwrap_or_else(|| alloc(&mut next_id));
        let emit_sampler_type = existing_sampler_type.is_none();

        let (ptr_sampler, emit_ptr_sampler) = match ptr_to_sampler.get(&sampler_type) {
            Some(&p) => (p, false),
            None => (alloc(&mut next_id), true),
        };

        // A UniformConstant pointer per distinct image type (reuse an existing one), and which need emitting.
        let mut image_ptr: HashMap<u32, u32> = HashMap::new();
        let mut image_ptr_emit: HashSet<u32> = HashSet::new(); // image types whose pointer we synthesize
        for &(_, _, image_type) in &combined_var_ids {
            if let std::collections::hash_map::Entry::Vacant(e) = image_ptr.entry(image_type) {
                match ptr_to_image.get(&image_type) {
                    Some(&p) => {
                        e.insert(p);
                    }
                    None => {
                        e.insert(alloc(&mut next_id));
                        image_ptr_emit.insert(image_type);
                    }
                }
            }
        }

        // The per-variable split record.
        let mut info: HashMap<u32, CombinedSampler> = HashMap::new();
        for &(var, _sampled, image_type) in &combined_var_ids {
            let image_var = alloc(&mut next_id);
            let sampler_var = alloc(&mut next_id);
            info.insert(
                var,
                CombinedSampler {
                    image_type,
                    set: dec_set.get(&var).copied().unwrap_or(0),
                    binding: dec_binding.get(&var).copied().unwrap_or(0),
                    image_var,
                    sampler_var,
                    ptr_image: image_ptr[&image_type],
                },
            );
        }

        // ---- 5. Rebuild the word stream. ----------------------------------------------------------------
        let mut out: Vec<u32> = words[..HEADER_WORDS].to_vec();
        let mut decorations_injected = false;
        let mut sampler_type_emitted = !emit_sampler_type;
        let mut ptr_sampler_emitted = !emit_ptr_sampler;
        let mut image_ptr_emitted: HashSet<u32> = HashSet::new();

        for &(op, s, len) in &insts {
            let inst = &words[s..s + len];

            // New decorations belong in the annotations section — inject them just before the first type op.
            if !decorations_injected && Instruction::is_type(op) {
                for &(var, _, _) in &combined_var_ids {
                    let c = info[&var];
                    emit(
                        &mut out,
                        OP_DECORATE,
                        &[c.image_var, DEC_DESCRIPTOR_SET, c.set],
                    );
                    emit(
                        &mut out,
                        OP_DECORATE,
                        &[c.image_var, DEC_BINDING, c.binding],
                    );
                    emit(
                        &mut out,
                        OP_DECORATE,
                        &[c.sampler_var, DEC_DESCRIPTOR_SET, c.set],
                    );
                    emit(
                        &mut out,
                        OP_DECORATE,
                        &[
                            c.sampler_var,
                            DEC_BINDING,
                            c.binding + SAMPLER_BINDING_OFFSET,
                        ],
                    );
                }
                decorations_injected = true;
            }

            match op {
                // Drop the combined var's own annotations (re-emitted for the split vars above).
                OP_DECORATE | OP_MEMBER_DECORATE | OP_NAME
                    if len >= 2 && combined_ids.contains(&words[s + 1]) => {}

                // Replace the combined OpVariable with a separate image variable + sampler variable, emitting
                // any not-yet-emitted shared types first (all their referenced types are already declared here).
                OP_VARIABLE if len >= 3 && combined_ids.contains(&words[s + 2]) => {
                    let c = info[&words[s + 2]];
                    if !sampler_type_emitted {
                        emit(&mut out, OP_TYPE_SAMPLER, &[sampler_type]);
                        sampler_type_emitted = true;
                    }
                    if !ptr_sampler_emitted {
                        emit(
                            &mut out,
                            OP_TYPE_POINTER,
                            &[ptr_sampler, SC_UNIFORM_CONSTANT, sampler_type],
                        );
                        ptr_sampler_emitted = true;
                    }
                    if image_ptr_emit.contains(&c.image_type)
                        && image_ptr_emitted.insert(c.image_type)
                    {
                        emit(
                            &mut out,
                            OP_TYPE_POINTER,
                            &[c.ptr_image, SC_UNIFORM_CONSTANT, c.image_type],
                        );
                    }
                    emit(
                        &mut out,
                        OP_VARIABLE,
                        &[c.ptr_image, c.image_var, SC_UNIFORM_CONSTANT],
                    );
                    emit(
                        &mut out,
                        OP_VARIABLE,
                        &[ptr_sampler, c.sampler_var, SC_UNIFORM_CONSTANT],
                    );
                }

                // Expand `OpLoad %sampled %r %combined` into: load image, load sampler, recombine as %r.
                OP_LOAD if len >= 4 && combined_ids.contains(&words[s + 3]) => {
                    let c = info[&words[s + 3]];
                    let (result_type, result_id) = (words[s + 1], words[s + 2]);
                    let img_tmp = alloc(&mut next_id);
                    let smp_tmp = alloc(&mut next_id);
                    emit(&mut out, OP_LOAD, &[c.image_type, img_tmp, c.image_var]);
                    emit(&mut out, OP_LOAD, &[sampler_type, smp_tmp, c.sampler_var]);
                    emit(
                        &mut out,
                        OP_SAMPLED_IMAGE,
                        &[result_type, result_id, img_tmp, smp_tmp],
                    );
                }

                // Rewrite the entry-point interface (SPIR-V ≥ 1.4 lists global vars): swap a combined var for
                // its image + sampler vars. The literal name string is skipped before the interface id list.
                OP_ENTRY_POINT if len >= 3 => {
                    let ops = &words[s + 1..s + len];
                    let mut name_end = 2; // ops[0]=exec model, ops[1]=entry id, ops[2..]=name then interface
                    while name_end < ops.len() {
                        let w = ops[name_end];
                        name_end += 1;
                        if w.to_le_bytes().contains(&0) {
                            break; // last word of the null-terminated literal string
                        }
                    }
                    let mut new_ops: Vec<u32> = ops[..name_end.min(ops.len())].to_vec();
                    for &id in &ops[name_end.min(ops.len())..] {
                        match info.get(&id) {
                            Some(c) => new_ops.extend_from_slice(&[c.image_var, c.sampler_var]),
                            None => new_ops.push(id),
                        }
                    }
                    emit(&mut out, OP_ENTRY_POINT, &new_ops);
                }

                _ => out.extend_from_slice(inst),
            }
        }

        out[3] = next_id; // publish the grown id bound
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glslang-style COMBINED image-sampler fragment shader (an `OpTypeSampledImage` `UniformConstant`
    /// variable sampled with NO `OpSampledImage`) at descriptor set 0, binding `b`.
    fn combined_frag(b: u32) -> Vec<u32> {
        let (main, uv, color, tex) = (1u32, 2, 3, 4);
        let (void, fnty, float, v2, v4) = (5u32, 6, 7, 8, 9);
        let (image, sampled, ptr_s, ptr_in, ptr_out) = (10u32, 11, 12, 13, 14);
        let (label, ld_tex, ld_uv, sample, bound) = (15u32, 16, 17, 18, 19u32);
        let mut w: Vec<u32> = vec![0x0723_0203, 0x0001_0000, 0, bound, 0];
        let mut push = |op: u16, ops: &[u32]| {
            w.push(((ops.len() as u32 + 1) << 16) | op as u32);
            w.extend_from_slice(ops);
        };
        push(17, &[1]); // OpCapability Shader
        push(14, &[0, 1]); // OpMemoryModel Logical GLSL450
        push(15, &[4, main, 0x6E69_616D, 0, uv, color]); // OpEntryPoint Fragment %main "main" %uv %color
        push(16, &[main, 7]); // OpExecutionMode OriginUpperLeft
        push(71, &[uv, 30, 0]); // Decorate uv Location 0
        push(71, &[color, 30, 0]); // Decorate color Location 0
        push(71, &[tex, 34, 0]); // Decorate tex DescriptorSet 0
        push(71, &[tex, 33, b]); // Decorate tex Binding b
        push(19, &[void]);
        push(33, &[fnty, void]);
        push(22, &[float, 32]);
        push(23, &[v2, float, 2]);
        push(23, &[v4, float, 4]);
        push(25, &[image, float, 1, 0, 0, 0, 1, 0]);
        push(27, &[sampled, image]);
        push(32, &[ptr_s, 0, sampled]);
        push(59, &[ptr_s, tex, 0]);
        push(32, &[ptr_in, 1, v2]);
        push(59, &[ptr_in, uv, 1]);
        push(32, &[ptr_out, 3, v4]);
        push(59, &[ptr_out, color, 3]);
        push(54, &[void, main, 0, fnty]);
        push(248, &[label]);
        push(61, &[sampled, ld_tex, tex]);
        push(61, &[v2, ld_uv, uv]);
        push(87, &[v4, sample, ld_tex, ld_uv]);
        push(62, &[color, sample]);
        push(253, &[]);
        push(56, &[]);
        w
    }

    fn parses(words: &[u32]) -> bool {
        let bytes: &[u8] = bytemuck::cast_slice(words);
        naga::front::spv::parse_u8_slice(bytes, &naga::front::spv::Options::default()).is_ok()
    }

    #[test]
    fn raw_combined_sampler_is_rejected_but_split_is_accepted() {
        let raw = combined_frag(0);
        // FAIL-BEFORE: naga's spv-in cannot parse the combined image-sampler model.
        assert!(
            !parses(&raw),
            "the raw combined-sampler SPIR-V must be rejected by naga (the gap)"
        );
        // PASS-AFTER: the split rewrites it into the separate model naga accepts.
        let split = CombinedSamplers::split(&raw).unwrap();
        assert!(
            parses(&split),
            "the split SPIR-V must parse (separate image + sampler)"
        );
    }

    #[test]
    fn split_produces_texture_and_sampler_at_coordinated_bindings() {
        // Combined at guest binding 0 becomes texture 0 + sampler 16, then host reservation shifts both.
        let wgsl =
            crate::wgsl::spirv_to_wgsl(&combined_frag(0)).expect("spirv_to_wgsl through the split");
        assert!(
            wgsl.contains("texture_2d"),
            "expected a separate texture_2d: {wgsl}"
        );
        assert!(
            wgsl.contains("sampler"),
            "expected a separate sampler: {wgsl}"
        );
        assert!(
            wgsl.contains("@binding(17)"),
            "sampler must reflect at guest binding 0 + split offset 16 + host offset 1: {wgsl}"
        );

        // A combined descriptor at guest binding 3 pushes the native sampler to binding 20.
        let wgsl3 = crate::wgsl::spirv_to_wgsl(&combined_frag(3)).unwrap();
        assert!(
            wgsl3.contains("@binding(20)"),
            "guest binding 3 + split offset 16 + host offset 1 = 20: {wgsl3}"
        );
    }

    #[test]
    fn shader_without_combined_sampler_is_unchanged() {
        // A separate-model SPIR-V (naga-minted) carries no OpTypeSampledImage variable → passthrough.
        let seed =
            "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0,1.0,0.0,1.0); }";
        let module = naga::front::wgsl::parse_str(seed).unwrap();
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
        let spirv =
            naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
                .unwrap();
        let out = CombinedSamplers::split(&spirv).unwrap();
        assert_eq!(
            out, spirv,
            "a shader with no combined sampler must be returned byte-for-byte"
        );
    }
}
