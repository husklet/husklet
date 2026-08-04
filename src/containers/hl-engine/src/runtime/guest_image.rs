//! Behavior-free guest images used only to stage runtime composition tests.

const AARCH64_MACHINE: u16 = 183;
const LINK_BASE: u64 = 0x40_0000;
const ENTRY_OFFSET: usize = 0x180;

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

pub(super) fn aarch64_exit_image() -> Vec<u8> {
    let mut bytes = vec![0_u8; 4096];
    bytes[..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    put_u16(&mut bytes, 16, 2);
    put_u16(&mut bytes, 18, AARCH64_MACHINE);
    put_u32(&mut bytes, 20, 1);
    put_u64(&mut bytes, 24, LINK_BASE + ENTRY_OFFSET as u64);
    put_u64(&mut bytes, 32, 64);
    put_u16(&mut bytes, 52, 64);
    put_u16(&mut bytes, 54, 56);
    put_u16(&mut bytes, 56, 1);
    put_u32(&mut bytes, 64, 1);
    put_u32(&mut bytes, 68, 5);
    put_u64(&mut bytes, 80, LINK_BASE);
    put_u64(&mut bytes, 88, LINK_BASE);
    let image_length = bytes.len() as u64;
    put_u64(&mut bytes, 96, image_length);
    put_u64(&mut bytes, 104, image_length);
    put_u64(&mut bytes, 112, 4096);

    for (index, instruction) in [0xd280_0ba8_u32, 0xd280_0000_u32, 0xd400_0001_u32]
        .into_iter()
        .enumerate()
    {
        let offset = ENTRY_OFFSET + index * 4;
        bytes[offset..offset + 4].copy_from_slice(&instruction.to_le_bytes());
    }
    bytes
}
