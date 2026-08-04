use std::net::{IpAddr, ToSocketAddrs};

const HEADER: usize = 12;
pub(super) const QUEUE_LIMIT: usize = 64;
pub(super) const BYTE_LIMIT: usize = 1024 * 1024;
const MESSAGE_LIMIT: usize = u16::MAX as usize;
const ANSWER_LIMIT: usize = 32;

pub(super) fn endpoint(address: &hl_network::SocketAddress) -> bool {
    matches!(
        address,
        hl_network::SocketAddress::Inet4 {
            address: [127, 0, 0, 11],
            port: 53
        }
    )
}

pub(super) fn response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < HEADER || query.len() > MESSAGE_LIMIT || u16::from_be_bytes([query[4], query[5]]) == 0 {
        return None;
    }
    let (name, end) = question_name(query, HEADER)?;
    if end + 4 > query.len() {
        return None;
    }
    let kind = u16::from_be_bytes([query[end], query[end + 1]]);
    let class = u16::from_be_bytes([query[end + 2], query[end + 3]]);
    let question_end = end + 4;
    let mut addresses = Vec::new();
    if class == 1 && matches!(kind, 1 | 28) {
        if let Ok(found) = (name.as_str(), 0).to_socket_addrs() {
            for address in found {
                let address = address.ip();
                if (kind == 1 && address.is_ipv4()) || (kind == 28 && address.is_ipv6()) {
                    if !addresses.contains(&address) {
                        addresses.push(address);
                        if addresses.len() == ANSWER_LIMIT {
                            break;
                        }
                    }
                }
            }
        }
    }
    let mut output = Vec::with_capacity(question_end + addresses.len() * 28);
    output.extend_from_slice(&query[..2]);
    let request_flags = u16::from_be_bytes([query[2], query[3]]);
    let response_flags = 0x8080 | (request_flags & 0x0100);
    output.extend_from_slice(&response_flags.to_be_bytes());
    output.extend_from_slice(&1_u16.to_be_bytes());
    output.extend_from_slice(&0_u16.to_be_bytes());
    output.extend_from_slice(&[0; 4]);
    output.extend_from_slice(&query[HEADER..question_end]);
    let mut answer_count = 0_u16;
    for address in addresses {
        let record_length = if address.is_ipv4() { 16 } else { 28 };
        if output.len() + record_length > MESSAGE_LIMIT {
            break;
        }
        output.extend_from_slice(&[0xc0, 0x0c]);
        output.extend_from_slice(&kind.to_be_bytes());
        output.extend_from_slice(&1_u16.to_be_bytes());
        output.extend_from_slice(&30_u32.to_be_bytes());
        match address {
            IpAddr::V4(value) => {
                output.extend_from_slice(&4_u16.to_be_bytes());
                output.extend_from_slice(&value.octets());
            }
            IpAddr::V6(value) => {
                output.extend_from_slice(&16_u16.to_be_bytes());
                output.extend_from_slice(&value.octets());
            }
        }
        answer_count += 1;
    }
    output[6..8].copy_from_slice(&answer_count.to_be_bytes());
    Some(output)
}

pub(super) fn queue_available(packets: usize, bytes: usize, response: usize) -> bool {
    packets < QUEUE_LIMIT && response <= BYTE_LIMIT && bytes <= BYTE_LIMIT - response
}

fn question_name(query: &[u8], mut offset: usize) -> Option<(String, usize)> {
    let mut labels = Vec::new();
    while offset < query.len() {
        let length = usize::from(query[offset]);
        offset += 1;
        if length == 0 {
            return Some((labels.join("."), offset));
        }
        if length > 63 || offset.checked_add(length)? > query.len() {
            return None;
        }
        labels.push(std::str::from_utf8(&query[offset..offset + length]).ok()?.to_owned());
        offset += length;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_compressed_and_oversized_questions() {
        let mut compressed = vec![0; HEADER];
        compressed[5] = 1;
        compressed.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
        assert!(response(&compressed).is_none());

        let mut oversized = vec![0; MESSAGE_LIMIT + 1];
        oversized[5] = 1;
        assert!(response(&oversized).is_none());
    }

    #[test]
    fn queue_admission_bounds_packets_and_total_bytes() {
        assert!(queue_available(QUEUE_LIMIT - 1, 0, 512));
        assert!(!queue_available(QUEUE_LIMIT, 0, 1));
        assert!(!queue_available(0, BYTE_LIMIT, 1));
        assert!(!queue_available(0, 0, BYTE_LIMIT + 1));
    }
}
