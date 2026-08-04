use crate::{SYSTEM_INFO_SIZE, SystemInfo};

#[test]
fn layout_exact() {
    let encoded = SystemInfo {
        uptime_seconds: 1,
        loads: [2, 3, 4],
        total_ram: 5,
        free_ram: 6,
        shared_ram: 7,
        buffer_ram: 8,
        total_swap: 9,
        free_swap: 10,
        processes: 11,
        total_high: 12,
        free_high: 13,
    }
    .encode();
    assert_eq!(encoded.len(), SYSTEM_INFO_SIZE);
    for (offset, value) in [
        (0, 1_u64),
        (8, 2),
        (16, 3),
        (24, 4),
        (32, 5),
        (40, 6),
        (48, 7),
        (56, 8),
        (64, 9),
        (72, 10),
        (88, 12),
        (96, 13),
    ] {
        assert_eq!(
            u64::from_le_bytes(encoded[offset..offset + 8].try_into().unwrap()),
            value
        );
    }
    assert_eq!(u16::from_le_bytes(encoded[80..82].try_into().unwrap()), 11);
    assert_eq!(&encoded[82..88], &[0; 6]);
    assert_eq!(u32::from_le_bytes(encoded[104..108].try_into().unwrap()), 1);
    assert_eq!(&encoded[108..112], &[0; 4]);
}
