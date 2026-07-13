//! Software-backend capability and present behavior.
//!
//! Do not inspect implementation source here. A rendering test must execute an API, transport,
//! state transition, or pixel path and assert its observable result.

use dd_gpu::backend::{GpuBackend, PresentKind};
use dd_gpu::id::{SurfaceId, TextureId};
use dd_gpu::ir::*;
use dd_gpu::software::SoftwareBackend;

#[test]
fn software_backend_graphics_and_present_claims_have_observable_behavior() {
    let mut backend = SoftwareBackend::new();
    let caps = backend.capabilities();
    assert!(caps.supports_graphics);
    assert!(caps.present_kinds.contains(&PresentKind::Shm));
    assert!(caps.max_texture_2d >= 4);

    let texture = TextureDesc {
        width: 4, height: 3, depth: 1, mip_levels: 1, sample_count: 1, dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC | texture_usage::PRESENT,
        label: "capability-proof".into(),
    };
    backend.create_texture(TextureId(1), &texture).unwrap();
    backend.create_surface(SurfaceId(2), &SurfaceDesc {
        width: 4, height: 3, format: TextureFormat::Rgba8Unorm, ddp_surface: 99,
    }).unwrap();
    backend.submit(&CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass { color: vec![ColorAttachment {
                texture: 1, load: LoadOp::Clear, clear: [0.25, 0.5, 0.75, 1.0], store: true,
            }], depth: None },
            Enc::EndRenderPass,
        ],
        signal: None,
    }).unwrap();
    let mut pixels = vec![0u8; 4 * 3 * 4];
    backend.read_texture(TextureId(1), &mut pixels).unwrap();
    for pixel in pixels.chunks_exact(4) {
        assert_eq!(pixel, [64, 128, 191, 255]);
    }
    let token = backend.present(SurfaceId(2), TextureId(1)).unwrap();
    assert_eq!(token.kind, PresentKind::Shm);
    assert_eq!((token.surface, token.width, token.height), (2, 4, 3));
    assert!(token.format_ok);
}
