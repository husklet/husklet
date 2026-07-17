//! The CPU-native pipeline. A render pipeline remembers the state a draw is validated + rasterized
//! against (color formats, vertex layouts, topology, per-target blend); a compute pipeline remembers its
//! kernel shader so a `Dispatch` can run it. Ported from the `Pipeline` enum in `hl-gpu/src/software.rs`.

use crate::protocol::model::descriptor::{BlendState, DepthState, VertexLayout};
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
        /// Per-color-target RGBA write mask (`ColorTargetState::write_mask`, low 4 bits
        /// `R<<0|G<<1|B<<2|A<<3`). Aligned with `color_formats`. Only the channels whose bit is set are
        /// written; the rest keep their prior value. `0xF` (the neutral default) writes all channels —
        /// byte-for-byte the pre-mask replace/blend behavior.
        write_masks: Vec<u32>,
        /// Face culling (`RenderPipelineDesc::cull`: 0 = none, 1 = front, 2 = back). `0` (neutral) draws
        /// every triangle regardless of winding.
        cull: u32,
        /// Winding that defines a front face (`RenderPipelineDesc::front_face`: 0 = CCW, 1 = CW). Only
        /// changes an observable result together with a non-zero `cull`.
        front_face: u32,
        /// Depth test/write state, if the pipeline declares a depth-stencil attachment. `Some(_)` makes a
        /// draw run the per-fragment depth test against the render pass's depth buffer.
        depth: Option<DepthState>,
    },
    Compute {
        shader: u32,
    },
}
