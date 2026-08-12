use hl_network::SocketAddress;

pub(super) const QUEUE_LIMIT: usize = 4096;
pub(super) const BYTE_LIMIT: usize = 1024 * 1024;

struct EchoRequest<'a> {
    bytes: &'a [u8],
}

impl<'a> TryFrom<&'a [u8]> for EchoRequest<'a> {
    type Error = ();

    fn try_from(bytes: &'a [u8]) -> Result<Self, Self::Error> {
        if bytes.len() < 8 || bytes.len() > 65_535 || bytes[0] != 8 || bytes[1] != 0 {
            return Err(());
        }
        Ok(Self { bytes })
    }
}

impl EchoRequest<'_> {
    fn reply(&self) -> Vec<u8> {
        let mut reply = self.bytes.to_vec();
        reply[0] = 0;
        reply[2] = 0;
        reply[3] = 0;
        let checksum = Self::checksum(&reply);
        reply[2..4].copy_from_slice(&checksum.to_be_bytes());
        reply
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
    if let Ok(request) = EchoRequest::try_from(request) {
        let reply = request.reply();
        *bytes += reply.len();
        packets.push_back((reply, peer));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_changes_type_and_repairs_checksum() {
        let request = [8, 0, 0, 0, 0x12, 0x34, 0, 1];
        let reply = EchoRequest::try_from(request.as_slice()).unwrap().reply();
        assert_eq!(reply[0], 0);
        assert_eq!(EchoRequest::checksum(&reply), 0);
    }
}
