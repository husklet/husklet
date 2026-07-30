use super::*;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;

fn image_file() -> (std::fs::File, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "hl-egl-image-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    (file, path)
}

#[test]
fn imported_xrgb_image_preserves_bgra_and_canonicalizes_alpha() {
    let (mut file, path) = image_file();
    file.write_all(&[3, 2, 1, 0, 6, 5, 4, 0]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let attributes = [
        EGL_WIDTH,
        2,
        EGL_HEIGHT,
        1,
        EGL_LINUX_DRM_FOURCC_EXT,
        DRM_FORMAT_XRGB8888 as isize,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        file.as_raw_fd() as isize,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        8,
        EGL_NONE,
    ];
    let mut images = Images::default();
    let token = images
        .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), false)
        .unwrap();
    drop(file);

    let sibling = images.get(token).unwrap();
    assert_eq!(
        sibling.native_bgra().unwrap(),
        [3, 2, 1, 0xff, 6, 5, 4, 0xff]
    );
    assert!(images.remove(token));
    assert!(images.get(token).is_none());
    sibling
        .write_native_bgra(&[9, 8, 7, 0xff, 12, 11, 10, 0xff])
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [9, 8, 7, 0, 12, 11, 10, 0]);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn imported_argb_image_preserves_native_bgra_and_alpha() {
    let (mut file, path) = image_file();
    file.write_all(&[0xaa, 0xbb, 0xcc, 0x44]).unwrap();
    let image = Image {
        fd: file.try_clone().unwrap().into(),
        width: 1,
        height: 1,
        fourcc: DRM_FORMAT_ARGB8888,
        offset: 0,
        stride: 4,
        modifier: DRM_FORMAT_MOD_LINEAR,
        external: None,
    };

    assert_eq!(image.native_bgra().unwrap(), [0xaa, 0xbb, 0xcc, 0x44]);
    image.write_native_bgra(&[0x11, 0x22, 0x33, 0x55]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), [0x11, 0x22, 0x33, 0x55]);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn native_bgra_respects_plane_offset_and_stride() {
    let (mut file, path) = image_file();
    file.write_all(&[
        0xee, 0xee, 0xee, 0xee, // plane offset
        1, 2, 3, 4, 0xdd, 0xdd, 0xdd, 0xdd, // row 0 + padding
        5, 6, 7, 8, 0xcc, 0xcc, 0xcc, 0xcc, // row 1 + padding
    ])
    .unwrap();
    let image = Image {
        fd: file.try_clone().unwrap().into(),
        width: 1,
        height: 2,
        fourcc: DRM_FORMAT_ARGB8888,
        offset: 4,
        stride: 8,
        modifier: DRM_FORMAT_MOD_LINEAR,
        external: None,
    };

    assert_eq!(image.native_bgra().unwrap(), [1, 2, 3, 4, 5, 6, 7, 8]);
    image
        .write_native_bgra(&[9, 10, 11, 12, 13, 14, 15, 16])
        .unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        [
            0xee, 0xee, 0xee, 0xee, 9, 10, 11, 12, 0xdd, 0xdd, 0xdd, 0xdd, 13, 14, 15, 16, 0xcc,
            0xcc, 0xcc, 0xcc,
        ]
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn padded_writeback_rejects_an_unbounded_staging_plane() {
    let (file, path) = image_file();
    let image = Image {
        fd: file.into(),
        width: 1,
        height: 2,
        fourcc: DRM_FORMAT_ARGB8888,
        offset: 0,
        stride: 256 << 20,
        modifier: DRM_FORMAT_MOD_LINEAR,
        external: None,
    };

    let error = image
        .write_native_bgra(&[0; 8])
        .expect_err("oversized padded plane must not allocate");
    assert!(
        error.to_string().contains("exceeds writeback limit"),
        "unexpected error: {error}"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn rejects_unknown_attributes_and_non_linear_modifiers() {
    let unknown = [0x7fff, 1, EGL_NONE];
    let mut images = Images::default();
    assert!(images
        .import(EGL_LINUX_DMA_BUF_EXT, unknown.as_ptr(), false)
        .is_none());
}

#[test]
fn khr_import_reads_32_bit_eglints_on_64_bit_hosts() {
    let (mut file, path) = image_file();
    file.write_all(&[3, 2, 1, 0]).unwrap();
    let attributes = [
        EGL_WIDTH as i32,
        1,
        EGL_HEIGHT as i32,
        1,
        EGL_LINUX_DRM_FOURCC_EXT as i32,
        DRM_FORMAT_ARGB8888 as i32,
        EGL_DMA_BUF_PLANE0_FD_EXT as i32,
        file.as_raw_fd(),
        EGL_DMA_BUF_PLANE0_OFFSET_EXT as i32,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT as i32,
        4,
        EGL_NONE as i32,
    ];
    let token =
        unsafe { Images::default().import_khr(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), false) };
    assert!(token.is_some());
    std::fs::remove_file(path).unwrap();
}

#[test]
fn external_import_requires_capability_and_publishes_only_the_header() {
    let (file, path) = image_file();
    let allocation = 64 + hl_surface_protocol::buffer::HEADER_LEN as u64;
    file.set_len(allocation).unwrap();
    let header = Header::new(41, 4, 2, DRM_FORMAT_ARGB8888, 32, allocation).unwrap();
    let header_offset = header.header_offset().unwrap();
    let mut plane = vec![0xa5; header_offset as usize];
    plane[0..8].copy_from_slice(&[3, 5, 7, 11, 13, 17, 19, 23]);
    plane[32..40].copy_from_slice(&[29, 31, 37, 41, 43, 47, 53, 59]);
    file.write_all_at(&plane, 0).unwrap();
    file.write_all_at(&header.encode().unwrap(), header_offset)
        .unwrap();
    let attributes = [
        EGL_WIDTH,
        4,
        EGL_HEIGHT,
        2,
        EGL_LINUX_DRM_FOURCC_EXT,
        DRM_FORMAT_ARGB8888 as isize,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        file.as_raw_fd() as isize,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        hl_surface_protocol::buffer::PLANE_OFFSET as isize,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        32,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
        MODIFIER as u32 as isize,
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
        (MODIFIER >> 32) as u32 as isize,
        EGL_NONE,
    ];

    assert!(
        Images::default()
            .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), false)
            .is_none(),
        "private modifier must remain hidden before IoSurface negotiation"
    );
    let mut images = Images::default();
    let token = images
        .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), true)
        .expect("negotiated external image");
    let sibling = images
        .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), true)
        .expect("the same dma-buf may have sibling EGLImage wrappers");
    let image = images.get(token).unwrap();
    assert_eq!(
        images.get(sibling).unwrap().external_token(),
        image.external_token()
    );
    assert_eq!(image.external_token().unwrap().get(), 41);
    assert_eq!(
        image.native_bgra().unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    let mut before = [0; hl_surface_protocol::buffer::HEADER_LEN];
    file.read_exact_at(&mut before, header_offset).unwrap();
    image.publish(7).unwrap();

    let mut bytes = [0; hl_surface_protocol::buffer::HEADER_LEN];
    file.read_exact_at(&mut bytes, header_offset).unwrap();
    assert_eq!(Header::decode(&bytes).unwrap().serial, 7);
    assert_eq!(&bytes[..24], &before[..24]);
    assert_eq!(&bytes[32..], &before[32..]);
    let mut actual_plane = vec![0; plane.len()];
    file.read_exact_at(&mut actual_plane, 0).unwrap();
    assert_eq!(
        actual_plane, plane,
        "publishing metadata must not overwrite offset-zero pixels or row padding"
    );
    assert_eq!(
        file.metadata().unwrap().len(),
        allocation,
        "publishing must not write a CPU dummy plane"
    );
    assert!(images.remove(token));
    assert_eq!(
        images
            .external_tokens
            .get(&41)
            .map(|(_, references)| *references),
        Some(1)
    );
    assert!(images.remove(sibling));
    assert!(!images.external_tokens.contains_key(&41));
    assert!(
        images
            .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), true)
            .is_some(),
        "destroying the prior EGLImage releases its token"
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn import_rejects_negative_fd_before_borrowing_it() {
    let attributes = [
        EGL_WIDTH,
        1,
        EGL_HEIGHT,
        1,
        EGL_LINUX_DRM_FOURCC_EXT,
        DRM_FORMAT_ARGB8888 as isize,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        -1,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        4,
        EGL_NONE,
    ];
    assert!(Images::default()
        .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), false)
        .is_none());
}

#[test]
fn external_import_rejects_noncanonical_allocation() {
    let (file, path) = image_file();
    let canonical = 64 + hl_surface_protocol::buffer::HEADER_LEN as u64;
    let oversized = canonical + 64;
    file.set_len(oversized).unwrap();
    let mut header = Header::new(51, 4, 2, DRM_FORMAT_ARGB8888, 32, canonical)
        .unwrap()
        .encode()
        .unwrap();
    header[56..64].copy_from_slice(&oversized.to_le_bytes());
    file.write_all_at(&header, 64).unwrap();
    let attributes = [
        EGL_WIDTH,
        4,
        EGL_HEIGHT,
        2,
        EGL_LINUX_DRM_FOURCC_EXT,
        DRM_FORMAT_ARGB8888 as isize,
        EGL_DMA_BUF_PLANE0_FD_EXT,
        file.as_raw_fd() as isize,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT,
        hl_surface_protocol::buffer::PLANE_OFFSET as isize,
        EGL_DMA_BUF_PLANE0_PITCH_EXT,
        32,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
        MODIFIER as u32 as isize,
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
        (MODIFIER >> 32) as u32 as isize,
        EGL_NONE,
    ];
    assert!(Images::default()
        .import(EGL_LINUX_DMA_BUF_EXT, attributes.as_ptr(), true)
        .is_none());
    std::fs::remove_file(path).unwrap();
}
