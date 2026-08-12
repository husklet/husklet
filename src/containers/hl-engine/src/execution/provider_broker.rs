use crate::engine::EngineError;
use crate::native::AuthorityWorker;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const MAGIC: u32 = 0x484c_5052;
const VERSION: u16 = 1;
const REQUEST: u16 = 3;
const REPLY: u16 = 4;
const HEADER_SIZE: usize = 32;
const MAXIMUM_PAYLOAD: usize = 4096;

pub(super) fn spawn(stream: UnixStream, authority: Arc<Mutex<AuthorityWorker>>) -> Result<JoinHandle<()>, EngineError> {
    std::thread::Builder::new()
        .name("hl-c-provider-broker".into())
        .spawn(move || {
            serve(stream, |payload| {
                let mut authority = authority.lock().map_err(|_| ())?;
                authority.provider(payload).map_err(|_| ())
            });
        })
        .map_err(|_| EngineError::LaunchFailed)
}

fn serve(mut stream: UnixStream, mut dispatch: impl FnMut(&[u8]) -> Result<Vec<u8>, ()>) {
    loop {
        let mut header = [0_u8; HEADER_SIZE];
        if stream.read_exact(&mut header).is_err() {
            return;
        }
        let Some((request, size)) = decode_request(&header) else {
            return;
        };
        let mut payload = vec![0_u8; size];
        if stream.read_exact(&mut payload).is_err() || !hl_provider::TreeWire::is_request(&payload) {
            return;
        }
        let Ok(reply) = dispatch(&payload) else {
            return;
        };
        if reply.len() > MAXIMUM_PAYLOAD {
            return;
        }
        let header = reply_header(request, reply.len());
        if stream.write_all(&header).is_err() || stream.write_all(&reply).is_err() {
            return;
        }
    }
}

fn decode_request(header: &[u8; HEADER_SIZE]) -> Option<(u64, usize)> {
    let magic = u32::from_le_bytes(header[0..4].try_into().ok()?);
    let version = u16::from_le_bytes(header[4..6].try_into().ok()?);
    let kind = u16::from_le_bytes(header[6..8].try_into().ok()?);
    let size = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
    let request = u64::from_le_bytes(header[12..20].try_into().ok()?);
    (magic == MAGIC
        && version == VERSION
        && kind == REQUEST
        && request != 0
        && (1..=MAXIMUM_PAYLOAD).contains(&size)
        && header[20..].iter().all(|byte| *byte == 0))
    .then_some((request, size))
}

fn reply_header(request: u64, size: usize) -> [u8; HEADER_SIZE] {
    let mut header = [0_u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    header[4..6].copy_from_slice(&VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&REPLY.to_le_bytes());
    header[8..12].copy_from_slice(&(size as u32).to_le_bytes());
    header[12..20].copy_from_slice(&request.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::{HEADER_SIZE, MAGIC, MAXIMUM_PAYLOAD, REQUEST, VERSION, decode_request, reply_header, serve};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    fn request_header(id: u64, size: usize) -> [u8; HEADER_SIZE] {
        let mut header = reply_header(id, size);
        header[6..8].copy_from_slice(&REQUEST.to_le_bytes());
        header
    }

    #[test]
    fn broker_forwards_only_bounded_tree_requests() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve(server, |payload| Ok(vec![payload[0], 7])));
        client.write_all(&request_header(41, 1)).unwrap();
        client.write_all(&[16]).unwrap();
        let mut header = [0; HEADER_SIZE];
        client.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..4], &MAGIC.to_le_bytes());
        assert_eq!(&header[4..6], &VERSION.to_le_bytes());
        assert_eq!(u64::from_le_bytes(header[12..20].try_into().unwrap()), 41);
        let mut reply = [0; 2];
        client.read_exact(&mut reply).unwrap();
        assert_eq!(reply, [16, 7]);
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn malformed_headers_fail_closed() {
        let mut oversized = request_header(1, MAXIMUM_PAYLOAD + 1);
        assert_eq!(decode_request(&oversized), None);
        let mut empty = request_header(1, 0);
        assert_eq!(decode_request(&empty), None);
        empty = request_header(1, 1);
        empty[0..4].copy_from_slice(&(MAGIC ^ 1).to_le_bytes());
        assert_eq!(decode_request(&empty), None);
        empty = request_header(1, 1);
        empty[4..6].copy_from_slice(&(VERSION + 1).to_le_bytes());
        assert_eq!(decode_request(&empty), None);
        empty = request_header(1, 1);
        empty[6..8].copy_from_slice(&4_u16.to_le_bytes());
        assert_eq!(decode_request(&empty), None);
        oversized = request_header(1, 1);
        oversized[20] = 1;
        assert_eq!(decode_request(&oversized), None);
        assert_eq!(decode_request(&request_header(0, 1)), None);
    }

    #[test]
    fn truncated_header_and_payload_close_without_dispatch() {
        for bytes in [
            request_header(1, 1)[..HEADER_SIZE - 1].to_vec(),
            request_header(1, 2).to_vec(),
        ] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || serve(server, |_| panic!("must not dispatch")));
            client.write_all(&bytes).unwrap();
            if bytes.len() == HEADER_SIZE {
                client.write_all(&[16]).unwrap();
            }
            client.shutdown(std::net::Shutdown::Write).unwrap();
            let mut byte = [0];
            assert_eq!(client.read(&mut byte).unwrap(), 0);
            worker.join().unwrap();
        }
    }

    #[test]
    fn dead_authority_and_oversized_reply_close_transport() {
        for oversized_reply in [false, true] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let worker = std::thread::spawn(move || {
                serve(server, move |_| {
                    if oversized_reply {
                        Ok(vec![0; MAXIMUM_PAYLOAD + 1])
                    } else {
                        Err(())
                    }
                });
            });
            client.write_all(&request_header(7, 1)).unwrap();
            client.write_all(&[16]).unwrap();
            let mut byte = [0];
            assert_eq!(client.read(&mut byte).unwrap(), 0);
            worker.join().unwrap();
        }
    }

    #[test]
    fn peer_death_ends_broker_without_dispatch() {
        let (client, server) = UnixStream::pair().unwrap();
        drop(client);
        let worker = std::thread::spawn(move || serve(server, |_| panic!("must not dispatch")));
        worker.join().unwrap();
    }

    #[test]
    fn non_tree_payload_closes_transport_without_dispatch() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || serve(server, |_| panic!("must not dispatch")));
        client.write_all(&request_header(1, 1)).unwrap();
        client.write_all(&[1]).unwrap();
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).unwrap(), 0);
        worker.join().unwrap();
    }
}
