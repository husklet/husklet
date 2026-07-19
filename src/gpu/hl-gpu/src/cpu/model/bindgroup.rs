//! The CPU-native bind group: the descriptor plus the generation of every resource it referenced at
//! creation time, so a later use can detect that an id was destroyed and (possibly) reused since binding.
//! Ported from `BindGroupState` / `GenRef` in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::BindGroupDesc;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, SamplerId, TextureId};
use crate::runtime::model::resources::SessionResources;

/// A generation stamp captured for one resource a bind group references.
#[derive(Clone, Copy)]
pub struct GenRef {
    pub id: u32,
    pub gen: u32,
}

pub struct BindGroupState {
    pub desc: BindGroupDesc,
    pub buffers: Vec<GenRef>,
    pub textures: Vec<GenRef>,
    pub samplers: Vec<GenRef>,
}

impl BindGroupState {
    /// Verifies that every captured resource still names the allocation used when this group was created.
    pub fn validate(&self, resources: &SessionResources) -> Result<()> {
        for reference in &self.buffers {
            if resources.buffers.generation(reference.id) != Some(reference.gen) {
                return Err(GpuError::UnknownId {
                    kind: BufferId::KIND,
                    id: reference.id,
                });
            }
        }
        for reference in &self.textures {
            if resources.textures.generation(reference.id) != Some(reference.gen) {
                return Err(GpuError::UnknownId {
                    kind: TextureId::KIND,
                    id: reference.id,
                });
            }
        }
        for reference in &self.samplers {
            if resources.samplers.generation(reference.id) != Some(reference.gen) {
                return Err(GpuError::UnknownId {
                    kind: SamplerId::KIND,
                    id: reference.id,
                });
            }
        }
        Ok(())
    }
}
