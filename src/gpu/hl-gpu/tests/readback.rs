//! Device→host **readback** tests: a guest writes a buffer, then reads its bytes back over the REAL socket
//! transport (a [`RemoteCommandSink`] against a runtime-backed [`serve_connection_with_handler`] host), and
//! we assert the exact bytes. We also assert in-process/remote PARITY: the same submit + readback through a
//! socket-free [`InProcessCommandSink`] returns byte-identical results.
//!
//! The readback request rides a reserved-magic frame disjoint from the submit path, so this exercises the
//! additive path without disturbing the frozen submit framing (see `tests/transport.rs` + `tests/golden.rs`).

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::transport::{SubmitHeader, Verdict};
use hl_gpu::{
    BufferId, Capabilities, Cmd, CommandBuffer, CommandSink, ConnectionHandler, CpuExecutor,
    FakeClock, FenceId, GlobalLedger, GpuExecutor, InProcessCommandSink, Limits, ReadbackRequest,
    RemoteCommandSink, Session,
};

/// A unique temp socket path for one test, removed on drop.
struct TempSock(PathBuf);
impl TempSock {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "hl-readback-{tag}-{}-{:?}.sock",
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

/// A host that owns a runtime `Session` + `CpuExecutor` and serves BOTH submit (through the runtime
/// pipeline) and readback (through the executor's device memory). One `&mut self` drives both halves.
struct RuntimeHost {
    session: Session,
    exec: CpuExecutor,
}

struct PendingFenceHost;

impl ConnectionHandler for PendingFenceHost {
    fn submit(&mut self, _header: &SubmitHeader, _batch: &[Cmd]) -> Verdict {
        Verdict::Ack
    }

    fn poll_fence(&mut self, _req: &ReadbackRequest) -> Option<bool> {
        Some(false)
    }

    fn wait_fence(&mut self, req: &ReadbackRequest) -> Option<hl_gpu::FenceWait> {
        assert_eq!(req.len, 123_456, "wire preserves the requested timeout");
        Some(hl_gpu::FenceWait::Timeout)
    }
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

    fn poll_fence(&mut self, req: &ReadbackRequest) -> Option<bool> {
        hl_gpu::runtime::service::dispatch::poll_fence(
            &self.session,
            &mut self.exec,
            FenceId(req.id),
            req.offset,
        )
        .ok()
    }

    fn wait_fence(&mut self, req: &ReadbackRequest) -> Option<hl_gpu::FenceWait> {
        hl_gpu::runtime::service::dispatch::wait_timeout(
            &mut self.session,
            &mut self.exec,
            FenceId(req.id),
            req.offset,
            req.len,
        )
        .ok()
    }
}

/// Submit CreateBuffer + WriteBuffer for a 8-byte buffer id=1 holding `data`.
fn write_program(data: &[u8]) -> Vec<Cmd> {
    vec![
        Cmd::CreateBuffer(
            1,
            BufferDesc {
                size: data.len() as u64,
                usage: buffer_usage::COPY_DST | buffer_usage::COPY_SRC,
                label: "rb".into(),
            },
        ),
        Cmd::WriteBuffer {
            id: 1,
            offset: 0,
            data: data.to_vec(),
        },
    ]
}

#[test]
fn remote_sink_reads_a_written_buffer_back_over_the_socket() {
    let data = vec![0x11u8, 0x22, 0x33, 0x44, 0xAA, 0xBB, 0xCC, 0xDD];
    let sock = TempSock::new("roundtrip");
    let listener = UnixListener::bind(&sock.0).unwrap();

    // Host: a runtime-backed handler serving one connection (submit + readback) until the client drops.
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    sink.submit(&write_program(&data)).expect("write submitted");
    let got = sink
        .read_buffer(BufferId(1), 0, data.len())
        .expect("readback over socket");
    assert_eq!(
        got, data,
        "socket readback returned the exact written bytes"
    );

    // A partial-window readback returns exactly that slice.
    let mid = sink
        .read_buffer(BufferId(1), 2, 3)
        .expect("partial readback");
    assert_eq!(mid, &data[2..5]);

    drop(sink);
    server.join().unwrap();
}

#[test]
fn remote_fence_poll_reports_real_host_completion() {
    let sock = TempSock::new("fence-poll");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    sink.submit(&[
        Cmd::CreateFence(7),
        Cmd::Submit(CommandBuffer {
            encoder: Vec::new(),
            signal: Some((7, 3)),
        }),
    ])
    .unwrap();
    assert!(sink.poll_fence(FenceId(7), 3).unwrap());
    assert!(!sink.poll_fence(FenceId(7), 4).unwrap());
    assert_eq!(
        sink.wait_timeout(FenceId(7), 3, 1).unwrap(),
        hl_gpu::FenceWait::Complete
    );

    drop(sink);
    server.join().unwrap();
}

#[test]
fn remote_fence_wait_preserves_pending_and_bounded_timeout() {
    let sock = TempSock::new("fence-timeout");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut PendingFenceHost).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    assert!(!sink.poll_fence(FenceId(9), 4).unwrap());
    assert_eq!(
        sink.wait_timeout(FenceId(9), 4, 123_456).unwrap(),
        hl_gpu::FenceWait::Timeout
    );

    drop(sink);
    server.join().unwrap();
}

#[test]
fn in_process_and_remote_readback_are_byte_identical() {
    let data = vec![0xDEu8, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];

    // In-process: socket-free path straight through the runtime + CpuExecutor.
    let mut inproc = InProcessCommandSink::new(CpuExecutor::new());
    inproc.submit(&write_program(&data)).unwrap();
    let inproc_bytes = CommandSink::read_buffer(&mut inproc, BufferId(1), 0, data.len())
        .expect("in-process readback");

    // Remote: identical program + readback over a real socket.
    let sock = TempSock::new("parity");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });
    let mut remote = RemoteCommandSink::new(sock.path());
    remote.submit(&write_program(&data)).unwrap();
    let remote_bytes = remote
        .read_buffer(BufferId(1), 0, data.len())
        .expect("remote readback");
    drop(remote);
    server.join().unwrap();

    assert_eq!(inproc_bytes, data, "in-process readback bytes");
    assert_eq!(remote_bytes, data, "remote readback bytes");
    assert_eq!(
        inproc_bytes, remote_bytes,
        "in-process and remote readback are byte-identical"
    );
}

#[test]
fn readback_of_a_missing_buffer_fails_cleanly() {
    // A readback for a buffer that was never created must surface a typed error, not garbage bytes.
    let sock = TempSock::new("missing");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        let mut host = RuntimeHost::new();
        hl_gpu::serve_connection_with_handler(&stream, &caps, &mut host).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    let err = sink.read_buffer(BufferId(999), 0, 4);
    assert!(err.is_err(), "readback of an unknown buffer must fail");
    drop(sink);
    server.join().unwrap();
}

#[test]
fn submit_only_serve_connection_fails_readback() {
    // The back-compat `serve_connection` (submit-only closure) must still answer a readback request with a
    // clean failure, never hang or corrupt the stream.
    let sock = TempSock::new("submitonly");
    let listener = UnixListener::bind(&sock.0).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let caps = Capabilities::full("host");
        // A submit-only host: acks submits, has no readback half.
        hl_gpu::serve_connection(&stream, &caps, |_h, _b: &[Cmd]| true).unwrap();
    });

    let mut sink = RemoteCommandSink::new(sock.path());
    sink.submit(&[Cmd::CreateFence(1)]).unwrap();
    assert!(
        sink.read_buffer(BufferId(1), 0, 4).is_err(),
        "submit-only host fails readback cleanly"
    );
    drop(sink);
    server.join().unwrap();
}
