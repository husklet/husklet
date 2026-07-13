//! Behavioral cross-backend and executor-transport tests.
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

#[test]
fn executor_transport_rejects_a_failed_frame_acknowledgement() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let path = std::env::temp_dir().join(format!("dd-render-ack-{}-{}.sock", std::process::id(),
        std::thread::current().name().unwrap_or("test")));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind fake executor");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept shim transport");
        let mut header = [0u8; 16];
        stream.read_exact(&mut header).expect("read frame header");
        let len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).expect("read frame payload");
        stream.write_all(&[0]).expect("send executor failure ack");
    });

    let mut connection = dd_shim_common::transport::ExecConn::new(path.to_string_lossy().into_owned());
    let surface = dd_shim_common::transport::Surface { id: 7, width: 16, height: 9, stride: 64, fd: -1 };
    let result = connection.submit(&surface, &[1, 2, 3, 4]);
    server.join().unwrap();
    let _ = std::fs::remove_file(path);
    assert!(result.is_err(), "ExecConn treated executor failure ack=0 as success");
}
