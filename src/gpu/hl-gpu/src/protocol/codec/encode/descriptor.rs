impl Capabilities {
    /// Serialize this descriptor into the connection handshake byte-stream (the guest decodes it with
    /// [`Capabilities::decode`] and negotiates before advertising any API feature).
    pub fn encode(&self, e: &mut Encoder) {
        e.u32(self.wire_version);
        e.str(&self.name);
        e.bool(self.unified_memory);
        e.bool(self.supports_compute);
        e.bool(self.supports_graphics);
        e.bool(self.supports_timeline_fences);
        e.u32(self.max_texture_2d);
        e.u32(self.max_bind_groups);
        e.u64(self.max_frame_bytes);
        e.u64(self.max_buffer_bytes);
        e.u64(self.command_bits);
        e.u32(self.shader_payloads);
        e.u32(self.texture_formats);
        e.u32(self.binding_arrays);
        e.u32(self.non_uniform_binding_arrays);
        if self.wire_version >= 11 {
            e.u32(self.gpu_features);
        }
        e.u32(self.present_bits());
    }

    /// Serialize to a standalone handshake frame (u32 length + body).
    pub fn to_handshake(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.frame(|inner| self.encode(inner));
        e.into_vec()
    }
}

// ---------------------------------------------------------------------------------------------------
// kernel descriptor → CreateShader words
// ---------------------------------------------------------------------------------------------------

impl KernelDescriptor {
    /// Serialize into `CreateShader` shader words: `[MAGIC, byte_len, ...packed bytes...]`.
    pub fn to_words(&self) -> Vec<u32> {
        let mut e = Encoder::new();
        e.str(&self.ptx);
        e.str(&self.entry);
        for v in self.block {
            e.u32(v);
        }
        let bytes = e.into_vec();
        let mut words = Vec::with_capacity(2 + bytes.len() / 4 + 1);
        words.push(KERNEL_MAGIC);
        words.push(bytes.len() as u32);
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(b));
        }
        words
    }
}

// ---------------------------------------------------------------------------------------------------
// GLSL descriptor → CreateShader words
// ---------------------------------------------------------------------------------------------------

impl GlslDescriptor {
    /// Serialize into `CreateShader` shader words led by [`GLSL_MAGIC`]:
    /// `[GLSL_MAGIC, byte_len, ...packed(stage, entry, source)...]`. The leading magic is what the decoder
    /// classifies the payload by (→ [`super::super::model::command::ShaderPayloadKind::Glsl`]), exactly as
    /// SPIR-V / kernel payloads are self-identifying.
    pub fn to_words(&self) -> Vec<u32> {
        let mut e = Encoder::new();
        e.u32(self.stage);
        e.str(&self.entry);
        e.str(&self.source);
        let bytes = e.into_vec();
        let mut words = Vec::with_capacity(2 + bytes.len() / 4 + 1);
        words.push(GLSL_MAGIC);
        words.push(bytes.len() as u32);
        for chunk in bytes.chunks(4) {
            let mut b = [0u8; 4];
            b[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(b));
        }
        words
    }
}

impl Encoder {
    pub(crate) fn pipeline_layout(&mut self, layout: &PipelineLayout) {
        self.u32(layout.bindings.len() as u32);
        for binding in &layout.bindings {
            self.u32(binding.group);
            self.u32(binding.binding);
            self.u32(binding.count);
            self.u32(binding.kind.to_u32());
        }
    }
}
use super::*;
