//! Native shader modules — the point where each protocol shader payload becomes something wgpu compiles.
//!
//! A SPIR-V payload (spv-in) or a forwarded GLSL payload (glsl-in, the `GlslDescriptor` the GLES/GL driver
//! ships) is translated to WGSL by naga at create time and handed to `create_shader_module`, so the guest's
//! real vertex/fragment shader executes on the device. A neutral *kernel* payload (`PtxKernel`) is kept as
//! its compiled [`KernelProgram`] and lowered to a WGSL compute entry point when a compute pipeline
//! references it (see `pipeline.rs`). MSL / demo-builtin payloads have no honest WGSL translation and
//! are rejected rather than silently substituted.

use hl_gpu::protocol::model::command::ShaderPayloadKind;
use hl_gpu::protocol::model::kernel::{
    glsl_stage, GlslDescriptor, KernelDescriptor, KernelProgram,
};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::wgsl;
use crate::WgpuExecutor;

/// The wgpu-native backing of one protocol shader module.
pub enum ShaderNative {
    /// A naga-translated wgpu shader module (from a SPIR-V or GLSL payload). Backs a render pipeline
    /// (vertex/fragment) OR — when the payload declares a compute entry point — a compute pipeline built
    /// with an auto layout (see `pipeline::create_compute_pipeline`). A single naga round-trip serves both
    /// stages, so the variant is stage-neutral rather than graphics-only.
    ///
    /// `reflected` carries each entry point's USED resource bindings ([`crate::reflect`]), captured from
    /// the naga module at translate time — the exact set wgpu's auto layout exposes. A render pipeline
    /// stores the union of its vertex + fragment entry points' sets and filters the driver's bind group to
    /// it at draw time, so a bind group carrying resources the compiled shader never samples still matches
    /// the auto layout. Unused by the compute path (whose bind groups match their layout as-is).
    ///
    /// `key` is this module's content identity ([`crate::dedup::ShaderKey`]) — the payload kind + verbatim
    /// words it was compiled from. Every `Module` shares its backing through the executor's dedup cache
    /// keyed on this; on `DestroyShader` it releases this id's alias of that shared backing.
    Module {
        module: wgpu::ShaderModule,
        reflected: crate::reflect::ModuleUsage,
        key: crate::dedup::ShaderKey,
    },
    /// A compiled compute kernel, lowered to WGSL lazily at compute-pipeline creation.
    Kernel(Box<KernelProgram>),
}

impl ShaderNative {
    /// Downcast a live shader id to its native handle.
    pub fn get(res: &SessionResources, id: u32) -> Result<&Self> {
        res.shaders
            .get(id)?
            .downcast_ref::<Self>()
            .ok_or(GpuError::Invalid("wgpu: shader native type mismatch"))
    }
}

impl WgpuExecutor {
    pub(crate) fn create_shader(
        &mut self,
        res: &mut SessionResources,
        id: u32,
        kind: ShaderPayloadKind,
        words: &[u32],
    ) -> Result<()> {
        if words.is_empty() {
            return Err(GpuError::Invalid("empty shader module"));
        }
        hl_log::hl_debug!(
            hl_log::tag::WGPU,
            "create_shader kind={:?} words={}",
            kind,
            words.len()
        );

        // Kernel payloads are compiled per-id (a `define_kernel`/front-end program keyed on the id) and are
        // not on the Chrome recreate-every-flush hot path, so they are NOT content-deduped: each keeps its
        // own `KernelProgram`.
        if let ShaderPayloadKind::PtxKernel = kind {
            // Mirror the CPU oracle: a real driver-produced descriptor (non-empty source) compiles on the
            // fly via the injected front-end; an empty placeholder resolves to a `define_kernel`
            // pre-registered program (the hand-built kernels the conformance suite injects).
            let prog = match KernelDescriptor::from_words(words) {
                Some(Ok(desc)) if !desc.ptx.is_empty() => {
                    let compiler = self.kernel_compiler.as_ref().ok_or(GpuError::Unsupported(
                        "wgpu: PtxKernel payload needs a kernel compiler (set_kernel_compiler)",
                    ))?;
                    compiler(&desc)?
                }
                _ => self.kernels.get(&id).cloned().ok_or(GpuError::Unsupported(
                    "wgpu: no compiled kernel registered for PtxKernel shader id",
                ))?,
            };
            return res
                .shaders
                .insert(id, Box::new(ShaderNative::Kernel(Box::new(prog))));
        }

        if matches!(
            kind,
            ShaderPayloadKind::Msl | ShaderPayloadKind::DemoBuiltin
        ) {
            hl_log::hl_warn!(
                hl_log::tag::WGPU,
                "shader rejected kind={:?} reason=no-wgsl",
                kind
            );
            return Err(GpuError::Unsupported(
                "wgpu: MSL / demo-builtin payloads (no WGSL)",
            ));
        }

        // Content-dedup the two compiled-module payload kinds (SPIR-V + forwarded GLSL). The key is the
        // EXACT compilation input — payload kind + verbatim words — so a `CreateShader` whose source matches
        // an already-compiled module ALIASES it (a cheap `Arc` clone of the shared `wgpu::ShaderModule`,
        // ~0 incremental residency) instead of re-running naga + recompiling. Distinct sources never share
        // (full-value key compare, no lossy hash).
        let key = crate::dedup::ShaderKey {
            kind: kind as u8,
            words: words.to_vec(),
        };
        if let Some((module, reflected)) = self.dedup.shader_get(&key) {
            hl_log::hl_count!(hl_log::tag::WGPU, "shader_dedup_hit");
            return res.shaders.insert(
                id,
                Box::new(ShaderNative::Module {
                    module,
                    reflected,
                    key,
                }),
            );
        }

        // Cache miss: translate to WGSL and compile a fresh module, then charge one backing's residency.
        let (src, reflected, label) = match kind {
            ShaderPayloadKind::SpirV => {
                let (src, reflected) = wgsl::Spirv::translate_reflect(words).map_err(|e| {
                    hl_log::hl_warn!(
                        hl_log::tag::WGPU,
                        "shader compile failed kind=SpirV err={}",
                        e
                    );
                    hl_log::hl_count!(hl_log::tag::WGPU, "shader_errors");
                    e
                })?;
                (src, reflected, "hl-spirv")
            }
            ShaderPayloadKind::Glsl => {
                // The guest GLES/GL driver forwards its GLSL source VERBATIM (a `GlslDescriptor` led by
                // `GLSL_MAGIC`); the host owns the compiler. naga's `glsl-in` lowers it to a naga module,
                // which `wgsl-out` writes as the WGSL wgpu compiles — so real GL geometry rasterizes on the
                // device instead of the old MSL-pretranslated payload the executor could not consume.
                let desc = GlslDescriptor::from_words(words)
                    .ok_or(GpuError::Invalid("wgpu: GLSL payload missing GLSL_MAGIC"))??;
                let stage = match desc.stage {
                    glsl_stage::VERTEX => naga::ShaderStage::Vertex,
                    glsl_stage::FRAGMENT => naga::ShaderStage::Fragment,
                    glsl_stage::COMPUTE => naga::ShaderStage::Compute,
                    other => {
                        return Err(GpuError::Kernel(format!(
                            "wgpu: unknown GLSL stage {other}"
                        )))
                    }
                };
                let (src, reflected) = wgsl::glsl_to_wgsl_reflect(&desc.source, stage, &desc.entry)
                    .map_err(|e| {
                        hl_log::hl_warn!(
                            hl_log::tag::WGPU,
                            "shader compile failed kind=Glsl err={}",
                            e
                        );
                        hl_log::hl_count!(hl_log::tag::WGPU, "shader_errors");
                        e
                    })?;
                (src, reflected, "hl-glsl")
            }
            // PtxKernel / Msl / DemoBuiltin already returned above.
            _ => unreachable!("kernel and non-WGSL kinds handled above"),
        };
        let module = self
            .gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            });
        let bytes = key.backing_bytes();
        // Insert the per-id native FIRST (it may reject a duplicate id); only register the shared backing
        // once the id is genuinely live, so a `DuplicateId` does not leave a phantom refcount.
        res.shaders.insert(
            id,
            Box::new(ShaderNative::Module {
                module: module.clone(),
                reflected: reflected.clone(),
                key: key.clone(),
            }),
        )?;
        self.dedup.shader_install(key, module, reflected, bytes);
        Ok(())
    }

    /// Destroy a shader id, releasing its alias of the shared compiled-module backing (a no-op for a kernel
    /// payload, which is not deduped). Reads the id's content key BEFORE removing it, then removes (which
    /// may raise `UnknownId` for a double-free — propagated before any refcount change).
    pub(crate) fn destroy_shader(&mut self, res: &mut SessionResources, id: u32) -> Result<()> {
        let key = match res.shaders.get(id) {
            Ok(n) => n.downcast_ref::<ShaderNative>().and_then(|n| match n {
                ShaderNative::Module { key, .. } => Some(key.clone()),
                ShaderNative::Kernel(_) => None,
            }),
            Err(_) => None,
        };
        res.shaders.remove(id)?;
        if let Some(key) = key {
            self.dedup.shader_release(&key);
        }
        Ok(())
    }
}
