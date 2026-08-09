use hl_isa::GuestArchitecture;
use hl_vfs::{DeviceId, FileIdentity, FileKind, FileMetadata, FileTimestamp, Permissions};

use crate::{
    STATX_BASIC_STATS, STATX_BTIME, STATX_MNT_ID, STATX_SIZE, StatEncoder, StatEncodingError, StatxExtensions,
};

struct StatFixture;

impl StatFixture {
    fn timestamp(seconds: i64, nanoseconds: u32) -> FileTimestamp {
        FileTimestamp { seconds, nanoseconds }
    }

    fn metadata() -> FileMetadata {
        FileMetadata {
            identity: FileIdentity {
                device: DeviceId::new(259, 65_537).linux_encoded(),
                inode: 0x1122_3344_5566_7788,
            },
            kind: FileKind::Character,
            permissions: Permissions::from_bits(0o6754),
            links: 7,
            user: 0x1020_3040,
            group: 0x5060_7080,
            special_device: DeviceId::new(226, 128).linux_encoded(),
            size: 0x0102_0304_0506_0708,
            blocks_512: 0x1112_1314_1516_1718,
            block_size: 4096,
            accessed: Self::timestamp(-2, 123_456_789),
            modified: Self::timestamp(3, 987_654_321),
            changed: Self::timestamp(4, 1),
        }
    }

    fn u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }
}

#[test]
fn aarch64_stat_padding() {
    let metadata = StatFixture::metadata();
    let mut output = [0xa5; 160];
    assert_eq!(
        StatEncoder::encode_stat(GuestArchitecture::Aarch64, &metadata, &mut output),
        Ok(128)
    );
    assert_eq!(StatFixture::u64(&output, 0), metadata.identity.device);
    assert_eq!(StatFixture::u64(&output, 8), metadata.identity.inode);
    assert_eq!(StatFixture::u32(&output, 16), 0o020_000 | 0o6754);
    assert_eq!(StatFixture::u32(&output, 20), 7);
    assert_eq!(StatFixture::u64(&output, 32), metadata.special_device);
    assert_eq!(&output[40..48], &[0; 8]);
    assert_eq!(StatFixture::u64(&output, 48), metadata.size);
    assert_eq!(StatFixture::u32(&output, 56), 4096);
    assert_eq!(&output[60..64], &[0; 4]);
    assert_eq!(StatFixture::u64(&output, 72), (-2_i64) as u64);
    assert_eq!(StatFixture::u64(&output, 80), 123_456_789);
    assert_eq!(&output[120..128], &[0; 8]);
    assert_eq!(&output[128..], &[0xa5; 32]);
}

#[test]
fn x86_64_padding() {
    let metadata = StatFixture::metadata();
    let mut output = [0xcc; 144];
    assert_eq!(
        StatEncoder::encode_stat64(GuestArchitecture::X86_64, &metadata, &mut output),
        Ok(144)
    );
    assert_eq!(StatFixture::u64(&output, 16), metadata.links);
    assert_eq!(StatFixture::u32(&output, 24), 0o020_000 | 0o6754);
    assert_eq!(StatFixture::u32(&output, 28), metadata.user);
    assert_eq!(StatFixture::u32(&output, 32), metadata.group);
    assert_eq!(&output[36..40], &[0; 4]);
    assert_eq!(StatFixture::u64(&output, 40), metadata.special_device);
    assert_eq!(StatFixture::u64(&output, 56), 4096);
    assert_eq!(StatFixture::u64(&output, 104), 4);
    assert_eq!(StatFixture::u64(&output, 112), 1);
    assert_eq!(&output[120..144], &[0; 24]);
}

#[test]
fn statx_linux_split() {
    let metadata = StatFixture::metadata();
    let extensions = StatxExtensions {
        birth: Some(StatFixture::timestamp(9, 17)),
        mount_id: Some(0x3344_5566_7788_99aa),
    };
    let mut output = [0xff; STATX_SIZE];
    assert_eq!(
        StatEncoder::encode_statx(&metadata, extensions, &mut output),
        Ok(STATX_SIZE)
    );
    assert_eq!(
        StatFixture::u32(&output, 0),
        STATX_BASIC_STATS | STATX_BTIME | STATX_MNT_ID
    );
    assert_eq!(StatFixture::u32(&output, 4), 4096);
    assert_eq!(&output[8..16], &[0; 8]);
    assert_eq!(StatFixture::u32(&output, 16), 7);
    assert_eq!(StatFixture::u16(&output, 28), 0o020_000 | 0o6754);
    assert_eq!(StatFixture::u64(&output, 32), metadata.identity.inode);
    assert_eq!(StatFixture::u64(&output, 80), 9);
    assert_eq!(StatFixture::u32(&output, 88), 17);
    assert_eq!(StatFixture::u32(&output, 128), 226);
    assert_eq!(StatFixture::u32(&output, 132), 128);
    assert_eq!(StatFixture::u32(&output, 136), 259);
    assert_eq!(StatFixture::u32(&output, 140), 65_537);
    assert_eq!(StatFixture::u64(&output, 144), 0x3344_5566_7788_99aa);
    assert!(output[152..].iter().all(|byte| *byte == 0));
}

#[test]
fn statx_omits_bits() {
    let mut output = [0xff; STATX_SIZE];
    StatEncoder::encode_statx(&StatFixture::metadata(), StatxExtensions::default(), &mut output).unwrap();
    assert_eq!(StatFixture::u32(&output, 0), STATX_BASIC_STATS);
    assert_eq!(&output[80..96], &[0; 16]);
    assert_eq!(&output[144..152], &[0; 8]);
}

#[test]
fn short_output_writes() {
    let mut metadata = StatFixture::metadata();
    metadata.size = u64::MAX;
    metadata.accessed.nanoseconds = 1_000_000_000;
    let mut output = [0x5a; 127];
    assert_eq!(
        StatEncoder::encode_stat(GuestArchitecture::Aarch64, &metadata, &mut output),
        Err(StatEncodingError::OutputTooSmall)
    );
    assert!(output.iter().all(|byte| *byte == 0x5a));
}

#[test]
fn validation_precedes_boundaries() {
    let mut metadata = StatFixture::metadata();
    let mut output = [0x7b; 256];
    metadata.accessed.nanoseconds = 1_000_000_000;
    assert_eq!(
        StatEncoder::encode_stat(GuestArchitecture::Aarch64, &metadata, &mut output),
        Err(StatEncodingError::InvalidTimestamp)
    );
    assert!(output.iter().all(|byte| *byte == 0x7b));

    metadata.accessed.nanoseconds = 999_999_999;
    metadata.size = i64::MAX as u64 + 1;
    assert_eq!(
        StatEncoder::encode_statx(&metadata, StatxExtensions::default(), &mut output),
        Err(StatEncodingError::Overflow)
    );
    assert!(output.iter().all(|byte| *byte == 0x7b));
}

#[test]
fn aarch64_link_links() {
    let mut metadata = StatFixture::metadata();
    metadata.links = u64::from(u32::MAX) + 1;
    let mut output = [0; 144];
    assert_eq!(
        StatEncoder::encode_stat(GuestArchitecture::Aarch64, &metadata, &mut output),
        Err(StatEncodingError::Overflow)
    );
    assert_eq!(
        StatEncoder::encode_stat(GuestArchitecture::X86_64, &metadata, &mut output),
        Ok(144)
    );
    assert_eq!(StatFixture::u64(&output, 16), metadata.links);
}

#[test]
fn anonymous_link_count() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut metadata = StatFixture::metadata();
        metadata.links = 0;
        let mut output = vec![0_u8; architecture.linux_stat_size()];
        StatEncoder::encode_stat(architecture, &metadata, &mut output).unwrap();
        let links = if architecture == GuestArchitecture::Aarch64 {
            u64::from(StatFixture::u32(&output, 20))
        } else {
            StatFixture::u64(&output, 16)
        };
        assert_eq!(links, 0);
    }
}

/// `stx_attributes_mask` names the attribute bits the kernel can report at all;
/// a zero there tells a caller that no attribute is knowable. Measured on Linux
/// 7.0.11: `0x203000` on procfs, sysfs and devpts, widened only by the on-disk
/// filesystems. `stx_blksize` follows the object, not a constant: 1024 on procfs.
#[test]
fn statx_reports_attributes_mask_and_object_block_size() {
    let mut metadata = StatFixture::metadata();
    metadata.block_size = 1024;
    let mut output = [0xff; STATX_SIZE];
    StatEncoder::encode_statx(&metadata, StatxExtensions::default(), &mut output).unwrap();
    assert_eq!(StatFixture::u64(&output, 56), 0x0020_3000);
    assert_eq!(StatFixture::u32(&output, 4), 1024);
    assert_eq!(StatFixture::u64(&output, 8), 0);

    let mut stat = [0xff; 160];
    StatEncoder::encode_stat(GuestArchitecture::Aarch64, &metadata, &mut stat).unwrap();
    assert_eq!(StatFixture::u32(&stat, 56), 1024);
    let mut stat = [0xff; 160];
    StatEncoder::encode_stat(GuestArchitecture::X86_64, &metadata, &mut stat).unwrap();
    assert_eq!(StatFixture::u64(&stat, 56), 1024);
}
