//! Adversarial coverage for the TRANSPORT framing + serve loop — the mechanism that moves encoded batches
//! across a process boundary while EXECUTING nothing. The wire here (16-byte submit header + 1-byte ack +
//! the length-prefixed readback response) is byte-identical to the shipped guest/host, so it must survive:
//! partial reads / split frames, a clean peer close at a frame boundary, a truncated payload, a malformed
//! (undecodable) submit payload, and interleaved readback (`READBACK_MAGIC`) frames — always with an honest
//! error, never a deadlock, corruption, or panic.
//!
//! These drive the adapter (`transport::adapter::unix`) directly over `UnixStream::pair`, and the serve
//! loop over a real `UnixListener`, complementing `tests/transport.rs` + `tests/readback.rs`.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use hl_gpu::transport::adapter::unix::{Connection, MAX_FRAME_BYTES};
use hl_gpu::transport::model::header::{SubmitHeader, ACK_FAIL, ACK_OK};
use hl_gpu::transport::model::readback::{
    ReadbackRequest, READBACK_FAIL, READBACK_MAGIC, READBACK_OK,
};
use hl_gpu::transport::{
    serve_connection, serve_connection_with_handler, ConnectionHandler, Verdict,
};
use hl_gpu::{Capabilities, Cmd};

// ---------------------------------------------------------------------------------------------------
// adapter framing: read_frame partial/EOF/truncation, split writes
// ---------------------------------------------------------------------------------------------------

#[test]
fn read_frame_reassembles_a_full_frame() {
    let (a, b) = UnixStream::pair().unwrap();
    let header = SubmitHeader {
        surface_id: 7,
        width: 64,
        height: 32,
        len: 5,
    };
    Connection::new(&a)
        .write_frame(&header, &[1, 2, 3, 4, 5])
        .unwrap();
    let frame = Connection::new(&b)
        .read_frame()
        .unwrap()
        .expect("a full frame");
    assert_eq!(frame.header, header);
    assert_eq!(frame.payload, vec![1, 2, 3, 4, 5]);
}

#[test]
fn read_frame_returns_none_on_clean_eof() {
    let (a, b) = UnixStream::pair().unwrap();
    drop(a); // peer closes with nothing sent — EOF exactly at a frame boundary
    assert!(
        Connection::new(&b).read_frame().unwrap().is_none(),
        "a clean close is Ok(None), not an error"
    );
}

#[test]
fn read_frame_errors_on_a_truncated_payload() {
    // A full header promising 16 payload bytes, but only 4 arrive before the peer closes: the payload read
    // must surface an IO error (truncated frame), NOT a silent short frame or a panic.
    let (a, b) = UnixStream::pair().unwrap();
    let header = SubmitHeader {
        surface_id: 1,
        width: 0,
        height: 0,
        len: 16,
    };
    let mut bytes = header.to_bytes().to_vec();
    bytes.extend_from_slice(&[0xAB; 4]); // only 4 of the promised 16 payload bytes
    let mut writer = a;
    writer.write_all(&bytes).unwrap();
    drop(writer); // EOF mid-payload
    assert!(
        Connection::new(&b).read_frame().is_err(),
        "a truncated payload is a transport error"
    );
}

#[test]
fn read_frame_reassembles_across_interleaved_partial_writes() {
    // The header and payload arrive in separate writes with a gap; the blocking reader must reassemble the
    // whole frame from the partial arrivals (read_exact loops), never returning a short/partial frame.
    let (a, b) = UnixStream::pair().unwrap();
    let header = SubmitHeader {
        surface_id: 3,
        width: 1,
        height: 1,
        len: 8,
    };
    let writer = thread::spawn(move || {
        let mut w = a;
        w.write_all(&header.to_bytes()).unwrap(); // header first
        thread::sleep(Duration::from_millis(20));
        w.write_all(&[9, 9, 9, 9]).unwrap(); // half the payload
        thread::sleep(Duration::from_millis(20));
        w.write_all(&[8, 8, 8, 8]).unwrap(); // the rest
    });
    let frame = Connection::new(&b)
        .read_frame()
        .unwrap()
        .expect("frame reassembled from partial writes");
    assert_eq!(frame.header.len, 8);
    assert_eq!(frame.payload, vec![9, 9, 9, 9, 8, 8, 8, 8]);
    writer.join().unwrap();
}

#[test]
fn read_frame_caps_an_untrusted_over_cap_length_but_still_passes_legit_frames() {
    // DoS gap: `read_frame` did `vec![0u8; header.len]` from an untrusted `u32` length (up to 4 GiB)
    // BEFORE reading the body. 1) A header declaring a payload above MAX_FRAME_BYTES must be refused at
    // header inspection — a typed InvalidData "FrameTooLarge" error — WITHOUT preallocating: we send ONLY
    // the 16-byte header (never a giant body) and the reader must still error rather than block on a
    // hundreds-of-MB allocation.
    let (a, b) = UnixStream::pair().unwrap();
    let huge = SubmitHeader {
        surface_id: 1,
        width: 0,
        height: 0,
        len: MAX_FRAME_BYTES + 1,
    };
    let mut w = a;
    w.write_all(&huge.to_bytes()).unwrap(); // header only; the promised giant payload is never sent
    drop(w);
    let err = Connection::new(&b)
        .read_frame()
        .expect_err("an over-cap frame length must error, not preallocate");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidData,
        "over-cap frame is a FrameTooLarge/protocol rejection"
    );

    // 2) A legitimately-sized frame (well under the cap) still round-trips byte-for-byte — the cap does
    //    not change framing for valid inputs.
    let (c, d) = UnixStream::pair().unwrap();
    let header = SubmitHeader {
        surface_id: 9,
        width: 4,
        height: 4,
        len: 6,
    };
    Connection::new(&c)
        .write_frame(&header, &[1, 2, 3, 4, 5, 6])
        .unwrap();
    let frame = Connection::new(&d)
        .read_frame()
        .unwrap()
        .expect("a legit frame round-trips under the cap");
    assert_eq!(frame.header, header);
    assert_eq!(frame.payload, vec![1, 2, 3, 4, 5, 6]);
}

#[test]
fn read_frame_errors_on_a_truncated_header_but_ok_none_on_a_clean_boundary() {
    // Clean boundary: the peer closes having sent NOTHING -> end-of-stream is still Ok(None) (unchanged).
    let (a, b) = UnixStream::pair().unwrap();
    drop(a);
    assert!(
        Connection::new(&b).read_frame().unwrap().is_none(),
        "a clean close at a frame boundary is still Ok(None)"
    );

    // Truncated header: SOME header bytes then EOF is a torn frame, NOT a clean boundary — it must error
    // like a truncated payload does, instead of being silently swallowed as Ok(None).
    let (c, d) = UnixStream::pair().unwrap();
    let mut w = c;
    w.write_all(&[0xAB; 8]).unwrap(); // only 8 of the 16 header bytes, then close
    drop(w);
    let err = Connection::new(&d)
        .read_frame()
        .expect_err("a partial header then EOF is a truncated-frame error");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
}

// ---------------------------------------------------------------------------------------------------
// readback response framing (disjoint from the submit ack)
// ---------------------------------------------------------------------------------------------------

#[test]
fn readback_response_ok_round_trips() {
    let (a, b) = UnixStream::pair().unwrap();
    Connection::new(&a)
        .write_readback_response(READBACK_OK, &[0xDE, 0xAD, 0xBE, 0xEF])
        .unwrap();
    let got = Connection::new(&b).read_readback_response(4).unwrap();
    assert_eq!(got, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn readback_response_fail_is_a_typed_error_not_empty_bytes() {
    // A FAIL status with a zero-length body must surface as an error, so a caller never mistakes a failed
    // readback for a legitimate empty result.
    let (a, b) = UnixStream::pair().unwrap();
    Connection::new(&a)
        .write_readback_response(READBACK_FAIL, &[])
        .unwrap();
    assert!(
        Connection::new(&b).read_readback_response(0).is_err(),
        "a FAIL readback response is an error"
    );
}

#[test]
fn readback_response_empty_success_is_not_failure() {
    let (a, b) = UnixStream::pair().unwrap();
    Connection::new(&a)
        .write_readback_response(READBACK_OK, &[])
        .unwrap();
    assert!(Connection::new(&b)
        .read_readback_response(0)
        .unwrap()
        .is_empty());
}

#[test]
fn large_readback_response_round_trips_without_changing_framing() {
    let (a, b) = UnixStream::pair().unwrap();
    a.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let expected = (0..8 * 1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let sent = expected.clone();
    let writer = std::thread::spawn(move || {
        Connection::new(&a)
            .write_readback_response(READBACK_OK, &sent)
            .unwrap();
    });
    let received = Connection::new(&b)
        .read_readback_response(expected.len())
        .unwrap();
    writer.join().unwrap();
    assert_eq!(received, expected);
}

#[test]
fn readback_response_rejects_untrusted_header_before_allocating() {
    for (status, declared_len, expected_len, label) in [
        (READBACK_OK, u32::MAX, 4, "huge success length"),
        (0x7f, 0, 0, "unknown status"),
        (READBACK_FAIL, 1, 0, "failure carrying a body"),
    ] {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.write_all(&[status]).unwrap();
        writer.write_all(&declared_len.to_le_bytes()).unwrap();
        if declared_len == 1 {
            writer.write_all(&[0xaa]).unwrap();
        }
        let error = Connection::new(&reader)
            .read_readback_response(expected_len)
            .expect_err(label);
        assert!(
            matches!(
                error,
                hl_gpu::transport::adapter::unix::ReadbackResponseError::Malformed(_)
            ),
            "{label}: {error:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// serve loop: malformed submit payload is NACKed; readback routes by the magic; both over one connection
// ---------------------------------------------------------------------------------------------------

struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-txadv-{tag}-{}-{:?}.sock",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn server_nacks_a_malformed_submit_payload_without_calling_the_handler() {
    // A frame whose payload is NOT decodable IR must be rejected at the boundary with ACK_FAIL, and the
    // handler must never see it. We drive a raw client so we control the exact (garbage) payload bytes.
    let sock = TempSock::new("nack-decode");
    let listener = UnixListener::bind(&sock.0).unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut handler_calls = 0u32;
        serve_connection(&stream, &caps, |_h, _b: &[Cmd]| {
            handler_calls += 1;
            true
        })
        .unwrap();
        handler_calls
    });

    // Client: read+discard the handshake, then send a frame with a bogus leading tag (250) as payload.
    let mut client = UnixStream::connect(&sock.0).unwrap();
    // handshake = [u32 len][body]; read the len then skip the body.
    let mut len_bytes = [0u8; 4];
    client.read_exact(&mut len_bytes).unwrap();
    let hs_len = u32::from_le_bytes(len_bytes) as usize;
    let mut hs_body = vec![0u8; hs_len];
    client.read_exact(&mut hs_body).unwrap();

    let payload = [250u8, 0, 0, 0, 0]; // an unknown top-level tag -> decode_stream errors
    let header = SubmitHeader {
        surface_id: 1,
        width: 0,
        height: 0,
        len: payload.len() as u32,
    };
    client.write_all(&header.to_bytes()).unwrap();
    client.write_all(&payload).unwrap();
    let ack = Connection::new(&client).read_ack().unwrap();
    assert_eq!(ack, ACK_FAIL, "an undecodable payload is NACKed");
    drop(client);

    let handler_calls = server.join().unwrap();
    assert_eq!(
        handler_calls, 0,
        "the handler never saw the malformed frame"
    );
}

/// A host that acks every submit and answers a readback for buffer id 42 with fixed bytes.
struct FixedReadbackHost {
    submits: u32,
}
impl ConnectionHandler for FixedReadbackHost {
    fn submit(&mut self, _h: &SubmitHeader, _batch: &[Cmd]) -> Verdict {
        self.submits += 1;
        Verdict::Ack
    }
    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        if req.id == 42 {
            Some(vec![0x10, 0x20, 0x30])
        } else {
            None // unknown buffer -> FAIL
        }
    }
}

#[test]
fn readback_frame_routes_to_readback_never_to_submit() {
    // A frame stamped with READBACK_MAGIC in surface_id is answered with the length-prefixed readback
    // response and must NOT be counted as a submit; a submit frame on the same connection still acks.
    let sock = TempSock::new("route");
    let listener = UnixListener::bind(&sock.0).unwrap();

    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = FixedReadbackHost { submits: 0 };
        serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
        host.submits
    });

    let mut client = UnixStream::connect(&sock.0).unwrap();
    // Discard handshake.
    let mut len_bytes = [0u8; 4];
    client.read_exact(&mut len_bytes).unwrap();
    let mut hs = vec![0u8; u32::from_le_bytes(len_bytes) as usize];
    client.read_exact(&mut hs).unwrap();

    // 1) A real submit (a single valid CreateFence) -> ACK_OK.
    let submit_payload = hl_gpu::Encoder::stream(&[Cmd::CreateFence(1)]);
    let submit_hdr = SubmitHeader {
        surface_id: 5,
        width: 0,
        height: 0,
        len: submit_payload.len() as u32,
    };
    client.write_all(&submit_hdr.to_bytes()).unwrap();
    client.write_all(&submit_payload).unwrap();
    assert_eq!(Connection::new(&client).read_ack().unwrap(), ACK_OK);

    // 2) A readback for buffer 42 -> the readback response (never the 1-byte ack), disjoint on the wire.
    Connection::new(&client)
        .write_readback_request(&ReadbackRequest::buffer(42, 0, 3))
        .unwrap();
    let bytes = Connection::new(&client).read_readback_response(3).unwrap();
    assert_eq!(
        bytes,
        vec![0x10, 0x20, 0x30],
        "readback returned the host's bytes"
    );

    // 3) A readback for an unknown buffer -> FAIL (surfaces as an error, never garbage).
    Connection::new(&client)
        .write_readback_request(&ReadbackRequest::buffer(7, 0, 3))
        .unwrap();
    assert!(
        Connection::new(&client).read_readback_response(3).is_err(),
        "unknown-buffer readback fails cleanly"
    );

    // 4) Sanity: the readback header really did carry the magic sentinel.
    assert_eq!(READBACK_MAGIC, u32::MAX);

    drop(client);
    let submits = server.join().unwrap();
    assert_eq!(
        submits, 1,
        "only the real submit was counted; readback frames bypassed submit"
    );
}

#[test]
fn serve_loop_returns_cleanly_when_the_peer_closes() {
    // The serve loop must return Ok when the client drops after the handshake (clean EOF), not hang or err.
    let sock = TempSock::new("close");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        serve_connection(&stream, &caps, |_h, _b: &[Cmd]| true)
    });
    let client = UnixStream::connect(&sock.0).unwrap();
    // Read the handshake so the server has written it, then close without sending any frame.
    let mut buf = [0u8; 4];
    let mut c = &client;
    c.read_exact(&mut buf).unwrap();
    let mut body = vec![0u8; u32::from_le_bytes(buf) as usize];
    c.read_exact(&mut body).unwrap();
    drop(client);
    assert!(
        server.join().unwrap().is_ok(),
        "serve_loop returns Ok on a clean peer close"
    );
}
