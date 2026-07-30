use super::*;
use crate::image::{Image, DRM_FORMAT_ARGB8888};
use crate::state::ImportedImage;
use hl_gpu::protocol::model::enums::TextureFormat;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

#[test]
fn compact_capture_removes_padded_pitch_for_non_aligned_width() {
    let capture = hl_gl::service::frame::FrameCapture {
        target: hl_gl::service::frame::FrameTarget {
            name: 1,
            generation: 1,
            shared_storage: None,
            shared_revision: None,
            surface: 1,
            texture: 2,
            width: 3,
            height: 2,
            format: TextureFormat::Bgra8Unorm,
            token: None,
        },
        buffer: 3,
        offset: 256,
        bytes_per_row: 256,
        len: 24,
    };
    let mut readback = vec![0xee; 256 + 512];
    readback[256..268].copy_from_slice(&(0..12).collect::<Vec<_>>());
    readback[512..524].copy_from_slice(&(12..24).collect::<Vec<_>>());

    assert_eq!(
        compact_capture(&readback, &capture).unwrap(),
        (0..24).collect::<Vec<_>>()
    );
}

#[test]
fn accepted_imported_write_keeps_gpu_authority_after_readback_failure() {
    let mut context = hl_gl::model::context::GlContext::new();
    let texture = context.textures.gen();
    hl_gl::service::record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    hl_gl::service::record::tex_image_2d(&mut context, 2, 2, &[7; 16]);
    let shared = Arc::new(hl_gl::model::texture::SharedPixels::new(Arc::new(vec![
        7;
        16
    ])));
    assert!(context.textures.bind_shared(texture, shared.clone()));
    let generation = context.textures.get(texture).unwrap().gen;
    context.accept_targets(&[hl_gl::service::frame::FrameTarget {
        name: texture,
        generation,
        shared_storage: Some(shared.id()),
        shared_revision: Some(shared.version()),
        surface: 9,
        texture: 10,
        width: 2,
        height: 2,
        format: TextureFormat::R8Unorm,
        token: None,
    }]);
    context.reset_frame();

    let readback = Err::<(), _>(hl_gpu::GpuError::Decode("forced readback failure".into()));
    assert!(readback.is_err());
    assert!(context.textures.get(texture).unwrap().gpu_authoritative());
}

#[test]
fn external_identity_is_visible_while_submit_blocks_and_remains_exact_on_failure() {
    let path = std::env::temp_dir().join(format!(
        "hl-external-submit-race-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let allocation = 64 + hl_surface_protocol::buffer::HEADER_LEN as u64;
    file.set_len(allocation).unwrap();
    let header =
        hl_surface_protocol::buffer::Header::new(41, 4, 2, DRM_FORMAT_ARGB8888, 32, allocation)
            .unwrap();
    let header_offset = header.header_offset().unwrap();
    file.write_all_at(&header.encode().unwrap(), header_offset)
        .unwrap();
    let image = Arc::new(Image {
        fd: file.try_clone().unwrap().into(),
        width: 4,
        height: 2,
        fourcc: DRM_FORMAT_ARGB8888,
        offset: hl_surface_protocol::buffer::PLANE_OFFSET,
        stride: 32,
        modifier: hl_surface_protocol::buffer::MODIFIER,
        external: Some(header),
    });
    let bindings = std::collections::HashMap::from([(
        7,
        ImportedImage {
            generation: 3,
            image,
            shared: None,
        },
    )]);
    let token = hl_gpu::protocol::model::descriptor::SurfaceToken::new(41).unwrap();
    let serial = hl_gpu::protocol::model::descriptor::FrameSerial::new(19).unwrap();
    let publications = vec![(
        hl_gl::service::frame::FrameTarget {
            name: 7,
            generation: 3,
            shared_storage: None,
            shared_revision: None,
            surface: 70,
            texture: 71,
            width: 4,
            height: 2,
            format: TextureFormat::Bgra8Unorm,
            token: Some(token),
        },
        serial,
    )];
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
    let worker = std::thread::spawn(move || {
        submit_external(&bindings, &publications, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Err(hl_gpu::GpuError::Invalid("forced submit failure"))
        })
    });

    entered_rx.recv().unwrap();
    let mut bytes = [0; hl_surface_protocol::buffer::HEADER_LEN];
    file.read_exact_at(&mut bytes, header_offset).unwrap();
    assert_eq!(
        hl_surface_protocol::buffer::Header::decode(&bytes)
            .unwrap()
            .serial,
        19,
        "the Wayland commit must observe the reserved serial before GPU completion"
    );
    release_tx.send(()).unwrap();
    assert!(matches!(
        worker.join().unwrap(),
        Err(hl_gpu::GpuError::Invalid("forced submit failure"))
    ));
    file.read_exact_at(&mut bytes, header_offset).unwrap();
    assert_eq!(
        hl_surface_protocol::buffer::Header::decode(&bytes)
            .unwrap()
            .serial,
        19,
        "the host needs the same exact pair to terminally cancel a racing commit"
    );
    std::fs::remove_file(path).unwrap();
}
