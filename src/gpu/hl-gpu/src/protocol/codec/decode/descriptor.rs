impl Capabilities {
    /// Decode a handshake descriptor produced by [`Capabilities::encode`](super::encode).
    ///
    /// Crate-private on purpose: the trailing high format half is PRESENCE-gated on `remaining()`, which is
    /// only exact inside a frame. [`Capabilities::from_handshake`] is the sole entry point, so appending a
    /// future field can never read into the next message's bytes.
    pub(crate) fn decode(d: &mut Decoder) -> Result<Capabilities> {
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
        let texture_formats_low = d.u32()?;
        let binding_arrays = d.u32()?;
        let non_uniform_binding_arrays = d.u32()?;
        let gpu_features = if wire_version >= 11 { d.u32()? } else { 0 };
        let pbits = d.u32()?;
        // Optional trailing high words of the 128-bit format bitset (see `Capabilities::encode`).
        let mut texture_formats = u128::from(texture_formats_low);
        for shift in [32, 64, 96] {
            if d.remaining() >= 4 {
                texture_formats |= u128::from(d.u32()?) << shift;
            }
        }
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
            binding_arrays,
            non_uniform_binding_arrays,
            gpu_features,
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

impl<'a> Decoder<'a> {
    pub(crate) fn pipeline_layout(&mut self) -> Result<PipelineLayout> {
        let count = self.u32()? as usize;
        let count = self.cap_count(count, 16);
        let mut bindings = Vec::with_capacity(count);
        for _ in 0..count {
            let group = self.u32()?;
            let binding = self.u32()?;
            let count = self.u32()?;
            let kind = PipelineBindingKind::from_u32(self.u32()?)?;
            if count == 0 {
                return Err(GpuError::Invalid("pipeline binding count must be non-zero"));
            }
            if bindings
                .iter()
                .any(|item: &PipelineBinding| item.group == group && item.binding == binding)
            {
                return Err(GpuError::Invalid("duplicate pipeline binding"));
            }
            bindings.push(PipelineBinding {
                group,
                binding,
                count,
                kind,
            });
        }
        Ok(PipelineLayout { bindings })
    }
}
use super::*;
