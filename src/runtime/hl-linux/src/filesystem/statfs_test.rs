use hl_vfs::{FilesystemKind, FilesystemStats};

use crate::{GuestArchitecture, STATFS_SIZE, StatfsEncoder};

fn stats() -> FilesystemStats {
    FilesystemStats {
        kind: FilesystemKind::Overlay,
        block_size: 4096,
        blocks: 100,
        blocks_free: 40,
        blocks_available: 30,
        files: 20,
        files_free: 10,
        filesystem_id: [0x1122_3344, 0x5566_7788],
        name_maximum: 255,
        fragment_size: 4096,
        read_only: true,
        nosuid: true,
        nodev: false,
        noexec: true,
        relatime: true,
    }
}

#[test]
fn layouts_match_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut output = [0xff; STATFS_SIZE];
        StatfsEncoder::encode(architecture, stats(), &mut output).unwrap();
        assert_eq!(u64::from_le_bytes(output[0..8].try_into().unwrap()), 0x794c_7630);
        assert_eq!(u64::from_le_bytes(output[8..16].try_into().unwrap()), 4096);
        assert_eq!(u64::from_le_bytes(output[16..24].try_into().unwrap()), 100);
        assert_eq!(&output[56..64], &[0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]);
        assert_eq!(u64::from_le_bytes(output[80..88].try_into().unwrap()), 0x102b);
        assert!(output[88..].iter().all(|byte| *byte == 0));
    }
}

#[test]
fn invalid_geometry_closed() {
    let mut invalid = stats();
    invalid.blocks_free = invalid.blocks + 1;
    let mut output = [0; STATFS_SIZE];
    assert!(StatfsEncoder::encode(GuestArchitecture::Aarch64, invalid, &mut output).is_err());
    assert!(StatfsEncoder::encode(GuestArchitecture::X86_64, stats(), &mut output[..119]).is_err());
}
