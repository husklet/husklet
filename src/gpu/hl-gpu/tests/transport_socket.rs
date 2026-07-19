//! Transport-over-a-REAL-socket battery: a client thread ↔ a server thread running the runtime pipeline
//! (`Session` + `CpuExecutor`) exchange framed submits + length-prefixed readbacks over a genuine
//! `UnixListener`/`UnixStream`, under conditions the in-process decode fuzz (`tests/wire_fuzz.rs`) and the
//! adapter-level `UnixStream::pair` tests (`tests/transport_adversarial.rs`) cannot reproduce: a multi-MiB
//! frame streamed through the OS socket buffers, a valid frame deliberately split across many tiny writes so
//! the server's reader must reassemble it across partial reads, a client that drops mid-frame, several
//! interleaved connections each owning an isolated `Session`, a bogus/huge length prefix arriving on the
//! wire, and a burst of many frames with acks drained concurrently (backpressure). Every scenario runs under
//! a hard watchdog so a transport HANG fails the test instead of wedging the runner.
//!
//! These complement — never duplicate — the existing socket tests: `tests/readback.rs` (single readback
//! round-trip + in-process parity), `tests/transport.rs` (decode equivalence + reconnect/residency), and
//! `tests/transport_robustness.rs` (one large frame + NACK/over-cap keep-alive on a SINGLE connection). The
//! new coverage here is the runtime-backed executor under real-socket stress: byte-exact readback of MANY
//! distinct buffers from a multi-MiB frame, reassembly of a fragmented frame THROUGH the serve loop,
//! graceful teardown + fresh-connection recovery after a mid-frame disconnect, cross-connection isolation,
//! the DoS length cap enforced at the wire read of the serve loop, and no-deadlock under a pipelined burst.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::transport::adapter::unix::{self, MAX_FRAME_BYTES};
use hl_gpu::transport::model::header::{SubmitHeader, ACK_OK};
use hl_gpu::transport::Verdict;
use hl_gpu::{
    serve_connection_with_handler, BufferId, Capabilities, Cmd, CommandSink, ConnectionHandler,
    CpuExecutor, FakeClock, GlobalLedger, GpuExecutor, Limits, ReadbackRequest, RemoteCommandSink,
    Session,
};

// ---------------------------------------------------------------------------------------------------
// harness: unique temp socket, a runtime-backed host, and a hard per-test watchdog
// ---------------------------------------------------------------------------------------------------

/// A unique temp socket path for one test, removed on drop.
struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-tx-socket-{tag}-{}-{:?}.sock",
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

/// A host that owns a runtime `Session` + `CpuExecutor` and serves BOTH the submit path (through the
/// validate→account→dispatch runtime pipeline) and the device→host readback path (through the executor's
/// device memory). One `&mut self` drives both halves — the same wiring the real host uses. Each instance
/// is a fully independent GPU context, so one host per connection = isolated resources per connection.
struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
}

impl RuntimeHost {
    fn new() -> Self {
        let exec = CpuExecutor::new();
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        Self { session, exec }
    }
}

impl ConnectionHandler for RuntimeHost {
    fn submit(&mut self, _header: &SubmitHeader, batch: &[Cmd]) -> Verdict {
        let frame_bytes = hl_gpu::Encoder::stream(batch).len();
        match hl_gpu::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch) {
            Ok(_) => Verdict::Ack,
            Err(_) => Verdict::Nack,
        }
    }

    fn read_buffer(&mut self, req: &ReadbackRequest) -> Option<Vec<u8>> {
        hl_gpu::runtime::service::dispatch::read_buffer(
            &self.session,
            &self.exec,
            BufferId(req.id),
            req.offset,
            req.len as usize,
        )
        .ok()
    }
}

/// Run `body` on a worker thread and fail the test if it does not finish within `secs`. A transport HANG
/// (a reader blocked forever, a deadlocked ack exchange) thus surfaces as a test failure instead of wedging
/// the whole runner. A panic inside `body` is re-raised on the calling thread (the sender drops, we observe
/// the disconnect, and `join` propagates the panic) so real assertion failures are not masked as timeouts.
fn with_watchdog<F: FnOnce() + Send + 'static>(secs: u64, body: F) {
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(()) => worker.join().expect("test body panicked"),
        // The sender dropped without signalling: the body panicked. Join to re-raise that panic.
        Err(RecvTimeoutError::Disconnected) => worker.join().expect("test body panicked"),
        Err(RecvTimeoutError::Timeout) => {
            panic!("transport test exceeded {secs}s watchdog (hang/deadlock)")
        }
    }
}

/// CreateBuffer(id) sized to `data` (COPY_DST|COPY_SRC so a readback can pull it back) + WriteBuffer(data).
fn write_program(id: u32, data: &[u8]) -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(
            id,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                label: "s".into(),
            },
        ),
        Cmd::WriteBuffer {
            id,
            offset: 0,
            data: data.to_vec(),
        },
    ]
}

/// Read + discard the host's `[u32 len][body]` capability handshake off a raw client socket (what
/// `RemoteCommandSink::ensure` does internally; a raw client must consume it before sending any frame).
fn drain_handshake(stream: &UnixStream) {
    let mut s = stream;
    let mut len = [0u8; 4];
    s.read_exact(&mut len).expect("handshake length");
    let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
    s.read_exact(&mut body).expect("handshake body");
}

/// Serve exactly one connection with a fresh runtime host, returning the serve-loop result.
fn serve_one(listener: &UnixListener) -> std::io::Result<()> {
    let (stream, _) = listener.accept().expect("accept");
    let caps = Capabilities::full("host");
    let mut host = RuntimeHost::new();
    serve_connection_with_handler(&stream, &caps, &mut host)
}

// ---------------------------------------------------------------------------------------------------
// 1. large_frame_roundtrip — a multi-MiB, many-buffer frame round-trips and every buffer reads back exact
// ---------------------------------------------------------------------------------------------------

#[test]
fn large_frame_roundtrip_many_buffers_byte_exact() {
    with_watchdog(30, || {
        // 8 buffers × 512 KiB = 4 MiB of uploads in ONE framed submit, each buffer a DISTINCT byte pattern,
        // plus a few thousand no-op creates so the frame is both multi-MiB AND many-command. Every buffer
        // must survive the OS socket buffering intact and read back byte-for-byte off the real CpuExecutor.
        const N_BUF: u32 = 8;
        const BUF_LEN: usize = 512 << 10; // 512 KiB each
        let sock = TempSock::new("large");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        // Distinct content per buffer: byte = (buffer_index * 31 + position) so no two buffers alias.
        let content = |b: u32| -> Vec<u8> {
            (0..BUF_LEN)
                .map(|i| (b as usize * 31 + i * 2654435761usize) as u8)
                .collect()
        };

        let mut batch = Vec::new();
        for b in 1..=N_BUF {
            let data = content(b);
            batch.push(Cmd::CreateBuffer(
                b,
                BufferDesc {
                    size: BUF_LEN as u64,
                    usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ));
            batch.push(Cmd::WriteBuffer {
                id: b,
                offset: 0,
                data,
            });
        }
        for f in 0..3000u32 {
            batch.push(Cmd::CreateFence(100_000 + f));
        }

        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&batch)
            .expect("multi-MiB frame acknowledged over the socket");

        // Read every buffer back over the SAME connection and assert byte-exact — and a partial window too.
        for b in 1..=N_BUF {
            let want = content(b);
            let got = sink
                .read_buffer(BufferId(b), 0, BUF_LEN)
                .expect("readback whole buffer");
            assert_eq!(
                got, want,
                "buffer {b} round-tripped byte-exact through the multi-MiB frame"
            );
            let mid = sink
                .read_buffer(BufferId(b), 1000, 256)
                .expect("partial readback");
            assert_eq!(mid, &want[1000..1256], "buffer {b} partial window is exact");
        }

        drop(sink);
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 2. fragmented_send — a valid frame split across many tiny writes; the serve loop reassembles it
// ---------------------------------------------------------------------------------------------------

#[test]
fn fragmented_frame_reassembles_through_the_serve_loop() {
    with_watchdog(30, || {
        // The client hand-writes a valid submit frame ONE BYTE AT A TIME with a flush + brief pause between
        // bytes, so the server's blocking reader is forced to reassemble the frame across dozens of partial
        // reads (each `read` returns a fragment). The runtime host must execute the SAME program it would
        // from a single write — proven by reading the uploaded buffer back byte-exact afterward.
        let data: Vec<u8> = (0..777u32).map(|i| (i * 7 + 1) as u8).collect();
        let sock = TempSock::new("frag");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        let client = UnixStream::connect(&sock.0).unwrap();
        drain_handshake(&client);

        // Frame the submit ourselves so we control the byte-by-byte trickle.
        let payload = hl_gpu::Encoder::stream(&write_program(1, &data));
        let header = SubmitHeader {
            surface_id: 1,
            width: 0,
            height: 0,
            len: payload.len() as u32,
        };
        let mut wire = header.to_bytes().to_vec();
        wire.extend_from_slice(&payload);

        {
            let mut w = &client;
            for (i, byte) in wire.iter().enumerate() {
                w.write_all(&[*byte]).expect("trickle one byte");
                w.flush().ok();
                // Pause on the first bytes (spanning the header→payload boundary) and periodically after,
                // to force the reader to return short reads rather than coalescing the whole frame.
                if i < 24 || i % 97 == 0 {
                    thread::sleep(Duration::from_micros(200));
                }
            }
        }

        // The host acked the reassembled frame (ACK_OK), proving the fragmented submit decoded correctly.
        let mut ack = [0u8; 1];
        (&client)
            .read_exact(&mut ack)
            .expect("host acked the reassembled fragmented frame");
        assert_eq!(
            ack[0], ACK_OK,
            "a frame split across single-byte writes must reassemble + ACK"
        );

        // Read the uploaded buffer back: byte-exact == the fragmented submit was reassembled correctly.
        unix::Connection::new(&client)
            .write_readback_request(&ReadbackRequest::buffer(1, 0, data.len() as u64))
            .unwrap();
        let got = unix::Connection::new(&client)
            .read_readback_response()
            .expect("readback after fragmented submit");
        assert_eq!(
            got, data,
            "the fragmented frame produced the same result as a single write"
        );

        drop(client);
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 3. disconnect_mid_frame — a partial frame then a dropped connection; graceful, and the next connection works
// ---------------------------------------------------------------------------------------------------

#[test]
fn disconnect_mid_frame_is_graceful_and_a_fresh_connection_works() {
    with_watchdog(30, || {
        // Connection #1: the client sends a header promising a payload, then only HALF the payload, then
        // drops. The serve loop must surface a clean typed error (truncated payload / UnexpectedEof) — no
        // hang, no panic, no corruption. Connection #2 (a fresh accept + fresh host) must then serve a real
        // submit + readback perfectly, proving the mid-frame disconnect did not wedge the listener.
        let sock = TempSock::new("disc");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            // #1: expect a transport error from the truncated frame (peer vanished mid-payload).
            let r1 = serve_one(&listener);
            // #2: a fresh connection must serve cleanly to a clean EOF.
            let r2 = serve_one(&listener);
            (r1.is_err(), r2)
        });

        // #1: connect, consume handshake, promise 64 payload bytes but send only 20, then drop.
        {
            let client = UnixStream::connect(&sock.0).unwrap();
            drain_handshake(&client);
            let header = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: 64,
            };
            let mut w = &client;
            w.write_all(&header.to_bytes()).unwrap();
            w.write_all(&[0xAB; 20]).unwrap(); // 20 of the promised 64 -> truncated
            w.flush().ok();
            drop(client); // vanish mid-frame
        }

        // #2: a fresh, well-behaved connection round-trips a submit + readback.
        let data = vec![0x5Au8; 32];
        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&write_program(2, &data))
            .expect("fresh connection submits after the disconnect");
        let got = sink
            .read_buffer(BufferId(2), 0, data.len())
            .expect("fresh connection readback works");
        assert_eq!(
            got, data,
            "the connection AFTER a mid-frame disconnect is fully functional"
        );
        drop(sink);

        let (first_errored, second) = server.join().unwrap();
        assert!(
            first_errored,
            "a mid-frame disconnect surfaces a typed transport error, not a panic/hang"
        );
        assert!(
            second.is_ok(),
            "the serve loop recovered: the next connection served to a clean EOF"
        );
    });
}

// ---------------------------------------------------------------------------------------------------
// 4. concurrent_connections — several interleaved connections, each an isolated Session, no cross-bleed
// ---------------------------------------------------------------------------------------------------

#[test]
fn concurrent_connections_have_isolated_sessions() {
    with_watchdog(30, || {
        // N clients connect at once. Each writes buffer id=1 (the SAME id on every connection) with a value
        // unique to that client, then reads id=1 back. If any resources bled across connections, a reader
        // would see another client's bytes; isolation means each reads back EXACTLY its own value. The
        // server spawns one RuntimeHost (a fresh Session + CpuExecutor) per accepted connection.
        const N: usize = 8;
        const LEN: usize = 4096;
        let sock = TempSock::new("concurrent");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            let caps = Capabilities::full("host");
            let mut handles = Vec::new();
            for stream in listener.incoming().take(N) {
                let stream = stream.unwrap();
                let caps = caps.clone();
                handles.push(thread::spawn(move || {
                    let mut host = RuntimeHost::new();
                    // A per-connection error must not abort the others; each thread owns its result.
                    let _ = serve_connection_with_handler(&stream, &caps, &mut host);
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
        });

        let path = sock.path();
        let mut clients = Vec::new();
        for k in 0..N {
            let path = path.clone();
            clients.push(thread::spawn(move || {
                let value = (0x40 + k) as u8; // a byte value unique to this connection
                let data = vec![value; LEN];
                let mut sink = RemoteCommandSink::new(path);
                sink.submit(&write_program(1, &data))
                    .expect("per-connection submit");
                let got = sink
                    .read_buffer(BufferId(1), 0, LEN)
                    .expect("per-connection readback");
                // Every byte must be THIS connection's value — never another connection's.
                assert!(
                    got.iter().all(|&b| b == value),
                    "connection {k} saw foreign bytes — cross-connection resource bleed"
                );
                assert_eq!(got.len(), LEN);
            }));
        }
        for c in clients {
            c.join()
                .expect("a concurrent client thread panicked (isolation broken)");
        }
        server.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------------
// 5. malformed_length_prefix — a bogus/huge wire length is capped at the read, no OOM, connection recovers
// ---------------------------------------------------------------------------------------------------

#[test]
fn malformed_length_prefix_is_capped_without_oom_and_recovers() {
    with_watchdog(30, || {
        // A hostile client stamps a header with a 4 GiB (`u32::MAX`) declared payload — far past the
        // MAX_FRAME_BYTES DoS cap — then drops WITHOUT sending the promised body. The serve loop must refuse
        // to preallocate that buffer (the cap applies at the WIRE read), drain to resync, hit EOF, and
        // return a typed error — all in well under the watchdog and without exhausting memory. A fresh
        // connection afterward must still work. This is the DoS cap enforced at the serve loop, not just in
        // the adapter unit test.
        assert!(
            u32::MAX > MAX_FRAME_BYTES,
            "the forged length must exceed the transport cap"
        );
        let sock = TempSock::new("malformed");
        let listener = UnixListener::bind(&sock.0).unwrap();

        let server = thread::spawn(move || {
            let r1 = serve_one(&listener); // the malformed-prefix connection
            let r2 = serve_one(&listener); // a fresh, well-behaved connection
            (r1.is_err(), r2)
        });

        // #1: forge the absurd length prefix, send NO body, drop.
        {
            let client = UnixStream::connect(&sock.0).unwrap();
            drain_handshake(&client);
            let header = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: u32::MAX,
            };
            let mut w = &client;
            w.write_all(&header.to_bytes()).unwrap();
            w.flush().ok();
            drop(client); // never send the 4 GiB it promised
        }

        // #2: a normal submit + readback proves the listener survived the malformed prefix.
        let data = vec![0xC3u8; 64];
        let mut sink = RemoteCommandSink::new(sock.path());
        sink.submit(&write_program(3, &data))
            .expect("fresh connection works after a malformed prefix");
        let got = sink
            .read_buffer(BufferId(3), 0, data.len())
            .expect("readback after malformed prefix");
        assert_eq!(
            got, data,
            "the connection after a malformed length prefix is fully functional"
        );
        drop(sink);

        let (first_errored, second) = server.join().unwrap();
        assert!(
            first_errored,
            "an over-cap wire length is refused (drained to EOF) as a typed error, not OOM"
        );
        assert!(
            second.is_ok(),
            "the serve loop recovered: the next connection served cleanly"
        );
    });
}

// ---------------------------------------------------------------------------------------------------
// 6. backpressure — a pipelined burst of many frames with acks drained concurrently: no deadlock
// ---------------------------------------------------------------------------------------------------

#[test]
fn pipelined_burst_does_not_deadlock_under_backpressure() {
    with_watchdog(45, || {
        // A raw client pipelines MANY frames back-to-back (a writer thread) while a separate reader thread
        // drains the acks, so more than a socket-buffer's worth of frame bytes is in flight at once and the
        // writer genuinely blocks waiting on the server to drain — real backpressure. The lockstep
        // per-frame ack must NOT deadlock: every frame is eventually processed in order, proven by a final
        // readback of the whole buffer equalling the concatenation of every chunk written.
        const FRAMES: usize = 200;
        const CHUNK: usize = 4096; // 200 × 4 KiB ≈ 800 KiB in flight, past the socket buffer
        let total = FRAMES * CHUNK;
        let sock = TempSock::new("backpressure");
        let listener = UnixListener::bind(&sock.0).unwrap();
        let server = thread::spawn(move || serve_one(&listener).unwrap());

        let client = UnixStream::connect(&sock.0).unwrap();
        drain_handshake(&client);

        // Frame 0: create the destination buffer. Wait for its ack before pipelining writes into it.
        {
            let create = hl_gpu::Encoder::stream(&[Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: total as u64,
                    usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            )]);
            let hdr = SubmitHeader {
                surface_id: 1,
                width: 0,
                height: 0,
                len: create.len() as u32,
            };
            unix::Connection::new(&client)
                .write_frame(&hdr, &create)
                .unwrap();
            let mut ack = [0u8; 1];
            (&client).read_exact(&mut ack).unwrap();
            assert_eq!(ack[0], ACK_OK, "buffer creation acked");
        }

        // Concurrent ack drainer: reads exactly FRAMES acks so the writer can never wedge on a full buffer.
        let ack_reader = {
            let client = client.try_clone().unwrap();
            thread::spawn(move || {
                let mut s = &client;
                let mut oks = 0usize;
                for _ in 0..FRAMES {
                    let mut ack = [0u8; 1];
                    s.read_exact(&mut ack).expect("ack for a pipelined frame");
                    if ack[0] == ACK_OK {
                        oks += 1;
                    }
                }
                oks
            })
        };

        // Writer: pipeline FRAMES WriteBuffer frames back-to-back WITHOUT waiting per-ack. Each frame writes
        // its chunk to a distinct offset with a value derived from the frame index, so the assembled buffer
        // is a known pattern. Backpressure engages when the in-flight bytes exceed the OS socket buffer.
        {
            let mut w = &client;
            for f in 0..FRAMES {
                let chunk = vec![(f as u8).wrapping_mul(3).wrapping_add(1); CHUNK];
                let batch = [Cmd::WriteBuffer {
                    id: 1,
                    offset: (f * CHUNK) as u64,
                    data: chunk,
                }];
                let payload = hl_gpu::Encoder::stream(&batch);
                let hdr = SubmitHeader {
                    surface_id: 1,
                    width: 0,
                    height: 0,
                    len: payload.len() as u32,
                };
                unix::Connection::new(w)
                    .write_frame(&hdr, &payload)
                    .expect("pipelined frame write");
                let _ = w.flush();
            }
        }

        let oks = ack_reader.join().expect("ack drainer thread");
        assert_eq!(
            oks, FRAMES,
            "every pipelined frame was processed and acked — no deadlock, no drop"
        );

        // Final readback of the WHOLE buffer proves ordered, complete processing of the burst.
        unix::Connection::new(&client)
            .write_readback_request(&ReadbackRequest::buffer(1, 0, total as u64))
            .unwrap();
        let got = unix::Connection::new(&client)
            .read_readback_response()
            .expect("full-buffer readback after the burst");
        assert_eq!(
            got.len(),
            total,
            "the whole buffer was written by the burst"
        );
        for f in 0..FRAMES {
            let want = (f as u8).wrapping_mul(3).wrapping_add(1);
            assert!(
                got[f * CHUNK..(f + 1) * CHUNK].iter().all(|&b| b == want),
                "frame {f}'s chunk landed intact and in order under backpressure"
            );
        }

        drop(client);
        server.join().unwrap();
    });
}
