//! Present. Mirrors the CPU oracle's `present`: validate that the presented texture matches the surface's
//! size and is single-sampled, then return the protocol-id pairing. The out-of-band presentable-image
//! handoff (IOSurface/dma-buf) is a compositor concern delivered on a separate channel; the executor
//! surfaces only the [`Presentation`] pairing the runtime records.

use hl_gpu::protocol::model::descriptor::SurfaceDesc;
use hl_gpu::protocol::model::id::{SurfaceId, TextureId};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Presentation, Result};

use crate::texture;

pub fn present(res: &SessionResources, surface_id: u32, texture_id: u32) -> Result<Presentation> {
    let sdesc = res
        .surfaces
        .get(surface_id)?
        .downcast_ref::<SurfaceDesc>()
        .ok_or(GpuError::Invalid("wgpu: surface native type mismatch"))?;
    let t = texture::WgpuTexture::get(res, texture_id)?;
    if t.width != sdesc.width || t.height != sdesc.height {
        return Err(GpuError::Invalid(
            "present texture size does not match surface",
        ));
    }
    Ok(Presentation {
        surface: SurfaceId(surface_id),
        texture: TextureId(texture_id),
    })
}
