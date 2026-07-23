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

#[path = "transport_socket/framing.rs"]
mod framing;
#[path = "transport_socket/resilience.rs"]
mod resilience;
#[path = "transport_socket/sessions.rs"]
mod sessions;
