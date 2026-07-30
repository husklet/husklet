//! The CPU-native texture: its descriptor + tight-packed level-0 pixels (`bytes_per_texel * w * h *
//! sample_count`). Stored behind a `TextureId`. Ported from the `Texture` struct in
//! `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::TextureDesc;

#[derive(Clone)]
pub struct Texture {
    pub desc: TextureDesc,
    /// Tight-packed level-0 pixels (bytes_per_texel * w * h [* sample_count]).
    pub pixels: Vec<u8>,
}
