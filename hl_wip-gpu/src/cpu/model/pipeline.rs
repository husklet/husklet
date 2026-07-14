//! The CPU-native pipeline. A render pipeline remembers the state a draw is validated + rasterized
//! against (color formats, vertex layouts, topology, per-target blend); a compute pipeline remembers its
//! kernel shader so a `Dispatch` can run it. Ported from the `Pipeline` enum in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::{BlendState, VertexLayout};
use crate::protocol::model::enums::{TextureFormat, Topology};

pub enum Pipeline {
    Render {
        color_formats: Vec<TextureFormat>,
        vertex_layouts: Vec<VertexLayout>,
        /// Primitive assembly for a draw's vertex stream.
        topology: Topology,
        /// Per-color-target blend: `Some(_)` selects premultiplied linear-light source-over; `None` is an
        /// opaque replace. Aligned with `color_formats`.
        blends: Vec<Option<BlendState>>,
    },
    Compute {
        shader: u32,
    },
}
