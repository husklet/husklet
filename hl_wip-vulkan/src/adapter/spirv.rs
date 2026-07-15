//! SPIR-V passthrough front-end — the seam's keystone.
//!
//! A `VkShaderModule`'s `pCode` is SPIR-V words, and the hl-GPU IR shader payload
//! ([`hl_gpu::Cmd::CreateShader`] `{ kind: SpirV, spirv }`) is ALSO SPIR-V, so the guest side does NO
//! translation — it only validates the module header and parses the `OpEntryPoint` names (so a
//! pipeline's `pName` can be resolved against the module, matching a real driver's rejection of a
//! missing entry point). Ported from `hl-shim-vk/src/{pipeline.rs,ir_seam.rs}` (`vkCreateShaderModule`
//! forwards `spirv.clone()` untouched; `parse_spirv` reads `OpEntryPoint`).
//!
//! The one behavioural change from the shipping shim: a malformed header is the protocol's typed
//! [`GpuError::Invalid`] (the shim returned `VK_ERROR_UNKNOWN`); [`crate::result`] maps it back.

use hl_gpu::{Cmd, GpuError, Result, ShaderPayloadKind};

/// The SPIR-V magic number (`vk.xml` / SPIR-V spec §2.3), little-endian in `pCode[0]`.
pub const SPIRV_MAGIC: u32 = 0x0723_0203;
/// `OpEntryPoint` opcode (SPIR-V spec).
const OP_ENTRY_POINT: u32 = 15;
/// Minimum SPIR-V module: the 5-word header.
const HEADER_WORDS: usize = 5;

/// Reinterpret a `VkShaderModuleCreateInfo::pCode` byte image (`codeSize` bytes, little-endian) as
/// SPIR-V words. Errors on a non-multiple-of-4 size or a too-short/mis-magicked module (the driver's
/// `VK_ERROR_UNKNOWN` case). Ported from `vkCreateShaderModule`'s `code_size` guards.
pub fn words_from_bytes(code: &[u8]) -> Result<Vec<u32>> {
    if code.len() % 4 != 0 || code.len() < HEADER_WORDS * 4 {
        return Err(GpuError::Invalid("vkCreateShaderModule: SPIR-V code size invalid"));
    }
    let words: Vec<u32> = code
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    validate(&words)?;
    Ok(strip_point_size(words))
}

/// Validate a SPIR-V word image's header (length + magic). No body translation — this is a passthrough.
pub fn validate(words: &[u32]) -> Result<()> {
    if words.len() < HEADER_WORDS || words[0] != SPIRV_MAGIC {
        return Err(GpuError::Invalid("vkCreateShaderModule: not a SPIR-V module"));
    }
    Ok(())
}

// SPIR-V opcodes touched by the point-size sanitizer (SPIR-V spec §3.37).
const OP_NAME: u32 = 5;
const OP_DECORATE: u32 = 71;
const OP_DECORATE_ID: u32 = 332;
const OP_DECORATE_STRING: u32 = 5632;
const OP_VARIABLE: u32 = 59;
const OP_STORE: u32 = 62;
/// `Decoration::BuiltIn` and `BuiltIn::PointSize` operand values (SPIR-V spec §3.20 / §3.21).
const DECORATION_BUILTIN: u32 = 11;
const BUILTIN_POINT_SIZE: u32 = 1;

/// Strip the `BuiltIn PointSize` output from a SPIR-V module so the host executor's naga WGSL backend
/// (`wgsl-out`, which has no representation for a point size) accepts it.
///
/// WHY: wgpu's naga SPIR-V writer emits a *synthesized* `PointSize` output variable for every vertex
/// entry point — an `OpVariable` decorated `BuiltIn PointSize` that unconditionally stores `1.0`. WGSL
/// has no `point_size` builtin, so when our host re-ingests that SPIR-V and re-emits WGSL the translation
/// fails (`Unsupported builtin PointSize`) and the whole `vkCreateShaderModule` is rejected — losing the
/// device on the app's first real shader. Point size only affects `PointList` rasterization, which Zed
/// (and any triangle/quad UI) never uses, and it is not representable downstream regardless, so removing
/// this write is behaviour-preserving for every non-point pipeline.
///
/// The removal is precise: it drops only a standalone output variable whose *sole* decoration is
/// `BuiltIn PointSize` (naga's exact pattern) — its `OpVariable`, every `OpStore` into it, all its
/// decorations/names, and its `OpEntryPoint` interface reference. If a module has no such variable it is
/// returned untouched, so non-vertex / non-naga modules are never perturbed.
pub fn strip_point_size(words: Vec<u32>) -> Vec<u32> {
    // Collect the ids decorated `BuiltIn PointSize` (a plain variable id, not a struct member — naga
    // emits a standalone variable, and rewriting a struct member's type/offsets is unsafe, so those are
    // left alone).
    let mut targets: Vec<u32> = Vec::new();
    for (op, ops) in instructions(&words) {
        if op == OP_DECORATE
            && ops.len() >= 4
            && ops[2] == DECORATION_BUILTIN
            && ops[3] == BUILTIN_POINT_SIZE
        {
            targets.push(ops[1]);
        }
    }
    if targets.is_empty() {
        return words;
    }
    let is_target = |id: u32| targets.contains(&id);

    let mut out: Vec<u32> = words[..HEADER_WORDS].to_vec();
    for (op, ops) in instructions(&words) {
        // Drop the point-size variable's definition, its stores, and all its names/decorations.
        let drop = match op {
            OP_VARIABLE => ops.len() >= 3 && is_target(ops[2]),
            OP_STORE => ops.len() >= 2 && is_target(ops[1]),
            OP_NAME | OP_DECORATE | OP_DECORATE_ID | OP_DECORATE_STRING => {
                ops.len() >= 2 && is_target(ops[1])
            }
            _ => false,
        };
        if drop {
            continue;
        }
        if op == OP_ENTRY_POINT {
            out.extend(entry_point_without(&ops, &targets));
        } else {
            out.extend_from_slice(ops);
        }
    }
    out
}

/// Re-emit an `OpEntryPoint` instruction with the given interface ids removed and its word count fixed.
/// Operands: `[packed, executionModel, entryId, name…(NUL-terminated), interface…]`.
fn entry_point_without(ops: &[u32], remove: &[u32]) -> Vec<u32> {
    // Locate the end of the inline entry-point name (a NUL-terminated literal string).
    let mut name_end = 3;
    while name_end < ops.len() {
        let w = ops[name_end];
        name_end += 1;
        if w.to_le_bytes().contains(&0) {
            break;
        }
    }
    let mut rebuilt: Vec<u32> = ops[1..name_end].to_vec(); // execModel, entryId, name…
    for &id in &ops[name_end..] {
        if !remove.contains(&id) {
            rebuilt.push(id);
        }
    }
    let count = (rebuilt.len() + 1) as u32; // + the packed opcode/count word itself
    let mut result = Vec::with_capacity(rebuilt.len() + 1);
    result.push((count << 16) | OP_ENTRY_POINT);
    result.extend(rebuilt);
    result
}

/// Iterate a SPIR-V module's instructions after the 5-word header, yielding `(opcode, operand_words)`
/// where `operand_words` is the whole instruction (packed word included at index 0). Stops on a
/// malformed/truncated instruction.
fn instructions(words: &[u32]) -> impl Iterator<Item = (u32, &[u32])> {
    let mut i = HEADER_WORDS;
    std::iter::from_fn(move || {
        if i >= words.len() {
            return None;
        }
        let w0 = words[i];
        let count = (w0 >> 16) as usize;
        let opcode = w0 & 0xffff;
        if count == 0 || i + count > words.len() {
            return None;
        }
        let inst = &words[i..i + count];
        i += count;
        Some((opcode, inst))
    })
}

/// The `OpEntryPoint` names declared in a SPIR-V module, in declaration order. Real + testable — walks
/// the instruction stream after the 5-word header, reading each instruction's `wordCount`/`opcode`
/// packed word and decoding the null-terminated literal name of every `OpEntryPoint`.
pub fn entry_points(words: &[u32]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = HEADER_WORDS;
    while i < words.len() {
        let w0 = words[i];
        let count = (w0 >> 16) as usize;
        let opcode = w0 & 0xffff;
        if count == 0 || i + count > words.len() {
            break; // malformed / truncated — stop rather than read out of range
        }
        // OpEntryPoint operands: [executionModel, entryPointId, name(literal string), interface…].
        if opcode == OP_ENTRY_POINT && count >= 4 {
            out.push(decode_string(&words[i + 3..i + count]));
        }
        i += count;
    }
    out
}

/// Decode a SPIR-V literal string (little-endian packed bytes, NUL-terminated) from a word slice.
fn decode_string(words: &[u32]) -> String {
    let mut bytes = Vec::with_capacity(words.len() * 4);
    'outer: for w in words {
        for b in w.to_le_bytes() {
            if b == 0 {
                break 'outer;
            }
            bytes.push(b);
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Build the [`Cmd::CreateShader`] that forwards `words` to the host **verbatim** — the keystone: no
/// translation, `kind = SpirV`. Ported from `ir_seam::create_shader_module`.
pub fn create_shader(id: u32, words: Vec<u32>) -> Cmd {
    Cmd::CreateShader { id, kind: ShaderPayloadKind::SpirV, spirv: words }
}

/// A minimal but valid single-`OpEntryPoint` compute SPIR-V module declaring `entry "main"`
/// (`GLCompute`). Used by the lowering tests as the SPIR-V payload the seam forwards verbatim.
pub fn sample_compute_spirv(entry: &str) -> Vec<u32> {
    // 5-word header: magic, version 1.0, generator 0, bound, schema 0.
    let mut words = vec![SPIRV_MAGIC, 0x0001_0000, 0, 2, 0];
    // OpEntryPoint GLCompute(5) %1 "<entry>" — pack the name into NUL-terminated LE words.
    let name_words = encode_string(entry);
    let count = (1 + 1 + 1 + name_words.len()) as u32; // op + execModel + id + name
    words.push((count << 16) | OP_ENTRY_POINT);
    words.push(5); // ExecutionModel = GLCompute
    words.push(1); // entry point id
    words.extend(name_words);
    words
}

/// Encode a string as SPIR-V literal words (little-endian, NUL-terminated, zero-padded to a word).
fn encode_string(s: &str) -> Vec<u32> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0); // NUL terminator
    while bytes.len() % 4 != 0 {
        bytes.push(0);
    }
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pack one SPIR-V instruction: `[(count<<16)|opcode, operands…]`.
    fn inst(op: u32, operands: &[u32]) -> Vec<u32> {
        let count = (operands.len() + 1) as u32;
        let mut v = vec![(count << 16) | op];
        v.extend_from_slice(operands);
        v
    }

    /// A tiny vertex module with a `Position` output (id 2) AND a naga-style standalone
    /// `BuiltIn PointSize` output (id 3): entry-point interface, name, decoration, variable, store.
    fn module_with_point_size() -> Vec<u32> {
        let mut w = vec![SPIRV_MAGIC, 0x0001_0000, 0, 100, 0];
        // OpEntryPoint Vertex(0) %1 "main" %2 %3
        let mut ep = vec![0u32, 1];
        ep.extend(encode_string("main"));
        ep.extend([2, 3]);
        w.extend(inst(OP_ENTRY_POINT, &ep));
        w.extend(inst(OP_NAME, &{
            let mut o = vec![3u32];
            o.extend(encode_string("gl_PointSize"));
            o
        }));
        w.extend(inst(OP_DECORATE, &[2, DECORATION_BUILTIN, 0])); // Position
        w.extend(inst(OP_DECORATE, &[3, DECORATION_BUILTIN, BUILTIN_POINT_SIZE]));
        w.extend(inst(OP_VARIABLE, &[10, 2, 3])); // %2 : type %10, Output
        w.extend(inst(OP_VARIABLE, &[10, 3, 3])); // %3 : type %10, Output (the point size)
        w.extend(inst(OP_STORE, &[3, 20])); // store 1.0 into %3
        w
    }

    fn has_point_size_decoration(words: &[u32]) -> bool {
        instructions(words).any(|(op, ops)| {
            op == OP_DECORATE
                && ops.len() >= 4
                && ops[2] == DECORATION_BUILTIN
                && ops[3] == BUILTIN_POINT_SIZE
        })
    }

    /// `%3` (the point-size id) must not survive anywhere: not as a variable/store target, not in the
    /// entry-point interface, not in any decoration/name.
    fn references_id(words: &[u32], id: u32) -> bool {
        instructions(words).any(|(op, ops)| match op {
            OP_VARIABLE => ops.get(2) == Some(&id),
            OP_STORE => ops.get(1) == Some(&id),
            OP_NAME | OP_DECORATE => ops.get(1) == Some(&id),
            OP_ENTRY_POINT => ops[3..].contains(&id),
            _ => false,
        })
    }

    #[test]
    fn strips_naga_point_size_output() {
        let m = module_with_point_size();
        assert!(has_point_size_decoration(&m), "fixture must contain a PointSize builtin");
        assert_eq!(entry_points(&m), vec!["main".to_string()]);

        let s = strip_point_size(m);

        // The header is intact and the entry point still parses.
        assert_eq!(s[0], SPIRV_MAGIC);
        assert!(validate(&s).is_ok());
        assert_eq!(entry_points(&s), vec!["main".to_string()], "entry point name survives the rewrite");
        // The PointSize output (id 3) is gone entirely; the Position output (id 2) is untouched.
        assert!(!has_point_size_decoration(&s), "PointSize decoration removed");
        assert!(!references_id(&s, 3), "no dangling reference to the removed point-size id remains");
        assert!(references_id(&s, 2), "the Position output variable is preserved");
    }

    #[test]
    fn passes_through_modules_without_point_size() {
        // A module with no PointSize builtin must be returned byte-for-byte unchanged.
        let m = sample_compute_spirv("main");
        assert_eq!(strip_point_size(m.clone()), m);
    }
}
