#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;

    use smithay::backend::allocator::dmabuf::DmabufFlags;

    use super::*;

    fn external_dmabuf(
        header: hl_surface_protocol::buffer::Header,
        width: u32,
        height: u32,
        fourcc: Fourcc,
        stride: u32,
        offset: u32,
        file_len: u64,
    ) -> Dmabuf {
        external_dmabuf_with_file(header, width, height, fourcc, stride, offset, file_len).0
    }

    fn external_dmabuf_with_file(
        header: hl_surface_protocol::buffer::Header,
        width: u32,
        height: u32,
        fourcc: Fourcc,
        stride: u32,
        offset: u32,
        file_len: u64,
    ) -> (Dmabuf, std::fs::File) {
        let path = std::env::temp_dir().join(format!(
            "hl-compositor-external-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        file.set_len(file_len).unwrap();
        file.write_all_at(&header.encode().unwrap(), header.header_offset().unwrap())
            .unwrap();
        let mut builder = Dmabuf::builder(
            (width as i32, height as i32),
            fourcc,
            Modifier::from(hl_surface_protocol::buffer::MODIFIER),
            DmabufFlags::empty(),
        );
        assert!(builder.add_plane(OwnedFd::from(file.try_clone().unwrap()), 0, offset, stride));
        (builder.build().unwrap(), file)
    }

    fn valid() -> (hl_surface_protocol::buffer::Header, u64) {
        let allocation = 64 * 4 + hl_surface_protocol::buffer::HEADER_LEN as u64;
        (
            hl_surface_protocol::buffer::Header::new(
                41,
                16,
                4,
                hl_surface_protocol::buffer::DRM_FMT_ARGB8888,
                64,
                allocation,
            )
            .unwrap()
            .with_serial(7)
            .unwrap(),
            allocation,
        )
    }

    #[test]
    fn private_modifier_is_advertised_only_for_native_sessions() {
        assert_eq!(dmabuf_formats(false).len(), 2);
        let native = dmabuf_formats(true);
        assert_eq!(native.len(), 4);
        assert!(native[2..].iter().all(|format| {
            format.modifier == Modifier::from(hl_surface_protocol::buffer::MODIFIER)
        }));
    }

    #[test]
    fn external_header_matches_every_dmabuf_field() {
        let (header, allocation) = valid();
        let dmabuf = external_dmabuf(
            header,
            16,
            4,
            Fourcc::Argb8888,
            64,
            hl_surface_protocol::buffer::PLANE_OFFSET as u32,
            allocation,
        );
        assert_eq!(
            ExternalBuffer::published(&dmabuf),
            Some(ExternalBuffer {
                token: 41,
                serial: 7,
                width: 16,
                height: 4,
                stride: 64,
                format: Format::Argb8888,
            })
        );
    }

    #[test]
    fn external_pixels_begin_at_zero_and_survive_descriptor_lifetime() {
        let (header, allocation) = valid();
        let (dmabuf, file) =
            external_dmabuf_with_file(header, 16, 4, Fourcc::Argb8888, 64, 0, allocation);
        let mut plane = vec![0xa5; 64 * 4];
        plane[0..8].copy_from_slice(&[3, 5, 7, 11, 13, 17, 19, 23]);
        plane[64..72].copy_from_slice(&[29, 31, 37, 41, 43, 47, 53, 59]);
        file.write_all_at(&plane, 0).unwrap();

        assert!(ExternalBuffer::published(&dmabuf).is_some());
        drop(dmabuf);

        let mut actual = vec![0; plane.len()];
        file.read_exact_at(&mut actual, 0).unwrap();
        assert_eq!(actual, plane);
        let mut trailer = [0; hl_surface_protocol::buffer::HEADER_LEN];
        file.read_exact_at(&mut trailer, plane.len() as u64)
            .unwrap();
        assert_eq!(
            hl_surface_protocol::buffer::Header::decode(&trailer).unwrap(),
            header
        );
    }

    #[test]
    fn external_header_accepts_zero_serial_but_rejects_wrong_layout() {
        let (header, allocation) = valid();
        let mismatches = [
            (15, 4, Fourcc::Argb8888, 64, 0, allocation),
            (16, 5, Fourcc::Argb8888, 64, 0, allocation),
            (16, 4, Fourcc::Xrgb8888, 64, 0, allocation),
            (16, 4, Fourcc::Argb8888, 68, 0, allocation),
            (16, 4, Fourcc::Argb8888, 64, 1, allocation),
            (16, 4, Fourcc::Argb8888, 64, 0, allocation + 1),
        ];
        for (width, height, fourcc, stride, offset, file_len) in mismatches {
            let dmabuf = external_dmabuf(header, width, height, fourcc, stride, offset, file_len);
            assert_eq!(ExternalBuffer::published(&dmabuf), None);
        }

        let zero_serial = hl_surface_protocol::buffer::Header::new(
            41,
            16,
            4,
            hl_surface_protocol::buffer::DRM_FMT_ARGB8888,
            64,
            allocation,
        )
        .unwrap();
        let dmabuf = external_dmabuf(zero_serial, 16, 4, Fourcc::Argb8888, 64, 0, allocation);
        assert_eq!(ExternalBuffer::token(&dmabuf), Some(41));
        assert_eq!(
            ExternalBuffer::published(&dmabuf),
            Some(ExternalBuffer {
                token: 41,
                serial: 0,
                width: 16,
                height: 4,
                stride: 64,
                format: Format::Argb8888,
            })
        );
    }
}
