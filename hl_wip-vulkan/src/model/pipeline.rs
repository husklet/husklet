//! Shader-module / pipeline / pipeline-layout records.
//!
//! Ported from `hl-shim-vk/src/pipeline.rs` (`vkCreateShaderModule`, `vkCreate{Compute,Graphics}Pipelines`,
//! `vkCreatePipelineLayout` bodies), mirroring MoltenVK's `MVKShaderModule`/`MVKPipeline`/
//! `MVKPipelineLayout`. A `VkShaderModule` carries its **SPIR-V words verbatim** (the IR shader ABI is
//! SPIR-V) plus the `OpEntryPoint` names parsed out of it (see [`crate::adapter::spirv`]).

use crate::VkDescriptorSetLayout;

/// One `VkShaderModule`: the backing hl-GPU shader id + the SPIR-V words (forwarded to the IR with no
/// translation) + the parsed entry-point names (from `OpEntryPoint`). Mirrors `MVKShaderModule`.
#[derive(Clone, PartialEq, Debug)]
pub struct ShaderRec {
    pub ir_id: u32,
    pub spirv: Vec<u32>,
    pub entries: Vec<String>,
}

impl ShaderRec {
    /// Whether this module declares an entry point named `name` (a `vkCreate*Pipelines` `pName` must
    /// resolve to one — a missing entry fails the pipeline, no id-zero default).
    pub fn has_entry(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e == name)
    }
}

/// Whether a `VkPipeline` is a graphics or compute pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PipelineKind {
    Compute,
    Graphics,
}

/// One `VkPipeline`: the backing hl-GPU pipeline id + its kind. Mirrors `MVKPipeline`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PipelineRec {
    pub ir_id: u32,
    pub kind: PipelineKind,
}

/// One `VkPipelineLayout`: the descriptor-set layouts it composes (compatibility is by set-layout).
/// Mirrors `MVKPipelineLayout`. No IR is emitted for a layout — bindings arrive with descriptor sets.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PipelineLayoutRec {
    pub set_layouts: Vec<VkDescriptorSetLayout>,
}
