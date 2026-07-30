use std::collections::HashMap;

use crate::model::es3::{ProgramPipelines, Queries, TransformFeedbacks};
use crate::model::framebuffer::Framebuffers;
use crate::model::program::{Attr, MAX_ATTR};

use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SurfaceTarget {
    pub(crate) texture: u32,
    pub(crate) surface: u32,
    pub(crate) token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
    pub(crate) size: (i32, i32),
}

impl SurfaceTarget {
    pub fn is_empty(self) -> bool {
        self.texture == 0
    }

    /// Consume the executor resources owned by an unbound EGL surface.
    pub fn retire(self) -> Vec<hl_gpu::Cmd> {
        if self.texture == 0 {
            return Vec::new();
        }
        let mut commands = vec![hl_gpu::Cmd::DestroyTexture(self.texture)];
        if self.token.is_some() {
            commands.push(hl_gpu::Cmd::DestroySurface(self.surface));
        }
        commands
    }
}

/// State owned by one GL context rather than its share group.
///
/// Object tables, IR allocators, residency, programs, buffers, textures, renderbuffers, and sampler
/// objects remain on [`GlContext`]. Sampler bindings are currently exchanged through `Samplers` because
/// that model intentionally stores shared descriptors and local bindings together.
pub(crate) struct LocalState {
    pub(crate) client_major: i32,
    pub(crate) client_minor: i32,
    pub(crate) no_error: bool,
    pub(crate) surf: GlSurface,
    pub(crate) surface_kind: SurfaceKind,
    pub(crate) draw_surface_id: u64,
    pub(crate) read_surf: GlSurface,
    pub(crate) read_surface_kind: SurfaceKind,
    pub(crate) read_surface_id: u64,
    pub(crate) framebuffers: Framebuffers,
    pub(crate) queries: Queries,
    pub(crate) transform_feedbacks: TransformFeedbacks,
    pub(crate) program_pipelines: ProgramPipelines,
    pub(crate) cur_prog: u32,
    pub(crate) array_buffer: u32,
    pub(crate) element_buffer: u32,
    pub(crate) general_buffers: HashMap<u32, u32>,
    pub(crate) active_texture: usize,
    pub(crate) tex_unit: [u32; 8],
    pub(crate) attr: [Attr; MAX_ATTR],
    pub(crate) current_attr: [[f32; 4]; MAX_ATTR],
    pub(crate) current_attr_kind: [u8; MAX_ATTR],
    pub(crate) vertex_bindings: [VertexBinding; MAX_ATTR],
    pub(crate) pipeline: PipelineState,
    pub(crate) bound_fbo: u32,
    pub(crate) read_fbo: u32,
    pub(crate) bound_rbo: u32,
    pub(crate) cur_vao: u32,
    pub(crate) vaos: HashMap<u32, Vao>,
    pub(crate) next_vao: u32,
    pub(crate) pixel_store: PixelStore,
    pub(crate) indexed_buffers: HashMap<(u32, u32), IndexedBinding>,
    pub(crate) draw_buffers: Vec<u32>,
    pub(crate) read_buffer_src: u32,
    pub(crate) gl_error: u32,
    pub(crate) recording: Recording,
    pub(crate) default_targets: HashMap<u64, SurfaceTarget>,
    pub(crate) present_token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
    pub(crate) present_serial: Option<hl_gpu::protocol::model::descriptor::FrameSerial>,
}

impl LocalState {
    pub fn with_version(major: i32, minor: i32, no_error: bool) -> Self {
        Self {
            client_major: major,
            client_minor: minor,
            no_error,
            ..Self::default()
        }
    }
}

impl Default for LocalState {
    fn default() -> Self {
        Self {
            client_major: 3,
            client_minor: 1,
            no_error: false,
            surf: GlSurface::default(),
            surface_kind: SurfaceKind::Window,
            draw_surface_id: 0,
            read_surf: GlSurface::default(),
            read_surface_kind: SurfaceKind::Window,
            read_surface_id: 0,
            framebuffers: Framebuffers::new(),
            queries: Queries::new(),
            transform_feedbacks: TransformFeedbacks::new(),
            program_pipelines: ProgramPipelines::new(),
            cur_prog: 0,
            array_buffer: 0,
            element_buffer: 0,
            general_buffers: HashMap::new(),
            active_texture: 0,
            tex_unit: [0; 8],
            attr: [Attr::default(); MAX_ATTR],
            current_attr: [[0.0, 0.0, 0.0, 1.0]; MAX_ATTR],
            current_attr_kind: [0; MAX_ATTR],
            vertex_bindings: [VertexBinding::default(); MAX_ATTR],
            pipeline: PipelineState::default(),
            bound_fbo: 0,
            read_fbo: 0,
            bound_rbo: 0,
            cur_vao: 0,
            vaos: HashMap::new(),
            next_vao: 1,
            pixel_store: PixelStore::default(),
            indexed_buffers: HashMap::new(),
            draw_buffers: vec![glconst::GL_BACK],
            read_buffer_src: glconst::GL_BACK,
            gl_error: glconst::GL_NO_ERROR,
            recording: Recording::default(),
            default_targets: HashMap::new(),
            present_token: None,
            present_serial: None,
        }
    }
}
