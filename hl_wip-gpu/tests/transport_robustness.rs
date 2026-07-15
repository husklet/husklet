//! Connection-robustness tests for the host serve loop: a per-frame failure — an executor that NACKs one
//! op, or a single frame whose declared length exceeds the transport cap — must NEVER tear down the
//! persistent connection. Dropping the connection loses the host's warm per-connection caches AND kills
//! every subsequent frame (the guest sees `Broken pipe`), which is exactly the GTK4/GskGL failure these
//! tests pin: one bad frame closed the pipe and every following frame died with it.
//!
//! They assert three guarantees over the REAL Unix-socket transport:
//!   1. a large multi-MB, thousands-of-`Cmd` frame round-trips client → host → readback correctly;
//!   2. a frame the host NACKs leaves the SAME connection serving (the next good frame ACKs, no reconnect);
//!   3. an over-cap frame is drained + NACKed and the SAME connection then serves a following good frame.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::transport::adapter::unix::{self, MAX_FRAME_BYTES};
use hl_gpu::transport::model::header::{SubmitHeader, ACK_FAIL, ACK_OK};
use hl_gpu::transport::Verdict;
use hl_gpu::{
    serve_connection_with_handler, Capabilities, Cmd, CommandSink, ConnectionHandler, ReadbackRequest,
    RemoteCommandSink,
};

/// A unique temp socket path for one test, removed on drop.
struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-transport-robust-{tag}-{}-{:?}.sock",
            std::process::id(),
            thread::current().id()
        ));
        let _ = std::fs::remove_file(&p);
        TempSock(p)
    }
    fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for TempSock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A minimal host: records the last decoded batch's bytes into a channel, stores every `WriteBuffer`'s bytes
/// so a following readback can return them, and NACKs any frame that contains a `CreateFence(0)` marker (a
/// stand-in for "one op the executor rejects"). Serves the readback path off its stored buffers.
struct TestHost {
    buffers: HashMap<u32, Vec<u8>>,
    batch_tx: mpsc::Sender<Vec<Cmd>>,
}

impl ConnectionHandler for TestHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        // A frame carrying the reject marker is NACKed WITHOUT closing the connection.
        if batch.iter().any(|c| matches!(c, Cmd::CreateFence(0))) {
            return Verdict::Nack;
        }
        for c in batch {
            if let Cmd::WriteBuffer { id, offset, data } = c {
                let buf = self.buffers.entry(*id).or_default();
                let end = *offset as usize + data.len();
                if buf.len() < end {
                    buf.resize(end, 0);
                }
                buf[*offset as usize..end].copy_from_slice(data);
            }
        }
        let _ = self.batch_tx.send(batch.to_vec());
        Verdict::Ack
    }

    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        let buf = self.buffers.get(&req.id)?;
        let start = req.offset as usize;
        let end = start + req.len as usize;
        buf.get(start..end).map(|s| s.to_vec())
    }
}

fn spawn_host(sock: &TempSock) -> (thread::JoinHandle<()>, mpsc::Receiver<Vec<Cmd>>) {
    let listener = UnixListener::bind(&sock.0).unwrap();
    let (batch_tx, batch_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = TestHost { buffers: HashMap::new(), batch_tx };
        // The loop returns only on a clean client disconnect; a NACKed/over-cap frame must NOT end it early.
        serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });
    (handle, batch_rx)
}

#[test]
fn large_multi_mb_many_cmd_frame_round_trips_over_the_socket() {
    // A GskGL-shaped frame: thousands of commands plus a multi-MB buffer upload. It must transit the real
    // socket in one framed submit (writes loop until fully sent, reads until the full declared length is
    // read — no truncation) and the host must decode it identically AND serve the uploaded bytes back.
    let sock = TempSock::new("large");
    let (host, batch_rx) = spawn_host(&sock);

    const BIG_LEN: usize = 6 << 20; // 6 MiB payload in one WriteBuffer — comfortably multi-MB
    let big: Vec<u8> = (0..BIG_LEN).map(|i| (i * 2654435761usize) as u8).collect();
    let mut batch = Vec::new();
    batch.push(Cmd::CreateBuffer(
        1,
        BufferDesc { size: BIG_LEN as u64, usage: buffer_usage::COPY_DST, label: "big".into() },
    ));
    batch.push(Cmd::WriteBuffer { id: 1, offset: 0, data: big.clone() });
    // Thousands of additional commands so the frame is both many-Cmd AND multi-MB.
    for i in 0..4000u32 {
        batch.push(Cmd::CreateFence(1000 + i));
    }

    let mut sink = RemoteCommandSink::new(sock.path());
    sink.submit(&batch).expect("large frame acknowledged over the socket");

    // The host decoded the exact batch we submitted (proves the multi-MB / many-Cmd frame transited whole).
    let decoded = batch_rx.recv().unwrap();
    assert_eq!(decoded.len(), batch.len(), "every command in the large frame arrived");
    assert_eq!(decoded, batch, "the large frame decoded byte-identically");

    // Readback of the uploaded buffer over the SAME connection returns exactly the bytes we wrote.
    let read = sink
        .read_buffer(hl_gpu::BufferId(1), 0, BIG_LEN)
        .expect("readback of the multi-MB buffer succeeds");
    assert_eq!(read, big, "the multi-MB upload round-tripped client -> host -> readback intact");

    drop(sink);
    host.join().unwrap();
}

#[test]
fn a_nacked_frame_leaves_the_same_connection_serving() {
    // The heart of the fix: a host that NACKs one frame must keep serving. The next good frame on the SAME
    // connection ACKs, and the sink never reconnects (connects() stays 1) — proving the pipe was not closed.
    let sock = TempSock::new("nack-keep");
    let (host, batch_rx) = spawn_host(&sock);

    let mut sink = RemoteCommandSink::new(sock.path());

    // A frame with the reject marker is NACKed → submit() returns Err, but the connection survives.
    let bad = sink.submit(&[Cmd::CreateFence(0)]);
    assert!(bad.is_err(), "the marked frame is NACKed");
    assert_eq!(sink.connects(), 1, "a NACK is not retried on a fresh connection");

    // A following GOOD frame on the SAME connection must ACK — this is the regression guard: before the fix
    // the connection would have been closed and this submit would fail with Broken pipe (or force a
    // reconnect against a host with no second accept).
    sink.submit(&[Cmd::CreateFence(7)]).expect("the same connection still serves after a NACK");
    assert_eq!(sink.connects(), 1, "the good frame reused the SAME connection — no reconnect, no close");

    let got = batch_rx.recv().unwrap();
    assert_eq!(got, vec![Cmd::CreateFence(7)], "the host served the good frame after the NACK");

    drop(sink);
    host.join().unwrap();
}

#[test]
fn an_over_cap_frame_is_drained_and_nacked_without_closing_the_connection() {
    // The exact GTK4/GskGL root cause: a single frame whose declared length exceeds the transport cap must
    // be drained + NACKed, NOT close the connection. We drive the raw wire (the client sink caps its own
    // frames, so we forge the over-cap header directly) and prove the SAME socket serves a good frame after.
    let sock = TempSock::new("overcap");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let host = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut h = TestHost { buffers: HashMap::new(), batch_tx: mpsc::channel().0 };
        serve_connection_with_handler(&stream, &caps, &mut h).unwrap();
    });

    let stream = UnixStream::connect(&sock.0).unwrap();
    // Consume the host's capability handshake first (the guest reads this on connect).
    let _caps = unix::read_handshake(&stream).unwrap();

    // Forge an over-cap frame: a header declaring a payload just past MAX_FRAME_BYTES, then stream that many
    // real bytes (in chunks, so we never allocate the whole thing). The host must drain all of it and NACK.
    let over_len: u32 = MAX_FRAME_BYTES + 4096;
    let header = SubmitHeader { surface_id: 5, width: 0, height: 0, len: over_len };
    {
        let mut s = &stream;
        s.write_all(&header.to_bytes()).unwrap();
        let chunk = vec![0xABu8; 1 << 20]; // 1 MiB
        let mut remaining = over_len as usize;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            s.write_all(&chunk[..n]).unwrap();
            remaining -= n;
        }
    }
    // The over-cap frame is NACKed (ACK_FAIL), not a closed pipe.
    let mut ack = [0u8; 1];
    (&stream).read_exact(&mut ack).expect("host answered the over-cap frame instead of closing");
    assert_eq!(ack[0], ACK_FAIL, "an over-cap frame is NACKed");

    // The SAME connection must still serve a normal small frame — the pipe survived the over-cap frame.
    let good = hl_gpu::encode_stream(&[Cmd::CreateFence(3)]);
    let good_hdr = SubmitHeader { surface_id: 5, width: 0, height: 0, len: good.len() as u32 };
    unix::write_frame(&stream, &good_hdr, &good).unwrap();
    (&stream).read_exact(&mut ack).expect("the same connection served a good frame after the over-cap one");
    assert_eq!(ack[0], ACK_OK, "the good frame ACKs on the connection that survived the over-cap frame");

    drop(stream);
    host.join().unwrap();
}
