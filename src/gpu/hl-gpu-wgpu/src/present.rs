//! Present. Mirrors the CPU oracle's `present`: validate that the presented texture matches the surface's
//! size and is single-sampled, then return the protocol-id pairing. The out-of-band presentable-image
//! handoff (IOSurface/dma-buf) is a compositor concern delivered on a separate channel; the executor
//! surfaces only the [`Presentation`] pairing the runtime records.

use hl_gpu::protocol::model::descriptor::SurfaceDesc;
use hl_gpu::protocol::model::id::{SurfaceId, TextureId};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Presentation, Result};

use crate::texture;

pub fn present(
    executor: &mut crate::WgpuExecutor,
    res: &SessionResources,
    surface_id: u32,
    texture_id: u32,
    serial: hl_gpu::FrameSerial,
) -> Result<Presentation> {
    #[cfg(not(target_os = "macos"))]
    let _ = executor;
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
    #[cfg(target_os = "macos")]
    if t.iosurface.is_some() {
        // Publish an explicit completion probe with the retained IOSurface instead of device-wide waiting
        // here. The compositor polls this probe before importing the frame, preserving cross-queue ownership
        // while allowing the producer and GPU to continue asynchronously.
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let callback_ready = std::sync::Arc::clone(&ready);
        executor.gpu.queue.on_submitted_work_done(move || {
            callback_ready.store(true, std::sync::atomic::Ordering::Release);
        });
        let key = (sdesc.token.get(), serial.get());
        let mut completions = executor
            .presentation_completions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Results are consumed before the next batch. An older unclaimed result for this surface token was
        // abandoned; keeping it would leak one callback record per canceled frame.
        completions.retain(|(token, previous), _| {
            *token != sdesc.token.get() || *previous >= serial.get()
        });
        completions.insert(
            key,
            crate::IoSurfaceCompletion {
                gpu: std::sync::Arc::clone(&executor.gpu),
                ready,
            },
        );
        drop(completions);
        executor.presentation_journal.push(key);
    }
    Ok(Presentation {
        surface: SurfaceId(surface_id),
        token: sdesc.token,
        texture: TextureId(texture_id),
        serial,
    })
}
