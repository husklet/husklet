use super::Wire as LaunchWire;
use super::*;

const POOL_SIZE: usize = 48;

fn put_u32(wire: &mut [u8], offset: usize, value: u32) {
    wire[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(wire: &mut [u8], offset: usize, value: u64) {
    wire[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn valid_wire() -> Vec<u8> {
    let mut wire = vec![0; HEADER_SIZE + POOL_SIZE];
    put_u32(&mut wire, 0, MAGIC);
    put_u32(&mut wire, 4, POOL_SIZE as u32);
    put_u32(&mut wire, 8, HEADER_SIZE as u32);
    put_u32(&mut wire, 12, ABI);
    put_u64(&mut wire, 152, 1);
    wire
}

#[test]
fn parses_byte_exact() {
    let mut wire = valid_wire();
    put_u64(&mut wire, 16, 0x0102_0304_0506_0708);
    put_u32(&mut wire, 24, 9);
    put_u32(&mut wire, 28, 10);
    put_u32(&mut wire, 32, (-2_i32) as u32);
    put_u32(&mut wire, 36, 1_000);
    put_u32(&mut wire, 168, 17);
    put_u32(&mut wire, 184, 18);

    let parsed = LaunchWire::parse(&wire).unwrap();
    assert_eq!(parsed.header().memory_limit, 0x0102_0304_0506_0708);
    assert_eq!(parsed.header().pid_limit, 9);
    assert_eq!(parsed.header().cpu_limit, 10);
    assert_eq!(parsed.header().uid, -2);
    assert_eq!(parsed.header().gid, 1_000);
    assert_eq!(parsed.header().executable_host_offset, 17);
    assert_eq!(parsed.header().name_binds_offset, 18);
    assert_eq!(parsed.pool(), &[0; POOL_SIZE]);
    assert_eq!(parsed.string(0), Ok(&[][..]));
}

#[test]
fn accepts_extended_header() {
    let mut wire = valid_wire();
    wire.splice(HEADER_SIZE..HEADER_SIZE, [0xaa; 8]);
    put_u32(&mut wire, 4, POOL_SIZE as u32);
    put_u32(&mut wire, 8, (HEADER_SIZE + 8) as u32);
    wire[HEADER_SIZE + 9..HEADER_SIZE + 15].copy_from_slice(b"guest\0");
    let parsed = LaunchWire::parse(&wire).unwrap();
    assert_eq!(parsed.string(1), Ok(&b"guest"[..]));
}

#[test]
fn validation_error_precedence() {
    assert_eq!(
        LaunchWire::parse(&vec![0; HEADER_SIZE - 1]).unwrap_err(),
        WireError::InvalidArgument
    );
    let mut wire = valid_wire();
    put_u32(&mut wire, 0, 0);
    put_u32(&mut wire, 12, 99);
    put_u64(&mut wire, 152, 0);
    assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::Corrupt);
    put_u32(&mut wire, 0, MAGIC);
    assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::AbiMismatch);
    put_u32(&mut wire, 12, ABI);
    put_u32(&mut wire, 148, 1);
    assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::Corrupt);
    put_u32(&mut wire, 148, 0);
    put_u32(&mut wire, 188, 1);
    assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::Corrupt);
    put_u32(&mut wire, 188, 0);
    assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::InvalidArgument);
}

#[test]
fn rejects_fixed_header() {
    for (offset, value) in [(8, (HEADER_SIZE - 1) as u32), (128, 4), (124, 4), (172, 3), (176, 9)] {
        let mut wire = valid_wire();
        put_u32(&mut wire, offset, value);
        assert_eq!(LaunchWire::parse(&wire).unwrap_err(), WireError::Corrupt);
    }

    let mut mismatch = valid_wire();
    put_u32(&mut mismatch, 48, 1);
    assert_eq!(LaunchWire::parse(&mismatch).unwrap_err(), WireError::Corrupt);
    put_u32(&mut mismatch, 172, 1);
    assert!(LaunchWire::parse(&mismatch).is_ok());
}

#[test]
fn rejects_size_and() {
    let mut oversized_header = valid_wire();
    let beyond_wire = (oversized_header.len() + 1) as u32;
    put_u32(&mut oversized_header, 8, beyond_wire);
    assert_eq!(LaunchWire::parse(&oversized_header).unwrap_err(), WireError::Corrupt);

    let mut wrong_total = valid_wire();
    put_u32(&mut wrong_total, 4, (POOL_SIZE + 1) as u32);
    assert_eq!(LaunchWire::parse(&wrong_total).unwrap_err(), WireError::Corrupt);

    let mut empty_pool = valid_wire();
    empty_pool.truncate(HEADER_SIZE);
    put_u32(&mut empty_pool, 4, 0);
    assert_eq!(LaunchWire::parse(&empty_pool).unwrap_err(), WireError::Corrupt);

    let mut missing_sentinel = valid_wire();
    missing_sentinel[HEADER_SIZE] = b'x';
    assert_eq!(LaunchWire::parse(&missing_sentinel).unwrap_err(), WireError::Corrupt);
}

#[test]
fn lower_records_are() {
    let mut wire = valid_wire();
    put_u32(&mut wire, 60, 1);
    put_u32(&mut wire, 176, 2);
    put_u32(&mut wire, 180, 30);
    wire[HEADER_SIZE + 1..HEADER_SIZE + 9].copy_from_slice(b"/a\0/bbb\0");
    assert!(LaunchWire::parse(&wire).is_ok());

    for mutation in [(60, 0), (60, POOL_SIZE as u32), (176, 0), (180, 0)] {
        let mut malformed = wire.clone();
        put_u32(&mut malformed, mutation.0, mutation.1);
        assert_eq!(LaunchWire::parse(&malformed).unwrap_err(), WireError::Corrupt);
    }

    let mut relative = wire.clone();
    relative[HEADER_SIZE + 1] = b'a';
    assert_eq!(LaunchWire::parse(&relative).unwrap_err(), WireError::Corrupt);
    let mut empty = wire.clone();
    empty[HEADER_SIZE + 1] = 0;
    assert_eq!(LaunchWire::parse(&empty).unwrap_err(), WireError::Corrupt);
    let mut unterminated = wire;
    unterminated[HEADER_SIZE + 1..].fill(b'/');
    assert_eq!(LaunchWire::parse(&unterminated).unwrap_err(), WireError::Corrupt);
}

#[test]
fn publish_extent_and() {
    let mut wire = valid_wire();
    put_u32(&mut wire, 72, 16);
    put_u32(&mut wire, 136, 1);
    let record = HEADER_SIZE + 16;
    put_u32(&mut wire, record, 0x0100_007f);
    wire[record + 4..record + 6].copy_from_slice(&8080_u16.to_le_bytes());
    wire[record + 6..record + 8].copy_from_slice(&80_u16.to_le_bytes());
    let parsed = LaunchWire::parse(&wire).unwrap();
    assert_eq!(
        parsed.publish_rules(),
        Ok(vec![PublishRule {
            host_ipv4_be: 0x0100_007f,
            host_port: 8080,
            guest_port: 80,
        }])
    );

    for (offset, count) in [(0, 1), (16, 0), (15, 1), (48, 1), (44, 1)] {
        let mut malformed = valid_wire();
        put_u32(&mut malformed, 72, offset);
        put_u32(&mut malformed, 136, count);
        assert_eq!(LaunchWire::parse(&malformed).unwrap_err(), WireError::Corrupt);
    }

    for port_offset in [record + 4, record + 6] {
        let mut zero_port = wire.clone();
        zero_port[port_offset..port_offset + 2].fill(0);
        let parsed = LaunchWire::parse(&zero_port).unwrap();
        assert_eq!(parsed.publish_rules(), Err(WireError::Corrupt));
    }
    assert_eq!(
        LaunchWire::parse(&valid_wire()).unwrap().publish_rules(),
        Err(WireError::InvalidArgument)
    );
}

#[test]
fn strings_distinguish_invalid() {
    let mut wire = valid_wire();
    wire[HEADER_SIZE + 1..HEADER_SIZE + 6].copy_from_slice(b"name\0");
    wire[HEADER_SIZE + 40..].fill(b'x');
    let parsed = LaunchWire::parse(&wire).unwrap();
    assert_eq!(parsed.string(1), Ok(&b"name"[..]));
    assert_eq!(parsed.string(POOL_SIZE as u32), Err(WireError::InvalidArgument));
    assert_eq!(parsed.string(40), Err(WireError::Corrupt));
}

#[test]
fn arguments_require_nonempty() {
    let mut wire = valid_wire();
    put_u32(&mut wire, 108, 8);
    wire[HEADER_SIZE + 8..HEADER_SIZE + 22].copy_from_slice(b"guest\0--flag\0\0");
    let parsed = LaunchWire::parse(&wire).unwrap();
    assert_eq!(parsed.arguments(), Ok(vec![&b"guest"[..], &b"--flag"[..]]));
    assert_eq!(parsed.argument(0), Ok(&b"guest"[..]));
    assert_eq!(parsed.argument(2), Err(WireError::NotFound));

    let absent_wire = valid_wire();
    let absent = LaunchWire::parse(&absent_wire).unwrap();
    assert_eq!(absent.arguments(), Err(WireError::InvalidArgument));
    assert_eq!(absent.argument(0), Err(WireError::Corrupt));

    let mut empty = valid_wire();
    put_u32(&mut empty, 108, 8);
    assert_eq!(LaunchWire::parse(&empty).unwrap().arguments(), Err(WireError::Corrupt));

    let mut no_final_empty = valid_wire();
    put_u32(&mut no_final_empty, 108, 40);
    no_final_empty[HEADER_SIZE + 40..].copy_from_slice(b"1234567\0");
    assert_eq!(
        LaunchWire::parse(&no_final_empty).unwrap().arguments(),
        Err(WireError::Corrupt)
    );

    let mut no_terminator = valid_wire();
    put_u32(&mut no_terminator, 108, 40);
    no_terminator[HEADER_SIZE + 40..].fill(b'x');
    assert_eq!(
        LaunchWire::parse(&no_terminator).unwrap().arguments(),
        Err(WireError::Corrupt)
    );
}
