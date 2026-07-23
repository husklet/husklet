impl Capabilities {
    /// Decode a handshake descriptor produced by [`Capabilities::encode`](super::encode).
    pub fn decode(d: &mut Decoder) -> Result<Capabilities> {
        let wire_version = d.u32()?;
        let name = d.str()?;
        let unified_memory = d.bool()?;
        let supports_compute = d.bool()?;
        let supports_graphics = d.bool()?;
        let supports_timeline_fences = d.bool()?;
        let max_texture_2d = d.u32()?;
        let max_bind_groups = d.u32()?;
        let max_frame_bytes = d.u64()?;
        let max_buffer_bytes = d.u64()?;
        let command_bits = d.u64()?;
        let shader_payloads = d.u32()?;
        let texture_formats = d.u32()?;
        let pbits = d.u32()?;
        Ok(Capabilities {
            name,
            unified_memory,
            supports_compute,
            supports_graphics,
            max_texture_2d,
            present_kinds: Capabilities::present_kinds_from_bits(pbits),
            wire_version,
            command_bits,
            shader_payloads,
            texture_formats,
            max_frame_bytes,
            max_buffer_bytes,
            max_bind_groups,
            supports_timeline_fences,
        })
    }

    /// Decode a handshake frame (u32 length + body) written by
    /// [`Capabilities::to_handshake`](super::encode).
    pub fn from_handshake(bytes: &[u8]) -> Result<Capabilities> {
        let mut d = Decoder::new(bytes);
        d.frame(Capabilities::decode)
    }
}

// ---------------------------------------------------------------------------------------------------
// kernel descriptor ← CreateShader words
// ---------------------------------------------------------------------------------------------------

impl KernelDescriptor {
    /// Decode from shader words. Returns `None` if the words are not a kernel descriptor (i.e. SPIR-V).
    pub fn from_words(words: &[u32]) -> Option<Result<Self>> {
        if words.len() < 2 || words[0] != KERNEL_MAGIC {
            return None;
        }
        let byte_len = words[1] as usize;
        let mut bytes = Vec::with_capacity((words.len() - 2) * 4);
        for &w in &words[2..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() < byte_len {
            return Some(Err(GpuError::Kernel("kernel descriptor truncated".into())));
        }
        bytes.truncate(byte_len);
        let mut d = Decoder::new(&bytes);
        Some((|| {
            let ptx = d.str()?;
            let entry = d.str()?;
            let block = [d.u32()?, d.u32()?, d.u32()?];
            Ok(KernelDescriptor { ptx, entry, block })
        })())
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL descriptor ← CreateShader words
// ---------------------------------------------------------------------------------------------------

impl GlslDescriptor {
    /// Decode from shader words. Returns `None` if the words are not a GLSL descriptor (leading word is not
    /// [`GLSL_MAGIC`]) — the mirror of [`KernelDescriptor::from_words`].
    pub fn from_words(words: &[u32]) -> Option<Result<Self>> {
        if words.len() < 2 || words[0] != GLSL_MAGIC {
            return None;
        }
        let byte_len = words[1] as usize;
        let mut bytes = Vec::with_capacity((words.len() - 2) * 4);
        for &w in &words[2..] {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        if bytes.len() < byte_len {
            return Some(Err(GpuError::Kernel("glsl descriptor truncated".into())));
        }
        bytes.truncate(byte_len);
        let mut d = Decoder::new(&bytes);
        Some((|| {
            let stage = d.u32()?;
            let entry = d.str()?;
            let source = d.str()?;
            Ok(GlslDescriptor {
                stage,
                entry,
                source,
            })
        })())
    }
}
use super::*;
