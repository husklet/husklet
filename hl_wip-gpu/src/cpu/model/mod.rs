//! The CPU executor's native storage objects (stored type-erased behind protocol ids in the runtime-owned
//! [`SessionResources`]) + the typed downcast accessors that fetch them back out.
//!
//! Singular ownership (§2 of the v2 overview): the runtime's [`SessionResources`] is the *only* id→object
//! map; this executor inserts its native (`Box<dyn Any>`) behind each created id and downcasts it on use.
//! Lifecycle (duplicate-create / use-after-free / double-free) is enforced once by the [`ResourceTable`];
//! the concrete native type stays private to this executor.
//!
//! [`SessionResources`]: crate::runtime::model::resources::SessionResources
//! [`ResourceTable`]: crate::protocol::model::id::ResourceTable

pub(crate) mod bindgroup;
pub(crate) mod buffer;
pub(crate) mod pipeline;
pub(crate) mod sampler;
pub(crate) mod shader;
pub(crate) mod texture;

pub use bindgroup::{BindGroupState, GenRef};
pub use buffer::Buffer;
pub use pipeline::Pipeline;
pub use sampler::Sampler;
pub use shader::ShaderModule;
pub use texture::Texture;

use crate::protocol::model::descriptor::SurfaceDesc;
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::SessionResources;

// Downcast accessors: turn a type-erased `Box<dyn Any>` native back into its concrete type. A downcast
// mismatch is impossible if the executor is the sole writer (it always inserts the matching type), but a
// failed downcast still surfaces as a typed `UnknownId` rather than a panic.

macro_rules! accessor {
    ($get:ident, $get_mut:ident, $table:ident, $ty:ty, $kind:literal) => {
        #[allow(dead_code)]
        pub(crate) fn $get(res: &SessionResources, id: u32) -> Result<&$ty> {
            res.$table
                .get(id)?
                .downcast_ref::<$ty>()
                .ok_or(GpuError::UnknownId { kind: $kind, id })
        }
        #[allow(dead_code)]
        pub(crate) fn $get_mut(res: &mut SessionResources, id: u32) -> Result<&mut $ty> {
            res.$table
                .get_mut(id)?
                .downcast_mut::<$ty>()
                .ok_or(GpuError::UnknownId { kind: $kind, id })
        }
    };
}

accessor!(buffer, buffer_mut, buffers, Buffer, "buffer");
accessor!(texture, texture_mut, textures, Texture, "texture");
accessor!(pipeline, pipeline_mut, pipelines, Pipeline, "pipeline");
accessor!(shader, shader_mut, shaders, ShaderModule, "shader");
accessor!(bind_group, bind_group_mut, bind_groups, BindGroupState, "bind_group");
accessor!(surface, surface_mut, surfaces, SurfaceDesc, "surface");
accessor!(fence, fence_mut, fences, u64, "fence");
