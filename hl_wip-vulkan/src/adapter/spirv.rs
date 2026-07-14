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
    Ok(words)
}

/// Validate a SPIR-V word image's header (length + magic). No body translation — this is a passthrough.
pub fn validate(words: &[u32]) -> Result<()> {
    if words.len() < HEADER_WORDS || words[0] != SPIRV_MAGIC {
        return Err(GpuError::Invalid("vkCreateShaderModule: not a SPIR-V module"));
    }
    Ok(())
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
