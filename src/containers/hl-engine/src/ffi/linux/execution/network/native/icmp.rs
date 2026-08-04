use hl_network::SocketAddress;

pub(super) const QUEUE_LIMIT: usize = 4096;
pub(super) const BYTE_LIMIT: usize = 1024 * 1024;

pub(super) fn echo(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 8 || request.len() > 65_535 || request[0] != 8 || request[1] != 0 {
        return None;
    }
    let mut reply = request.to_vec();
    reply[0] = 0;
    reply[2] = 0;
    reply[3] = 0;
    let checksum = checksum(&reply);
    reply[2..4].copy_from_slice(&checksum.to_be_bytes());
    Some(reply)
}

pub(super) fn enqueue(
    packets: &mut std::collections::VecDeque<(Vec<u8>, SocketAddress)>,
    bytes: &mut usize,
    request: &[u8],
    peer: SocketAddress,
) -> Result<(), ()> {
    if packets.len() >= QUEUE_LIMIT || request.len() > BYTE_LIMIT.saturating_sub(*bytes) {
        return Err(());
    }
    if let Some(reply) = echo(request) {
        *bytes += reply.len();
        packets.push_back((reply, peer));
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0_u32;
    for chunk in bytes.chunks(2) {
        sum += if chunk.len() == 2 {
            u32::from(u16::from_be_bytes([chunk[0], chunk[1]]))
        } else {
            u32::from(chunk[0]) << 8
        };
    }
    while sum > u32::from(u16::MAX) {
        sum = (sum & u32::from(u16::MAX)) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_changes_type_and_repairs_checksum() {
        let request = [8, 0, 0, 0, 0x12, 0x34, 0, 1];
        let reply = echo(&request).unwrap();
        assert_eq!(reply[0], 0);
        assert_eq!(checksum(&reply), 0);
    }
}
