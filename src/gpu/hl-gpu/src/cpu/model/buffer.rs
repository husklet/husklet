//! The CPU-native buffer: plain host memory + its usage bits. Stored behind a `BufferId` in the
//! runtime-owned [`SessionResources`](crate::runtime::model::resources::SessionResources). Ported from the
//! `Buffer` struct in `hl-gpu/src/software.rs`.

pub struct Buffer {
    pub data: Vec<u8>,
    pub usage: u32,
}
