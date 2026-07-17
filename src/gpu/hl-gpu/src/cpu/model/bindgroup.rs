//! The CPU-native bind group: the descriptor plus the generation of every resource it referenced at
//! creation time, so a later use can detect that an id was destroyed and (possibly) reused since binding.
//! Ported from `BindGroupState` / `GenRef` in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::BindGroupDesc;

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
